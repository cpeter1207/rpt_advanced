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
