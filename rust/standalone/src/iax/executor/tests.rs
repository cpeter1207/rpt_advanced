use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static CREATED: AtomicUsize = AtomicUsize::new(0);
static DESTROYED: AtomicUsize = AtomicUsize::new(0);

impl Executor {
    /// Exercise host cleanup without changing the retained native runtime's lifecycle.
    pub(crate) fn reject_attachment_for_test(&mut self) {
        self.api.peer_attach = Some(attach_failure);
    }
}

unsafe extern "C" fn create(out: *mut *mut c_void) -> i32 {
    CREATED.fetch_add(1, Ordering::Relaxed);
    unsafe { *out = Box::into_raw(Box::new(7_u8)).cast() };
    0
}
unsafe extern "C" fn destroy(runtime: *mut c_void) {
    unsafe { drop(Box::from_raw(runtime.cast::<u8>())) };
    DESTROYED.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn attach(
    runtime: *mut c_void,
    peer: *mut c_void,
    step: Option<Step>,
    context: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    assert_eq!(unsafe { *runtime.cast::<u8>() }, 7);
    assert_eq!(peer as usize, 123);
    assert_eq!(unsafe { step.unwrap()(context) }, 0);
    unsafe { *out = context };
    0
}
unsafe extern "C" fn stop(token: *mut c_void) {
    unsafe { (*token.cast::<AtomicUsize>()).fetch_add(10, Ordering::Relaxed) };
}
unsafe extern "C" fn detach(token: *mut c_void) {
    unsafe { (*token.cast::<AtomicUsize>()).fetch_add(100, Ordering::Relaxed) };
}
unsafe extern "C" fn step(context: *mut c_void) -> i32 {
    unsafe { (*context.cast::<AtomicUsize>()).fetch_add(1, Ordering::Relaxed) };
    0
}
unsafe extern "C" fn configure(peer: *mut c_void, jitter: *const c_void) -> i32 {
    assert_eq!(peer as usize, 123);
    assert_eq!(jitter as usize, 456);
    0
}
unsafe extern "C" fn enable(runtime: *mut c_void, server: *mut c_void) -> i32 {
    assert_eq!(unsafe { *runtime.cast::<u8>() }, 7);
    assert_eq!(server as usize, 789);
    0
}
fn descriptor() -> Descriptor {
    Descriptor {
        struct_size: size_of::<Descriptor>() as u32,
        abi_version: 1,
        capability: *b"rptadv.iaxasync\0",
        runtime_create: Some(create),
        runtime_destroy: Some(destroy),
        peer_attach: Some(attach),
        peer_stop: Some(stop),
        peer_detach: Some(detach),
        peer_configure_jitter: Some(configure),
        server_enable: Some(enable),
        peer_observe: Some(observe),
        peer_wake: Some(stop),
        peer_set_deadline: Some(deadline),
    }
}
unsafe extern "C" fn deadline(_: *mut c_void, _: u64) -> i32 {
    0
}

#[test]
fn retained_runtime_forwards_callback_and_quiesces_before_destroy() {
    let api = descriptor();
    let before = DESTROYED.load(Ordering::Relaxed);
    let mut executor = unsafe { Executor::new(ptr::from_ref(&api).cast()) }.unwrap();
    let state = AtomicUsize::new(0);
    let context = ptr::from_ref(&state).cast_mut().cast();
    unsafe {
        executor
            .configure_jitter(123_usize as *mut c_void, 456_usize as *const c_void)
            .unwrap();
        executor.enable_server(789_usize as *mut c_void).unwrap();
    }
    let token = unsafe { executor.attach(123_usize as *mut c_void, step, context) }.unwrap();
    assert_eq!(token, context);
    assert_eq!(
        unsafe { executor.observe(123_usize as *mut c_void, context) },
        0
    );
    unsafe {
        executor.stop(token);
        executor.wake(token);
        assert_eq!(executor.set_deadline(123_usize as *mut c_void, u64::MAX), 0);
        executor.detach(token)
    };
    assert_eq!(state.load(Ordering::Relaxed), 122);
    executor.api.peer_attach = Some(attach_failure);
    assert_eq!(
        unsafe { executor.attach(ptr::null_mut(), step, context) },
        Err(Error::Attach)
    );
    executor.api.peer_attach = Some(attach_null);
    assert_eq!(
        unsafe { executor.attach(ptr::null_mut(), step, context) },
        Err(Error::Attach)
    );
    executor.api.peer_configure_jitter = Some(configure_failure);
    assert_eq!(
        unsafe { executor.configure_jitter(ptr::null_mut(), ptr::null()) },
        Err(Error::Initialize)
    );
    executor.api.server_enable = Some(enable_failure);
    assert_eq!(
        unsafe { executor.enable_server(ptr::null_mut()) },
        Err(Error::Initialize)
    );
    drop(executor);
    assert_eq!(DESTROYED.load(Ordering::Relaxed), before + 1);
}

#[test]
fn truncated_wrong_and_incomplete_descriptors_are_rejected_before_create() {
    assert!(unsafe { Executor::new(ptr::null()) }.is_err());
    let prefix = [8_u32, 1];
    assert!(unsafe { Executor::new(prefix.as_ptr().cast()) }.is_err());
    let changes: &[fn(&mut Descriptor)] = &[
        |api| api.abi_version = 2,
        |api| api.capability[0] = b'?',
        |api| api.runtime_create = None,
        |api| api.runtime_destroy = None,
        |api| api.peer_attach = None,
        |api| api.peer_stop = None,
        |api| api.peer_detach = None,
        |api| api.peer_configure_jitter = None,
        |api| api.server_enable = None,
        |api| api.peer_observe = None,
        |api| api.peer_wake = None,
        |api| api.peer_set_deadline = None,
    ];
    for change in changes {
        let mut api = descriptor();
        change(&mut api);
        assert!(unsafe { Executor::new(ptr::from_ref(&api).cast()) }.is_err());
    }
    for create in [create_failure as _, create_null as _] {
        let mut api = descriptor();
        api.runtime_create = Some(create);
        assert!(matches!(
            unsafe { Executor::new(ptr::from_ref(&api).cast()) },
            Err(Error::Initialize)
        ));
    }
}

unsafe extern "C" fn create_failure(_: *mut *mut c_void) -> i32 {
    -1
}
unsafe extern "C" fn create_null(_: *mut *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn attach_failure(
    _: *mut c_void,
    _: *mut c_void,
    _: Option<Step>,
    _: *mut c_void,
    _: *mut *mut c_void,
) -> i32 {
    -1
}
unsafe extern "C" fn attach_null(
    _: *mut c_void,
    _: *mut c_void,
    _: Option<Step>,
    _: *mut c_void,
    _: *mut *mut c_void,
) -> i32 {
    0
}
unsafe extern "C" fn configure_failure(_: *mut c_void, _: *const c_void) -> i32 {
    -1
}
unsafe extern "C" fn enable_failure(_: *mut c_void, _: *mut c_void) -> i32 {
    -1
}
unsafe extern "C" fn observe(peer: *mut c_void, output: *mut c_void) -> i32 {
    assert_eq!(peer as usize, 123);
    unsafe { (*output.cast::<AtomicUsize>()).fetch_add(1, Ordering::Relaxed) };
    0
}
