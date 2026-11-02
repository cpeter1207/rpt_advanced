//! The existing control client is shared by product and host admission, not reimplemented.
use rptadv_peer_support::control::{
    Client, ClientError, Descriptor, DrainError, RejectionReason, Task,
};
use std::{
    ffi::{c_char, c_int, c_void},
    mem::size_of,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Backend {
    pending: Mutex<Option<Task>>,
    accepting: AtomicBool,
}
unsafe extern "C" fn open(_: *const c_char, capacity: usize) -> *mut c_void {
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
    if let Some(task) = backend.pending.lock().unwrap().take() {
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
    for pointer in [ptr::null(), ptr::from_ref(&prefix).cast()] {
        assert_eq!(
            unsafe { Client::open(pointer, "test", 1) }.err(),
            Some(ClientError::Incompatible)
        );
    }
    for index in 0..7 {
        let mut descriptor = DESCRIPTOR;
        match index {
            0 => descriptor.open = None,
            1 => descriptor.submit = None,
            2 => descriptor.stop_and_drain = None,
            3 => descriptor.close = None,
            4 => descriptor.capability = ptr::null(),
            5 => descriptor.capability = c"wrong".as_ptr(),
            _ => descriptor.abi_version = 2,
        }
        assert_eq!(
            unsafe { Client::open(&descriptor, "test", 1) }.err(),
            Some(ClientError::Incompatible)
        );
    }
    for (name, capacity, expected) in [
        ("", 1, ClientError::InvalidName),
        ("bad\0name", 1, ClientError::InvalidName),
        ("empty", 0, ClientError::Unavailable),
    ] {
        assert_eq!(
            unsafe { Client::open(&DESCRIPTOR, name, capacity) }.err(),
            Some(expected)
        );
    }
}

#[test]
fn rejection_returns_original_payload_and_drain_preserves_fifo_ownership() {
    fn append((output, value): (Arc<Mutex<Vec<u32>>>, u32)) {
        output.lock().unwrap().push(value);
    }
    let client = unsafe { Client::open(&DESCRIPTOR, "test", 1) }.unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    assert!(client.submit((output.clone(), 1), append).is_ok());
    let rejected = client.submit((output.clone(), 2), append).unwrap_err();
    assert_eq!(rejected.reason, RejectionReason::Full);
    append(rejected.payload);
    client.stop_and_drain().unwrap();
    assert_eq!(*output.lock().unwrap(), [2, 1]);
    assert_eq!(
        client.submit((output, 3), append).unwrap_err().reason,
        RejectionReason::Stopped
    );
}

#[test]
fn task_panic_and_execution_before_submit_returns_release_payload_once() {
    struct Released(Arc<AtomicUsize>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn panic_task(_payload: Released) {
        panic!("contained task panic");
    }
    unsafe extern "C" fn inline(_: *mut c_void, task: Task) -> c_int {
        unsafe {
            task.run.unwrap()(task.context);
            task.release.unwrap()(task.context);
        }
        0
    }
    for descriptor in [
        DESCRIPTOR,
        Descriptor {
            submit: Some(inline),
            ..DESCRIPTOR
        },
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let client = unsafe { Client::open(&descriptor, "panic", 1) }.unwrap();
        assert!(client.submit(Released(count.clone()), panic_task).is_ok());
        client.stop_and_drain().unwrap();
        drop(client);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn backend_failure_preserves_payload_and_reports_unsafe_self_drain() {
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
    let client = unsafe { Client::open(&descriptor, "backend", 1) }.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let rejected = client
        .submit(calls.clone(), |calls| {
            calls.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap_err();
    assert_eq!(rejected.reason, RejectionReason::Backend);
    assert!(Arc::ptr_eq(&calls, &rejected.payload));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(client.stop_and_drain(), Err(DrainError::OnExecutor));
}
