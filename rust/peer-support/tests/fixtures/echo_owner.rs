//! Isolated ABI fixture: reject early context destruction while an account survives.
#[path = "../../src/echolink/abi.rs"]
pub mod abi;
use std::{
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

static CONTEXTS: AtomicUsize = AtomicUsize::new(0);
static ACCOUNTS: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn fixture_peer() -> *mut abi::Qso {
    Box::into_raw(Box::new(1_u8)).cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_peer_destroy(peer: *mut abi::Qso) {
    unsafe { drop(Box::from_raw(peer.cast::<u8>())) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_expire(peer: *mut abi::Qso) {
    unsafe { peer.cast::<u8>().write(0) };
}
unsafe extern "C" fn validate(peer: *const abi::Qso, output: *mut abi::PeerInfo) -> abi::Status {
    if unsafe { *peer.cast::<u8>() } == 0 {
        return abi::Status::Closed;
    }
    unsafe { info(peer, output) }
}
unsafe extern "C" fn info(_: *const abi::Qso, output: *mut abi::PeerInfo) -> abi::Status {
    let bytes = |value: &'static [u8]| abi::ByteSpan {
        data: value.as_ptr(),
        length: value.len(),
    };
    unsafe {
        output.write(abi::PeerInfo {
            struct_size: size_of::<abi::PeerInfo>() as u32,
            abi_version: 1,
            node_number: u32::MAX,
            capabilities: 14,
            account_generation: 4,
            qso_generation: 9,
            callsign: bytes(b"W1XYZ-R"),
            local_node: bytes(b"524950"),
            source: bytes(b"127.0.0.2:49152"),
        })
    };
    abi::Status::Ok
}
macro_rules! operation {
    ($name:ident($($arg:ty),* $(,)?)) => {
        unsafe extern "C" fn $name($(_: $arg),*) -> abi::Status { abi::Status::Failed }
    };
}
operation!(activate(*mut abi::Account, Option<abi::EventFn>, *mut c_void));
operation!(reconfigure(*mut abi::Account, *const abi::AccountConfig, *const abi::AccountSecrets));
operation!(dial(*mut abi::Account, u32, *mut *mut abi::Qso));
struct Registration {
    step: abi::StepFn,
    context: *mut c_void,
    stopped: std::sync::atomic::AtomicBool,
}
unsafe extern "C" fn attach(
    _: *mut abi::Context,
    _: *mut abi::Qso,
    step: Option<abi::StepFn>,
    context: *mut c_void,
    out: *mut *mut abi::Registration,
) -> abi::Status {
    let step = step.unwrap();
    unsafe { step(context) };
    unsafe {
        out.write(
            Box::into_raw(Box::new(Registration {
                step,
                context,
                stopped: std::sync::atomic::AtomicBool::new(false),
            }))
            .cast(),
        )
    };
    abi::Status::Ok
}
operation!(deadline(*mut abi::Qso, u64));
operation!(
    read(
        *mut abi::Qso,
        u32,
        *const abi::PcmSink,
        Option<abi::TextFn>,
        *mut c_void,
        *mut abi::ReadResult,
    )
);
operation!(encode(*mut abi::Qso, *const f32, usize, *mut u8, usize, u32, *mut c_void));
operation!(reset(*mut abi::Qso));
operation!(send(*mut abi::Qso, abi::ByteSpan, u32, u32));
operation!(elapsed(*const abi::Qso, *mut u64));
operation!(text(*mut abi::Qso, abi::ByteSpan, abi::ByteSpan));
unsafe extern "C" fn registration_stop(registration: *mut abi::Registration) {
    unsafe { &*registration.cast::<Registration>() }
        .stopped
        .store(true, Ordering::Release);
}
unsafe extern "C" fn registration_wake(registration: *mut abi::Registration) {
    let registration = unsafe { &*registration.cast::<Registration>() };
    if !registration.stopped.load(Ordering::Acquire) {
        unsafe { (registration.step)(registration.context) };
    }
}
unsafe extern "C" fn registration_detach(registration: *mut abi::Registration) {
    unsafe { drop(Box::from_raw(registration.cast::<Registration>())) };
}

unsafe extern "C" fn create(
    _: *const abi::RuntimeConfig,
    out: *mut *mut abi::Context,
) -> abi::Status {
    assert_eq!(CONTEXTS.fetch_add(1, Ordering::SeqCst), 0);
    assert_eq!(ACCOUNTS.load(Ordering::SeqCst), 0);
    unsafe { out.write(Box::into_raw(Box::new(1_u8)).cast()) };
    abi::Status::Ok
}
unsafe extern "C" fn destroy_context(context: *mut abi::Context) {
    assert_eq!(ACCOUNTS.load(Ordering::SeqCst), 0);
    CONTEXTS.fetch_sub(1, Ordering::SeqCst);
    unsafe { drop(Box::from_raw(context.cast::<u8>())) };
}
unsafe extern "C" fn prepare(
    _: *mut abi::Context,
    config: *const abi::AccountConfig,
    _: *const abi::AccountSecrets,
    out: *mut *mut abi::Account,
) -> abi::Status {
    ACCOUNTS.fetch_add(1, Ordering::SeqCst);
    unsafe { out.write(Box::into_raw(Box::new((*config).node_number)).cast()) };
    abi::Status::Ok
}
unsafe extern "C" fn stop(account: *mut abi::Account) -> abi::Status {
    if CONTEXTS.load(Ordering::SeqCst) == 1 && unsafe { *account.cast::<u32>() } != 99 {
        abi::Status::Ok
    } else {
        abi::Status::Failed
    }
}
unsafe extern "C" fn destroy(account: *mut abi::Account) {
    ACCOUNTS.fetch_sub(1, Ordering::SeqCst);
    unsafe { drop(Box::from_raw(account.cast::<u32>())) };
}

#[unsafe(no_mangle)]
pub extern "C" fn fixture_contexts() -> usize {
    CONTEXTS.load(Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub extern "C" fn fixture_accounts() -> usize {
    ACCOUNTS.load(Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub extern "C" fn rptadv_echolink_descriptor_v1() -> *const abi::Descriptor {
    static TABLE: abi::Descriptor = abi::Descriptor {
        struct_size: size_of::<abi::Descriptor>() as u32,
        abi_version: 1,
        capability: *b"rptadv.echolink.v1\0\0\0\0\0\0",
        capabilities: if cfg!(network) {
            15
        } else if cfg!(partial) {
            3
        } else {
            1
        },
        context_create: Some(create),
        context_destroy: Some(destroy_context),
        account_prepare: Some(prepare),
        account_stop: Some(stop),
        account_destroy: Some(destroy),
        account_activate: if cfg!(network) { Some(activate) } else { None },
        account_reconfigure: if cfg!(network) {
            Some(reconfigure)
        } else {
            None
        },
        dial: if cfg!(network) { Some(dial) } else { None },
        validate_pending: if cfg!(network) { Some(validate) } else { None },
        peer_info: if cfg!(network) { Some(info) } else { None },
        peer_attach: if cfg!(network) { Some(attach) } else { None },
        peer_stop: if cfg!(network) {
            Some(registration_stop)
        } else {
            None
        },
        peer_detach: if cfg!(network) {
            Some(registration_detach)
        } else {
            None
        },
        peer_wake: if cfg!(network) {
            Some(registration_wake)
        } else {
            None
        },
        peer_set_deadline: if cfg!(network) { Some(deadline) } else { None },
        peer_destroy: if cfg!(network) {
            Some(fixture_peer_destroy)
        } else {
            None
        },
        peer_read: if cfg!(network) { Some(read) } else { None },
        peer_prepare_audio: if cfg!(network) { Some(encode) } else { None },
        peer_reset_encoder: if cfg!(network) { Some(reset) } else { None },
        peer_send_prepared: if cfg!(network) { Some(send) } else { None },
        peer_elapsed_ms: if cfg!(network) { Some(elapsed) } else { None },
        peer_send_text: if cfg!(network) { Some(text) } else { None },
    };
    ptr::from_ref(&TABLE)
}
