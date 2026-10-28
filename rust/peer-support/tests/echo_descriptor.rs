//! Reject partial host capabilities before any provider side effect.

use rptadv_peer_support::echolink::{Api, abi};
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn create(
    _: *const abi::RuntimeConfig,
    _: *mut *mut abi::Context,
) -> abi::Status {
    CALLS.fetch_add(1, Ordering::Relaxed);
    abi::Status::Failed
}
unsafe extern "C" fn destroy_context(_: *mut abi::Context) {}
unsafe extern "C" fn prepare(
    _: *mut abi::Context,
    _: *const abi::AccountConfig,
    _: *const abi::AccountSecrets,
    _: *mut *mut abi::Account,
) -> abi::Status {
    abi::Status::Failed
}
unsafe extern "C" fn stop(_: *mut abi::Account) -> abi::Status {
    abi::Status::Ok
}
unsafe extern "C" fn destroy(_: *mut abi::Account) {}

fn lifecycle() -> abi::Descriptor {
    abi::Descriptor {
        struct_size: size_of::<abi::Descriptor>() as u32,
        abi_version: 1,
        capability: *b"rptadv.echolink.v1\0\0\0\0\0\0",
        capabilities: 1,
        context_create: Some(create),
        context_destroy: Some(destroy_context),
        account_prepare: Some(prepare),
        account_activate: None,
        account_reconfigure: None,
        account_stop: Some(stop),
        account_destroy: Some(destroy),
        dial: None,
        validate_pending: None,
        peer_info: None,
        peer_attach: None,
        peer_stop: None,
        peer_detach: None,
        peer_wake: None,
        peer_set_deadline: None,
    }
}

#[test]
fn partial_descriptor_fails_before_side_effect() {
    assert!(unsafe { Api::open(std::ptr::null()) }.is_err());
    let mut short = lifecycle();
    short.struct_size = 8;
    assert!(unsafe { Api::open(&short) }.is_err());
    for changed in 0..10 {
        let mut table = lifecycle();
        match changed {
            0 => table.abi_version = 2,
            1 => table.capability[0] = b'X',
            2 => table.context_create = None,
            3 => table.context_destroy = None,
            4 => table.account_prepare = None,
            5 => table.account_stop = None,
            6 => table.account_destroy = None,
            7 => table.capabilities = 2,
            8 => table.capabilities = 3,
            _ => table.capabilities = 0x8000_0001,
        }
        assert!(unsafe { Api::open(&table) }.is_err());
    }
    let table = lifecycle();
    let api = unsafe { Api::open(&table) }.unwrap();
    assert!(!api.network_available());
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert_eq!(
        unsafe { api.table().context_create.unwrap()(std::ptr::null(), std::ptr::null_mut()) },
        abi::Status::Failed
    );
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
}

#[test]
fn runtime_resource_tail_preserves_the_three_word_lifecycle_prefix() {
    use std::mem::offset_of;
    let config = abi::RuntimeConfig {
        gsm_library: abi::ByteSpan {
            data: b"gsm.so".as_ptr(),
            length: 6,
        },
        speex_library: abi::ByteSpan::default(),
        jitter_library: abi::ByteSpan {
            data: b"jitter.so".as_ptr(),
            length: 9,
        },
        ..Default::default()
    };
    assert_eq!(offset_of!(abi::RuntimeConfig, worker_threads), 8);
    assert_eq!(offset_of!(abi::RuntimeConfig, gsm_library), 16);
    assert_eq!(offset_of!(abi::RuntimeConfig, speex_library), 32);
    assert_eq!(offset_of!(abi::RuntimeConfig, jitter_library), 48);
    assert_eq!(config.struct_size, 64);
    let words = unsafe { std::slice::from_raw_parts(std::ptr::from_ref(&config).cast::<u32>(), 3) };
    assert_eq!(words, [64, 1, 0]);
}
