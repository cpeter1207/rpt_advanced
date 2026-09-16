use super::*;
use std::ffi::{c_char, c_int};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

type Task = crate::abi::rptadv_control_task_v1;

struct Backend {
    pending: Mutex<Option<Task>>,
    accepting: AtomicBool,
}

unsafe extern "C" fn open(_name: *const c_char, capacity: usize) -> *mut c_void {
    if capacity == 0 {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Backend {
        pending: Mutex::new(None),
        accepting: AtomicBool::new(true),
    }))
    .cast()
}

unsafe extern "C" fn submit(context: *mut c_void, task: Task) -> c_int {
    let backend = unsafe { &*context.cast::<Backend>() };
    if !backend.accepting.load(Ordering::Acquire) {
        return 1;
    }
    let mut pending = backend.pending.lock().unwrap();
    if pending.is_some() {
        2
    } else {
        *pending = Some(task);
        0
    }
}

unsafe extern "C" fn stop(context: *mut c_void) -> c_int {
    let backend = unsafe { &*context.cast::<Backend>() };
    backend.accepting.store(false, Ordering::Release);
    let task = backend.pending.lock().unwrap().take();
    if let Some(task) = task {
        unsafe {
            task.run.unwrap()(task.context);
            task.release.unwrap()(task.context);
        }
    }
    0
}

unsafe extern "C" fn close(context: *mut c_void) -> c_int {
    unsafe {
        stop(context);
        drop(Box::from_raw(context.cast::<Backend>()));
    }
    0
}

static DESCRIPTOR: Descriptor = Descriptor {
    abi_version: 1,
    struct_size: size_of::<Descriptor>(),
    capability: c"rptadv.control".as_ptr(),
    open: Some(open),
    submit: Some(submit),
    stop_and_drain: Some(stop),
    close: Some(close),
};

// SAFETY: immutable test descriptor points only to static code and text.
unsafe impl Sync for Descriptor {}

#[test]
fn truncated_or_missing_operation_tables_fail_before_open() {
    #[repr(C)]
    struct Header {
        abi_version: u32,
        struct_size: usize,
    }
    let prefix = Header {
        abi_version: 1,
        struct_size: size_of::<Header>(),
    };
    assert!(matches!(
        unsafe { ControlClient::open(ptr::from_ref(&prefix).cast(), "test", 1) },
        Err(ClientError::Incompatible)
    ));
    assert!(matches!(
        unsafe { ControlClient::open(ptr::null(), "test", 1) },
        Err(ClientError::Incompatible)
    ));
    for index in 0..6 {
        let mut descriptor = Descriptor {
            abi_version: 1,
            struct_size: size_of::<Descriptor>(),
            capability: c"rptadv.control".as_ptr(),
            open: Some(open),
            submit: Some(submit),
            stop_and_drain: Some(stop),
            close: Some(close),
        };
        match index {
            0 => descriptor.open = None,
            1 => descriptor.submit = None,
            2 => descriptor.stop_and_drain = None,
            3 => descriptor.close = None,
            4 => descriptor.capability = ptr::null(),
            _ => descriptor.capability = c"wrong".as_ptr(),
        }
        assert!(matches!(
            unsafe { ControlClient::open(&descriptor, "test", 1) },
            Err(ClientError::Incompatible)
        ));
    }
}

#[test]
fn client_validates_and_preserves_rejected_ownership() {
    let mut bad = Descriptor {
        abi_version: 2,
        struct_size: size_of::<Descriptor>(),
        capability: c"rptadv.control".as_ptr(),
        open: Some(open),
        submit: Some(submit),
        stop_and_drain: Some(stop),
        close: Some(close),
    };
    assert_eq!(
        unsafe { ControlClient::open(&bad, "test", 1) }.err(),
        Some(ClientError::Incompatible)
    );
    bad.abi_version = 1;
    assert_eq!(
        unsafe { ControlClient::open(&bad, "", 1) }.err(),
        Some(ClientError::InvalidName)
    );
    let client = unsafe { ControlClient::open(&DESCRIPTOR, "test", 1) }.unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::clone(&output);
    assert!(
        client
            .submit(ControlTask::lifecycle(move || first
                .lock()
                .unwrap()
                .push(1)))
            .is_ok()
    );
    let second = Arc::clone(&output);
    let rejected = client
        .submit(ControlTask::lifecycle(move || {
            second.lock().unwrap().push(2)
        }))
        .unwrap_err();
    assert_eq!(rejected.reason, RejectionReason::Full);
    rejected.task.run();
    client.stop_and_drain().unwrap();
    assert_eq!(*output.lock().unwrap(), [2, 1]);
    assert_eq!(
        client
            .submit(ControlTask::lifecycle(|| panic!("stopped task ran")))
            .unwrap_err()
            .reason,
        RejectionReason::Stopped
    );
}

#[test]
fn client_contains_task_panic_and_releases_consumed_payload_once() {
    struct Released(Arc<AtomicUsize>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let client = unsafe { ControlClient::open(&DESCRIPTOR, "panic", 1) }.unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let payload = Released(count.clone());
    assert!(
        client
            .submit(ControlTask::lifecycle(move || {
                let _payload = payload;
                panic!("contained task panic");
            }))
            .is_ok()
    );
    client.stop_and_drain().unwrap();
    drop(client);
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

#[test]
fn invalid_names_unavailable_backend_and_provider_rejections_preserve_ownership() {
    assert_eq!(
        unsafe { ControlClient::open(&DESCRIPTOR, "bad\0name", 1) }.err(),
        Some(ClientError::InvalidName)
    );
    assert_eq!(
        unsafe { ControlClient::open(&DESCRIPTOR, "empty", 0) }.err(),
        Some(ClientError::Unavailable)
    );
    unsafe extern "C" fn reject(_: *mut c_void, _: Task) -> c_int {
        3
    }
    unsafe extern "C" fn on_executor(_: *mut c_void) -> c_int {
        -1
    }
    let descriptor = Descriptor {
        submit: Some(reject),
        stop_and_drain: Some(on_executor),
        ..DESCRIPTOR
    };
    let client = unsafe { ControlClient::open(&descriptor, "backend", 1) }.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let output = calls.clone();
    let rejected = client
        .submit(ControlTask::lifecycle(move || {
            output.fetch_add(1, Ordering::Relaxed);
        }))
        .unwrap_err();
    assert_eq!(rejected.reason, RejectionReason::Backend);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    rejected.task.run();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(client.stop_and_drain(), Err(DrainError::OnExecutor));
}
