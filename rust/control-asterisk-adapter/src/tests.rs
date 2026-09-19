use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Payload {
    value: usize,
    output: Arc<Mutex<Vec<usize>>>,
    releases: Arc<AtomicUsize>,
}

unsafe extern "C" fn run_payload(context: *mut c_void) {
    let payload = unsafe { &*context.cast::<Payload>() };
    payload.output.lock().unwrap().push(payload.value);
}

unsafe extern "C" fn release_payload(context: *mut c_void) {
    let payload = unsafe { Box::from_raw(context.cast::<Payload>()) };
    payload.releases.fetch_add(1, Ordering::Relaxed);
}

fn task(value: usize, output: &Arc<Mutex<Vec<usize>>>, releases: &Arc<AtomicUsize>) -> Task {
    Task {
        context: Box::into_raw(Box::new(Payload {
            value,
            output: Arc::clone(output),
            releases: Arc::clone(releases),
        }))
        .cast(),
        run: run_payload,
        release: release_payload,
    }
}

unsafe fn discard(task: Task) {
    unsafe { (task.release)(task.context) };
}

#[test]
fn accepted_fifo_rejected_ownership_and_stop_drain() {
    let executor = AsteriskExecutor::open("rptadv-task9-test", 2).unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(AtomicUsize::new(0));
    assert!(executor.submit(task(1, &output, &releases)).is_ok());
    assert!(executor.submit(task(2, &output, &releases)).is_ok());
    let (rejected, reason) = executor.submit(task(3, &output, &releases)).unwrap_err();
    assert_eq!(reason, SubmitError::Full);
    assert!(output.lock().unwrap().is_empty());
    fixture::execute_all(executor.handle);
    assert_eq!(*output.lock().unwrap(), [1, 2]);
    assert!(executor.submit(rejected).is_ok());
    fixture::execute_all(executor.handle);
    assert!(executor.stop_and_drain());
    assert_eq!(*output.lock().unwrap(), [1, 2, 3]);
    assert_eq!(releases.load(Ordering::Relaxed), 3);
    let (rejected, reason) = executor.submit(task(4, &output, &releases)).unwrap_err();
    assert_eq!(reason, SubmitError::Stopped);
    unsafe { discard(rejected) };
}

#[test]
fn backend_rejection_does_not_consume_the_task() {
    let executor = AsteriskExecutor::open("rptadv-task9-reject", 2).unwrap();
    fixture::reject(executor.handle);
    let output = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(AtomicUsize::new(0));
    let (rejected, reason) = executor.submit(task(9, &output, &releases)).unwrap_err();
    assert_eq!(reason, SubmitError::Backend);
    assert!(output.lock().unwrap().is_empty());
    unsafe {
        (rejected.run)(rejected.context);
        discard(rejected);
    }
    assert_eq!(*output.lock().unwrap(), [9]);
    assert_eq!(releases.load(Ordering::Relaxed), 1);
    assert!(executor.stop_and_drain());
}

#[test]
fn poisoned_admission_state_recovers_submit_execute_and_drain() {
    let executor = AsteriskExecutor::open("poison-recovery", 1).unwrap();
    let state = Arc::clone(&executor.state);
    assert!(
        std::panic::catch_unwind(|| {
            let _guard = state.admission.lock().unwrap();
            panic!("poison admission for recovery coverage");
        })
        .is_err()
    );

    let output = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(AtomicUsize::new(0));
    assert!(executor.submit(task(1, &output, &releases)).is_ok());
    std::thread::scope(|scope| {
        let drain = scope.spawn(|| executor.stop_and_drain());
        while state
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .accepting
        {
            std::thread::yield_now();
        }
        fixture::execute_all(executor.handle);
        assert!(drain.join().unwrap());
    });
    assert_eq!(*output.lock().unwrap(), [1]);
    assert_eq!(releases.load(Ordering::Relaxed), 1);
}

#[test]
fn dropping_from_own_executor_retains_backend_for_external_cleanup() {
    unsafe extern "C" fn drop_executor(context: *mut c_void) {
        drop(unsafe { Box::from_raw(context.cast::<AsteriskExecutor>()) });
    }
    unsafe extern "C" fn release_nothing(_context: *mut c_void) {}

    let executor = Box::new(AsteriskExecutor::open("self-drop", 1).unwrap());
    let handle = executor.handle;
    let context = Box::into_raw(executor);
    assert!(
        unsafe { &*context }
            .submit(Task {
                context: context.cast(),
                run: drop_executor,
                release: release_nothing,
            })
            .is_ok()
    );
    fixture::execute_all(handle);
    fixture::cleanup(handle);
}

#[test]
fn concurrent_submitters_preserve_each_producers_fifo_order() {
    let executor = Arc::new(AsteriskExecutor::open("rptadv-task9-concurrent", 400).unwrap());
    let output = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for producer in 0..4 {
            let executor = Arc::clone(&executor);
            let output = Arc::clone(&output);
            let releases = Arc::clone(&releases);
            scope.spawn(move || {
                for sequence in 0..100 {
                    assert!(
                        executor
                            .submit(task(producer * 100 + sequence, &output, &releases))
                            .is_ok()
                    );
                }
            });
        }
    });
    fixture::execute_all(executor.handle);
    let output = output.lock().unwrap();
    assert_eq!(output.len(), 400);
    for producer in 0..4 {
        assert_eq!(
            output
                .iter()
                .filter(|value| **value / 100 == producer)
                .map(|value| *value % 100)
                .collect::<Vec<_>>(),
            (0..100).collect::<Vec<_>>()
        );
    }
}

#[test]
fn descriptor_validates_inputs_and_releases_accepted_payload_once() {
    let descriptor = unsafe { &*rptadv_control_descriptor_v1() };
    assert_eq!(
        (descriptor.abi_version, descriptor.struct_size),
        (1, size_of::<Descriptor>())
    );
    assert!(unsafe { (descriptor.open)(ptr::null(), 1) }.is_null());
    assert!(unsafe { (descriptor.open)(c"valid".as_ptr(), 0) }.is_null());
    let invalid_utf8 = CString::new([0xff]).unwrap();
    assert!(unsafe { (descriptor.open)(invalid_utf8.as_ptr(), 1) }.is_null());
    let context = unsafe { (descriptor.open)(c"rptadv-task9-abi".as_ptr(), 1) };
    assert!(!context.is_null());
    let output = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        unsafe { (descriptor.submit)(context, task(1, &output, &releases)) },
        0
    );
    let rejected = task(2, &output, &releases);
    assert_eq!(unsafe { (descriptor.submit)(context, rejected) }, 2);
    assert_eq!(releases.load(Ordering::Relaxed), 0);
    unsafe { discard(rejected) };
    fixture::execute_all(unsafe { &*context.cast::<AsteriskExecutor>() }.handle);
    assert_eq!(unsafe { (descriptor.close)(context) }, 0);
    assert_eq!(*output.lock().unwrap(), [1]);
    assert_eq!(releases.load(Ordering::Relaxed), 2);
    assert_eq!(unsafe { (descriptor.stop_and_drain)(ptr::null_mut()) }, -1);
    assert_eq!(unsafe { (descriptor.close)(ptr::null_mut()) }, -1);
    let rejected = task(4, &output, &releases);
    assert_eq!(unsafe { (descriptor.submit)(ptr::null_mut(), rejected) }, 3);
    unsafe { discard(rejected) };
}

#[test]
fn open_rejects_invalid_names_and_backend_failure() {
    assert!(AsteriskExecutor::open("", 1).is_err());
    assert!(AsteriskExecutor::open("embedded\0nul", 1).is_err());
    assert!(AsteriskExecutor::open("reject-open", 1).is_err());
}

#[test]
fn descriptor_reports_stopped_and_backend_rejections() {
    let descriptor = unsafe { &*rptadv_control_descriptor_v1() };
    let output = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(AtomicUsize::new(0));

    let stopped = unsafe { (descriptor.open)(c"stopped-status".as_ptr(), 1) };
    assert!(!stopped.is_null());
    assert_eq!(unsafe { (descriptor.stop_and_drain)(stopped) }, 0);
    let rejected = task(1, &output, &releases);
    assert_eq!(unsafe { (descriptor.submit)(stopped, rejected) }, 1);
    unsafe { discard(rejected) };
    assert_eq!(unsafe { (descriptor.close)(stopped) }, 0);

    let backend = unsafe { (descriptor.open)(c"backend-status".as_ptr(), 1) };
    assert!(!backend.is_null());
    fixture::reject(unsafe { &*backend.cast::<AsteriskExecutor>() }.handle);
    let rejected = task(2, &output, &releases);
    assert_eq!(unsafe { (descriptor.submit)(backend, rejected) }, 3);
    unsafe { discard(rejected) };
    assert_eq!(unsafe { (descriptor.close)(backend) }, 0);
    assert_eq!(releases.load(Ordering::Relaxed), 2);
}

#[test]
fn self_drain_and_close_retain_handle_for_external_shutdown() {
    struct Check {
        executor: *mut c_void,
        results: Arc<Mutex<Vec<c_int>>>,
    }
    unsafe extern "C" fn run(context: *mut c_void) {
        let check = unsafe { &*context.cast::<Check>() };
        check
            .results
            .lock()
            .unwrap()
            .extend([unsafe { drain_abi(check.executor) }, unsafe {
                close_abi(check.executor)
            }]);
    }
    unsafe extern "C" fn release(context: *mut c_void) {
        drop(unsafe { Box::from_raw(context.cast::<Check>()) });
    }
    let context = unsafe { open_abi(c"self-close".as_ptr(), 1) };
    let results = Arc::new(Mutex::new(Vec::new()));
    let payload = Box::into_raw(Box::new(Check {
        executor: context,
        results: results.clone(),
    }))
    .cast();
    assert_eq!(
        unsafe {
            submit_abi(
                context,
                Task {
                    context: payload,
                    run,
                    release,
                },
            )
        },
        0
    );
    fixture::execute_all(unsafe { &*context.cast::<AsteriskExecutor>() }.handle);
    assert_eq!(*results.lock().unwrap(), [-1, -1]);
    assert_eq!(unsafe { drain_abi(context) }, 0);
    assert_eq!(unsafe { close_abi(context) }, 0);
}

#[test]
fn drain_waits_for_running_payload_release_and_gates_new_submissions() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    struct Blocking {
        entered: mpsc::Sender<()>,
        finish: mpsc::Receiver<()>,
        released: Arc<AtomicUsize>,
    }
    unsafe extern "C" fn run(context: *mut c_void) {
        let task = unsafe { &*context.cast::<Blocking>() };
        task.entered.send(()).unwrap();
        task.finish.recv().unwrap();
    }
    unsafe extern "C" fn release(context: *mut c_void) {
        let task = unsafe { Box::from_raw(context.cast::<Blocking>()) };
        task.released.fetch_add(1, Ordering::Release);
    }
    let executor = AsteriskExecutor::open("running-drain", 2).unwrap();
    let released = Arc::new(AtomicUsize::new(0));
    let (entered_send, entered) = mpsc::channel();
    let (finish, finish_receive) = mpsc::channel();
    let payload = Box::into_raw(Box::new(Blocking {
        entered: entered_send,
        finish: finish_receive,
        released: released.clone(),
    }))
    .cast();
    assert!(
        executor
            .submit(Task {
                context: payload,
                run,
                release
            })
            .is_ok()
    );
    std::thread::scope(|scope| {
        let executor_ref = &executor;
        let worker = scope.spawn(move || fixture::execute_all(executor_ref.handle));
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let drain = scope.spawn(|| executor.stop_and_drain());
        let deadline = Instant::now() + Duration::from_secs(2);
        while executor.state.admission.lock().unwrap().accepting {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!drain.is_finished());
        assert_eq!(released.load(Ordering::Acquire), 0);
        let output = Arc::new(Mutex::new(Vec::new()));
        let (rejected, reason) = executor.submit(task(1, &output, &released)).unwrap_err();
        assert_eq!(reason, SubmitError::Stopped);
        unsafe { discard(rejected) };
        finish.send(()).unwrap();
        worker.join().unwrap();
        assert!(drain.join().unwrap());
    });
    assert_eq!(released.load(Ordering::Acquire), 2);
}

#[test]
fn each_application_executor_has_a_unique_taskprocessor_name() {
    let first = AsteriskExecutor::open("application", 1).unwrap();
    let second = AsteriskExecutor::open("application", 1).unwrap();
    assert_ne!(fixture::name(first.handle), fixture::name(second.handle));
}

#[test]
fn close_waits_for_running_callback_and_final_worker_unreference() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    struct Blocking {
        entered: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }
    unsafe extern "C" fn run(context: *mut c_void) {
        let payload = unsafe { &*context.cast::<Blocking>() };
        payload.entered.send(()).unwrap();
        payload.resume.recv().unwrap();
    }
    unsafe extern "C" fn release(context: *mut c_void) {
        drop(unsafe { Box::from_raw(context.cast::<Blocking>()) });
    }
    let context = unsafe { open_abi(c"close-barrier".as_ptr(), 1) };
    let executor = unsafe { &*context.cast::<AsteriskExecutor>() };
    let handle = executor.handle as usize;
    let state = executor.state.clone();
    let (returned, resume_worker, unreference_started) =
        fixture::pause_worker_exit(executor.handle);
    let (entered_send, entered) = mpsc::channel();
    let (resume_task, resume_receive) = mpsc::channel();
    let payload = Box::into_raw(Box::new(Blocking {
        entered: entered_send,
        resume: resume_receive,
    }))
    .cast();
    assert_eq!(
        unsafe {
            submit_abi(
                context,
                Task {
                    context: payload,
                    run,
                    release,
                },
            )
        },
        0
    );
    let worker = std::thread::spawn(move || fixture::execute_all(handle as *mut _));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let context = context as usize;
    let close = std::thread::spawn(move || unsafe { close_abi(context as *mut _) });
    let deadline = Instant::now() + Duration::from_secs(2);
    while state.admission.lock().unwrap().accepting {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!close.is_finished());
    resume_task.send(()).unwrap();
    returned.recv_timeout(Duration::from_secs(2)).unwrap();
    unreference_started
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert!(
        !close.is_finished(),
        "drain alone cannot authorize unloading callback code"
    );
    resume_worker.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(close.join().unwrap(), 0);
}
