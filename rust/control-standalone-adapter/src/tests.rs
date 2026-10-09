use super::*;
use std::{
    ffi::CString,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

struct Payload(Option<Box<dyn FnOnce() + Send>>);

unsafe extern "C" fn run_payload(context: *mut c_void) {
    // SAFETY: each accepted task is invoked once before its release callback.
    let payload = unsafe { &mut *context.cast::<Payload>() };
    payload.0.take().unwrap()();
}

unsafe extern "C" fn release_payload(context: *mut c_void) {
    // SAFETY: accepted tasks call release once; rejected tasks remain caller-owned.
    unsafe { drop(Box::from_raw(context.cast::<Payload>())) };
}

fn task(operation: impl FnOnce() + Send + 'static) -> Task {
    Task {
        context: Box::into_raw(Box::new(Payload(Some(Box::new(operation))))).cast(),
        run: Some(run_payload),
        release: Some(release_payload),
    }
}

fn open(capacity: usize) -> StandaloneExecutor {
    StandaloneExecutor::open("test", capacity).expect("executor opens")
}

#[test]
fn capacity_does_not_overlap_stop_bit() {
    assert!(valid_capacity(1));
    assert!(!valid_capacity(0));
    assert!(valid_capacity(MAX_CAPACITY));
    assert!(!valid_capacity(MAX_CAPACITY + 1));
    assert!(!valid_capacity(STOPPED));
}

#[test]
fn open_rejects_invalid_configuration() {
    assert!(matches!(
        StandaloneExecutor::open("", 1),
        Err(OpenError::InvalidConfiguration)
    ));
    assert!(matches!(
        StandaloneExecutor::open("test", 0),
        Err(OpenError::InvalidConfiguration)
    ));
}

#[test]
fn worker_waits_for_a_reserved_submission_after_stop() {
    let shared = Arc::new(Shared {
        queue: ArrayQueue::new(1),
        state: std::sync::atomic::AtomicUsize::new(STOPPED | 1),
        capacity: 1,
    });
    let worker_shared = Arc::clone(&shared);
    let worker = thread::spawn(move || worker_loop(worker_shared));
    thread::sleep(Duration::from_millis(10));
    shared.state.store(STOPPED, Ordering::Release);
    worker.thread().unpark();
    worker.join().unwrap();
}

#[test]
fn executor_drop_on_worker_tolerates_a_detached_join_handle() {
    let executor = Arc::new(open(1));
    let weak_shared = Arc::downgrade(&executor.shared);
    let handle = executor.join.swap(ptr::null_mut(), Ordering::AcqRel);
    assert!(!handle.is_null());
    unsafe { drop(Box::from_raw(handle)) };
    let task_executor = Arc::clone(&executor);
    let (finished_send, finished_receive) = mpsc::channel();
    assert!(
        executor
            .submit(task(move || {
                drop(task_executor);
                finished_send.send(()).unwrap();
            }))
            .is_ok()
    );
    drop(executor);
    finished_receive
        .recv_timeout(Duration::from_secs(2))
        .expect("last executor owner drops from its worker");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while weak_shared.upgrade().is_some() && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(weak_shared.upgrade().is_none());
}

#[test]
fn submit_runs_once_on_owner() {
    let executor = open(2);
    let caller = thread::current().id();
    let (send, receive) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let task_calls = Arc::clone(&calls);
    assert!(
        executor
            .submit(task(move || {
                let previous = task_calls.fetch_add(1, Ordering::Relaxed);
                send.send((thread::current().id(), previous)).unwrap();
            }))
            .is_ok()
    );

    let (owner, previous) = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("task executes");
    assert_ne!(owner, caller);
    assert_eq!(previous, 0);
    executor.stop_and_drain().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn full_queue_returns_unrun_task() {
    let executor = open(2);
    let (started_send, started_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    assert!(
        executor
            .submit(task(move || {
                started_send.send(()).unwrap();
                release_receive.recv().unwrap();
            }))
            .is_ok()
    );
    started_receive
        .recv_timeout(Duration::from_secs(2))
        .expect("first task is running");

    assert!(executor.submit(task(|| {})).is_ok());
    let ran = Arc::new(AtomicBool::new(false));
    let rejected_ran = Arc::clone(&ran);
    let (rejected, reason) = executor
        .submit(task(move || {
            rejected_ran.store(true, Ordering::Relaxed);
        }))
        .expect_err("bounded queue rejects excess work");
    assert_eq!(reason, RejectionReason::Full);
    assert!(!ran.load(Ordering::Relaxed));
    unsafe { release_payload(rejected.context) };

    release_send.send(()).unwrap();
    executor.stop_and_drain().unwrap();
    assert!(!ran.load(Ordering::Relaxed));
}

#[test]
fn queue_insert_failure_returns_task_to_caller() {
    let shared = Arc::new(Shared {
        queue: ArrayQueue::new(1),
        state: std::sync::atomic::AtomicUsize::new(1),
        capacity: 2,
    });
    let queued = task(|| {});
    assert!(
        shared
            .queue
            .push(QueuedTask {
                context: queued.context,
                run: queued.run.unwrap(),
                release: queued.release.unwrap(),
            })
            .is_ok()
    );
    let executor = StandaloneExecutor {
        shared: Arc::clone(&shared),
        worker: thread::current(),
        join: AtomicPtr::new(ptr::null_mut()),
        joining: AtomicBool::new(false),
        joined: AtomicBool::new(true),
    };
    let ran = Arc::new(AtomicBool::new(false));
    let rejected_ran = Arc::clone(&ran);
    let (rejected, reason) = executor
        .submit(task(move || rejected_ran.store(true, Ordering::Relaxed)))
        .expect_err("an unexpectedly full backend queue rejects without consuming the task");
    assert_eq!(reason, RejectionReason::Full);
    assert!(!ran.load(Ordering::Relaxed));
    unsafe { release_payload(rejected.context) };
    let queued = shared.queue.pop().unwrap();
    unsafe { (queued.release)(queued.context) };
    shared.state.fetch_sub(1, Ordering::Release);
    executor.stop_and_drain().unwrap();
}

#[test]
fn stop_rejects_new_tasks_and_drains() {
    let executor = open(2);
    let calls = Arc::new(AtomicUsize::new(0));
    let task_calls = Arc::clone(&calls);
    assert!(
        executor
            .submit(task(move || {
                task_calls.fetch_add(1, Ordering::Relaxed);
            }))
            .is_ok()
    );

    executor.stop_and_drain().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let ran = Arc::new(AtomicBool::new(false));
    let rejected_ran = Arc::clone(&ran);
    let (rejected, reason) = executor
        .submit(task(move || {
            rejected_ran.store(true, Ordering::Relaxed);
        }))
        .expect_err("stopped executor rejects work");
    assert_eq!(reason, RejectionReason::Stopped);
    assert!(!ran.load(Ordering::Relaxed));
    unsafe { release_payload(rejected.context) };
}

#[test]
fn drain_from_owner_is_rejected() {
    let executor = Arc::new(open(2));
    let task_executor = Arc::clone(&executor);
    let (send, receive) = mpsc::channel();
    assert!(
        executor
            .submit(task(move || {
                send.send(task_executor.stop_and_drain()).unwrap();
            }))
            .is_ok()
    );

    assert_eq!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(DrainError::OnExecutor)
    );
    executor.stop_and_drain().unwrap();
}

#[test]
fn owner_can_drain_a_different_executor() {
    let first = open(1);
    let second = Arc::new(open(1));
    let task_second = Arc::clone(&second);
    let (send, receive) = mpsc::channel();
    assert!(
        first
            .submit(task(move || {
                send.send(task_second.stop_and_drain()).unwrap();
            }))
            .is_ok()
    );

    assert_eq!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        Ok(())
    );
    first.stop_and_drain().unwrap();
}

#[test]
fn descriptor_opens_and_closes_provider() {
    let descriptor = unsafe { &*rptadv_control_standalone_descriptor_v1() };
    assert_eq!(descriptor.abi_version, 1);
    assert_eq!(descriptor.struct_size, std::mem::size_of::<Descriptor>());
    assert_eq!(
        unsafe { CStr::from_ptr(descriptor.capability) },
        c"rptadv.control"
    );
    let name = CString::new("test").unwrap();
    let context = unsafe { descriptor.open.unwrap()(name.as_ptr(), 1) };
    assert!(!context.is_null());
    assert_eq!(unsafe { descriptor.close.unwrap()(context) }, 0);
}

#[test]
fn abi_rejects_invalid_context_names_tasks_and_closed_submissions() {
    assert!(unsafe { open_abi(ptr::null(), 1) }.is_null());
    let invalid_utf8 = [0xff_u8, 0];
    assert!(unsafe { open_abi(invalid_utf8.as_ptr().cast(), 1) }.is_null());
    assert!(unsafe { open_abi(c"test".as_ptr(), 0) }.is_null());
    assert_eq!(unsafe { drain_abi(ptr::null_mut()) }, -1);
    assert_eq!(unsafe { close_abi(ptr::null_mut()) }, -1);

    let name = CString::new("test").unwrap();
    let context = unsafe { open_abi(name.as_ptr(), 2) };
    assert!(!context.is_null());

    let rejected = task(|| {});
    let rejected_context = rejected.context;
    assert_eq!(unsafe { submit_abi(ptr::null_mut(), rejected) }, 3);
    unsafe { release_payload(rejected_context) };

    assert_eq!(
        unsafe {
            submit_abi(
                context,
                Task {
                    context: ptr::null_mut(),
                    run: None,
                    release: None,
                },
            )
        },
        3
    );
    assert_eq!(unsafe { drain_abi(context) }, 0);
    let rejected = task(|| {});
    let rejected_context = rejected.context;
    assert_eq!(unsafe { submit_abi(context, rejected) }, 1);
    unsafe { release_payload(rejected_context) };
    assert_eq!(unsafe { close_abi(context) }, 0);
}

#[test]
fn callback_cannot_close_its_own_executor_but_owner_can_close_afterward() {
    let name = CString::new("test").unwrap();
    let context = unsafe { open_abi(name.as_ptr(), 1) };
    assert!(!context.is_null());
    let context_address = context as usize;
    let (send, receive) = mpsc::channel();
    assert_eq!(
        unsafe {
            submit_abi(
                context,
                task(move || {
                    let context = context_address as *mut c_void;
                    send.send((drain_abi(context), close_abi(context))).unwrap();
                }),
            )
        },
        0
    );
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        (-1, -1)
    );
    assert_eq!(unsafe { close_abi(context) }, 0);
}

#[test]
fn simultaneous_external_stops_join_the_worker_once_after_draining() {
    let executor = Arc::new(open(32));
    let completed = Arc::new(AtomicUsize::new(0));
    for _ in 0..32 {
        let completed = Arc::clone(&completed);
        assert!(
            executor
                .submit(task(move || {
                    completed.fetch_add(1, Ordering::Relaxed);
                }))
                .is_ok()
        );
    }
    let stops = (0..2)
        .map(|_| {
            let executor = Arc::clone(&executor);
            thread::spawn(move || executor.stop_and_drain())
        })
        .collect::<Vec<_>>();
    for stop in stops {
        assert_eq!(stop.join().unwrap(), Ok(()));
    }
    assert_eq!(completed.load(Ordering::Relaxed), 32);
}

#[test]
fn dropping_last_executor_owner_on_worker_detaches_without_deadlock() {
    let executor = Arc::new(open(1));
    let weak = Arc::downgrade(&executor);
    let task_executor = Arc::clone(&executor);
    let (started_send, started_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let (finished_send, finished_receive) = mpsc::channel();
    assert!(
        executor
            .submit(task(move || {
                started_send.send(()).unwrap();
                release_receive.recv().unwrap();
                finished_send.send(()).unwrap();
                drop(task_executor);
            }))
            .is_ok()
    );
    started_receive
        .recv_timeout(Duration::from_secs(2))
        .expect("worker started task");
    drop(executor);
    release_send.send(()).unwrap();
    finished_receive
        .recv_timeout(Duration::from_secs(2))
        .expect("task finished before its executor owner is dropped");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while weak.upgrade().is_some() && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(weak.upgrade().is_none());
}

#[test]
fn concurrent_producers_keep_every_accepted_task_within_the_reserved_bound() {
    const PRODUCERS: usize = 16;
    const TASKS_PER_PRODUCER: usize = 64;
    let executor = Arc::new(open(PRODUCERS * TASKS_PER_PRODUCER));
    let completed = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(std::sync::Barrier::new(PRODUCERS));
    let producers = (0..PRODUCERS)
        .map(|_| {
            let executor = Arc::clone(&executor);
            let completed = Arc::clone(&completed);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                for _ in 0..TASKS_PER_PRODUCER {
                    let completed = Arc::clone(&completed);
                    assert!(
                        executor
                            .submit(task(move || {
                                completed.fetch_add(1, Ordering::Relaxed);
                            }))
                            .is_ok()
                    );
                }
            })
        })
        .collect::<Vec<_>>();
    for producer in producers {
        producer.join().unwrap();
    }
    executor.stop_and_drain().unwrap();
    assert_eq!(
        completed.load(Ordering::Relaxed),
        PRODUCERS * TASKS_PER_PRODUCER
    );
}

#[test]
fn ffi_reports_a_full_bounded_queue_and_preserves_rejected_task_ownership() {
    let name = CString::new("test").unwrap();
    let context = unsafe { open_abi(name.as_ptr(), 2) };
    assert!(!context.is_null());
    let (started_send, started_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    assert_eq!(
        unsafe {
            submit_abi(
                context,
                task(move || {
                    started_send.send(()).unwrap();
                    release_receive.recv().unwrap();
                }),
            )
        },
        0
    );
    started_receive
        .recv_timeout(Duration::from_secs(2))
        .expect("first task is running");
    assert_eq!(unsafe { submit_abi(context, task(|| {})) }, 0);
    let rejected = task(|| {});
    let rejected_context = rejected.context;
    assert_eq!(unsafe { submit_abi(context, rejected) }, 2);
    unsafe { release_payload(rejected_context) };
    release_send.send(()).unwrap();
    assert_eq!(unsafe { close_abi(context) }, 0);
}

#[test]
fn concurrent_external_stops_share_the_single_worker_join() {
    for _ in 0..8 {
        const STOPPERS: usize = 8;
        let executor = Arc::new(open(8));
        let (started_send, started_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        assert!(
            executor
                .submit(task(move || {
                    started_send.send(()).unwrap();
                    release_receive.recv().unwrap();
                }))
                .is_ok()
        );
        started_receive
            .recv_timeout(Duration::from_secs(2))
            .expect("worker task is running");
        for _ in 1..STOPPERS {
            assert!(executor.submit(task(|| {})).is_ok());
        }
        let start = Arc::new(std::sync::Barrier::new(STOPPERS + 1));
        let stoppers = (0..STOPPERS)
            .map(|_| {
                let executor = Arc::clone(&executor);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    executor.stop_and_drain()
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        release_send.send(()).unwrap();
        for stopper in stoppers {
            assert_eq!(stopper.join().unwrap(), Ok(()));
        }
    }
}
