//! Isolated ABI fixture: reject early context destruction while an account survives.
#[path = "../../src/echolink/abi.rs"]
pub mod abi;
use std::{
    mem::size_of,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

static CONTEXTS: AtomicUsize = AtomicUsize::new(0);
static ACCOUNTS: AtomicUsize = AtomicUsize::new(0);

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
        capabilities: if cfg!(partial) { 3 } else { 1 },
        context_create: Some(create),
        context_destroy: Some(destroy_context),
        account_prepare: Some(prepare),
        account_stop: Some(stop),
        account_destroy: Some(destroy),
        account_activate: None,
        account_reconfigure: None,
        dial: None,
        validate_pending: None,
        peer_info: None,
        peer_attach: None,
        peer_stop: None,
        peer_detach: None,
        peer_wake: None,
        peer_set_deadline: None,
        peer_destroy: None,
        peer_read: None,
        peer_prepare_audio: None,
        peer_reset_encoder: None,
        peer_send_prepared: None,
        peer_elapsed_ms: None,
        peer_send_text: None,
    };
    ptr::from_ref(&TABLE)
}
