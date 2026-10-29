//! Complete, compatible media groups fail closed without invoking foreign code.
use rptadv_peer_support::echolink::{Api, abi};
use std::{
    ffi::c_void,
    mem::{offset_of, size_of},
};

macro_rules! status_fn {
    ($name:ident($($arg:ty),* $(,)?)) => {
        unsafe extern "C" fn $name($(_: $arg),*) -> abi::Status { abi::Status::Failed }
    };
}
status_fn!(create(*const abi::RuntimeConfig, *mut *mut abi::Context));
status_fn!(
    prepare(
        *mut abi::Context,
        *const abi::AccountConfig,
        *const abi::AccountSecrets,
        *mut *mut abi::Account,
    )
);
status_fn!(activate(*mut abi::Account, Option<abi::EventFn>, *mut c_void));
status_fn!(reconfigure(*mut abi::Account, *const abi::AccountConfig, *const abi::AccountSecrets));
status_fn!(stop(*mut abi::Account));
status_fn!(dial(*mut abi::Account, u32, *mut *mut abi::Qso));
status_fn!(info(*const abi::Qso, *mut abi::PeerInfo));
status_fn!(
    attach(
        *mut abi::Context,
        *mut abi::Qso,
        Option<abi::StepFn>,
        *mut c_void,
        *mut *mut abi::Registration,
    )
);
status_fn!(deadline(*mut abi::Qso, u64));
status_fn!(
    read(
        *mut abi::Qso,
        u32,
        *const abi::PcmSink,
        Option<abi::TextFn>,
        *mut c_void,
        *mut abi::ReadResult,
    )
);
status_fn!(encode(*mut abi::Qso, *const f32, usize, *mut u8, usize, u32, *mut c_void));
status_fn!(reset(*mut abi::Qso));
status_fn!(send(*mut abi::Qso, abi::ByteSpan, u32, u32));
status_fn!(elapsed(*const abi::Qso, *mut u64));
status_fn!(text(*mut abi::Qso, abi::ByteSpan, abi::ByteSpan));
status_fn!(apply(*mut abi::Account, *mut abi::Account));
unsafe extern "C" fn context_destroy(_: *mut abi::Context) {}
unsafe extern "C" fn account_destroy(_: *mut abi::Account) {}
unsafe extern "C" fn registration(_: *mut abi::Registration) {}
unsafe extern "C" fn peer_destroy(_: *mut abi::Qso) {}

fn complete() -> abi::Descriptor {
    abi::Descriptor {
        struct_size: size_of::<abi::Descriptor>() as u32,
        abi_version: 1,
        capability: *b"rptadv.echolink.v1\0\0\0\0\0\0",
        capabilities: 15,
        context_create: Some(create),
        context_destroy: Some(context_destroy),
        account_prepare: Some(prepare),
        account_activate: Some(activate),
        account_reconfigure: Some(reconfigure),
        account_stop: Some(stop),
        account_destroy: Some(account_destroy),
        dial: Some(dial),
        validate_pending: Some(info),
        peer_info: Some(info),
        peer_attach: Some(attach),
        peer_stop: Some(registration),
        peer_detach: Some(registration),
        peer_wake: Some(registration),
        peer_set_deadline: Some(deadline),
        peer_destroy: Some(peer_destroy),
        peer_read: Some(read),
        peer_prepare_audio: Some(encode),
        peer_reset_encoder: Some(reset),
        peer_send_prepared: Some(send),
        peer_elapsed_ms: Some(elapsed),
        peer_send_text: Some(text),
        account_apply_prepared: Some(apply),
    }
}

#[test]
fn media_and_text_require_their_complete_sized_operation_groups() {
    let api = unsafe { Api::open(&complete()) }.unwrap();
    assert!(api.network_available());
    assert!(api.prepared_media_available());
    assert!(api.text_available());
    for missing in 0..11 {
        let mut table = complete();
        match missing {
            0 => table.peer_read = None,
            1 => table.peer_prepare_audio = None,
            2 => table.peer_reset_encoder = None,
            3 => table.peer_send_prepared = None,
            4 => table.peer_elapsed_ms = None,
            5 => table.peer_send_text = None,
            6 => table.capabilities = 1 | 4,
            7 => table.capabilities = 1 | 2 | 8,
            8 => table.struct_size = offset_of!(abi::Descriptor, peer_send_text) as u32,
            9 => table.account_apply_prepared = None,
            _ => table.struct_size = offset_of!(abi::Descriptor, account_apply_prepared) as u32,
        }
        assert!(unsafe { Api::open(&table) }.is_err(), "case {missing}");
    }
}

#[test]
fn earlier_network_prefix_is_rejected_without_prepared_account_commit() {
    let mut table = complete();
    table.capabilities = 3;
    let length = offset_of!(abi::Descriptor, peer_read);
    table.struct_size = length as u32;
    let mut prefix = vec![0_usize; length / size_of::<usize>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&table).cast::<u8>(),
            prefix.as_mut_ptr().cast::<u8>(),
            length,
        );
    }
    assert!(unsafe { Api::open(prefix.as_ptr().cast()) }.is_err());
}

#[test]
fn read_result_has_explicit_activity_and_finite_completion_without_external_types() {
    assert_eq!(size_of::<abi::ReadResult>(), 28);
    assert_eq!(offset_of!(abi::ReadResult, finish_pending), 16);
    let result = abi::ReadResult::default();
    assert_eq!(result.struct_size, 28);
    assert_eq!(offset_of!(abi::ReadResult, represented_node_number), 24);
    assert_eq!(result.represented_node_number, 0);
    assert_eq!(result.abi_version, 1);
    assert_eq!(result.activity, 0);
    assert_eq!(result.finish_pending, 0);
}
