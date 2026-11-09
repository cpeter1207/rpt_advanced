//! Echo metadata is shared; native Asterisk channel operations remain IAX-only.
use super::*;
use rptadv_peer_support::echolink::{abi as echo, host::Provider};
#[path = "../../../peer-support/tests/support/mod.rs"]
pub(crate) mod support;
use crate::bindings as host_abi;
#[path = "../../../peer-support/tests/support/host_media.rs"]
mod media;

#[path = "../../../peer-support/tests/support/host_accounts.rs"]
mod accounts;

#[path = "../../../peer-support/tests/support/peer_metadata.rs"]
mod peer_metadata;

#[test]
fn peer_metadata_writes_only_the_supported_caller_prefix_for_both_protocols() {
    crate::fixture::reset();
    let fixture = support::Fixture::new("network");
    let iax = PeerIo::dial(c"radio@host/200", c"100", 8, || true).unwrap();
    let handles = [
        into_raw(iax, "1000", "2000"),
        Box::into_raw(Box::new(PeerHandle::Echo(fixture.peer()))).cast(),
    ];
    for (index, handle) in handles.into_iter().enumerate() {
        peer_metadata::check(
            std::mem::offset_of!(ffi::rptadv_peer_info_v1, provider_account),
            size_of::<ffi::rptadv_peer_info_v1>(),
            |output| unsafe {
                let output = output.cast::<ffi::rptadv_peer_info_v1>();
                let result = peer_info(ptr::null_mut(), handle, output);
                if result == 0 {
                    let info = &*output;
                    assert_eq!(info.protocol, index as u32 + 1);
                    assert_eq!(
                        (
                            info.capabilities,
                            info.account_generation,
                            info.qso_generation
                        ),
                        if index == 0 { (12, 0, 0) } else { (23, 4, 9) }
                    );
                    assert_eq!(
                        text(info.remote.data.cast(), info.remote.length),
                        Some(if index == 0 { "2000" } else { "4294967295" })
                    );
                    if info.struct_size as usize == size_of::<ffi::rptadv_peer_info_v1>() {
                        assert!(info.provider_account.data.is_null());
                        assert_eq!(info.provider_account.length, 0);
                    }
                }
                result
            },
        );
        assert_eq!(
            unsafe { peer_info(ptr::null_mut(), handle, ptr::null_mut()) },
            -1
        );
        if index == 0 {
            assert_eq!(unsafe { peer_set_deadline(ptr::null_mut(), handle, 1) }, -1);
        }
        unsafe { peer_destroy(ptr::null_mut(), handle) };
    }
    let mut info: ffi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
    info.struct_size = size_of::<ffi::rptadv_peer_info_v1>() as u32;
    info.abi_version = 1;
    assert_eq!(
        unsafe { peer_info(ptr::null_mut(), ptr::null(), &mut info) },
        -1
    );
    crate::fixture::host(|state| state.clean());
}

#[test]
fn echo_accounts_prepare_before_activation_and_dial_from_the_stable_context() {
    accounts::check(HostServicesOwner::new());
}

#[test]
fn echo_media_and_registration_dispatch_preserves_the_provider_contract() {
    unsafe { media::check(ptr::null_mut()) };
}

#[test]
fn echo_binding_rejects_a_quiesced_radio_reservation() {
    crate::fixture::reset();
    let fixture = support::Fixture::new("network");
    let peer = Box::into_raw(Box::new(PeerHandle::Echo(fixture.peer()))).cast();
    let mut live = ptr::null_mut();
    assert_eq!(
        unsafe { radio_open(ptr::null_mut(), c"usb".as_ptr(), 3, 8, &mut live) },
        0
    );
    assert_eq!(unsafe { peer_bind_radio(ptr::null_mut(), peer, live) }, 0);
    assert_eq!(
        unsafe { radio_metadata_bind(ptr::null_mut(), live, None) },
        0
    );
    assert_eq!(crate::fixture::host(|state| state.channels), 1);
    unsafe { radio_destroy(ptr::null_mut(), live) };
    let mut reservation = ReservedRadio {
        metadata: None,
        radio: None,
        name: CString::new("quiesced fixture").unwrap(),
    };
    assert_eq!(
        unsafe {
            peer_bind_radio(
                ptr::null_mut(),
                peer,
                ptr::from_mut(&mut reservation).cast(),
            )
        },
        -1
    );
    unsafe { peer_destroy(ptr::null_mut(), peer) };
    crate::fixture::host(|state| state.clean());
}

#[test]
fn echo_handles_never_enter_asterisk_channel_callbacks() {
    let fixture = support::Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &echo::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &echo::AccountConfig {
                enabled: 1,
                local_node: echo::ByteSpan {
                    data: b"524950".as_ptr(),
                    length: 6,
                },
                ..echo::AccountConfig::default()
            },
            &echo::AccountSecrets {
                struct_size: size_of::<echo::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: echo::ByteSpan::default(),
                proxy_password: echo::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    let create =
        unsafe { library.get::<unsafe extern "C" fn() -> *mut echo::Qso>(b"fixture_peer\0") }
            .unwrap();
    let mut taken = 0;
    let peer = unsafe {
        account.take_pending(&echo::Event {
            struct_size: size_of::<echo::Event>() as u32,
            kind: 1,
            qso: create(),
            generation: 9,
            status: echo::Status::Pending,
            taken: &mut taken,
        })
    }
    .unwrap();
    let handle = Box::into_raw(Box::new(PeerHandle::Echo(peer))).cast();
    let mut info: ffi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
    info.struct_size = size_of::<ffi::rptadv_peer_info_v1>() as u32;
    info.abi_version = 1;
    unsafe {
        assert_eq!(peer_info(ptr::null_mut(), handle, &mut info), 0);
        assert_eq!(
            (
                info.protocol,
                info.capabilities,
                info.account_generation,
                info.qso_generation
            ),
            (2, 23, 4, 9)
        );
        assert_eq!(
            text(info.remote.data.cast(), info.remote.length),
            Some("4294967295")
        );
        assert_eq!(
            text(info.local.data.cast(), info.local.length),
            Some("524950")
        );
        assert_eq!(
            text(info.callsign.data.cast(), info.callsign.length),
            Some("W1XYZ-R")
        );
        assert_eq!(peer_rate(ptr::null_mut(), handle), 8000);
        assert_eq!(peer_ready(ptr::null_mut(), handle), -1);
        assert_eq!(peer_send_digit(ptr::null_mut(), handle, b'5'), -1);
        assert_eq!(
            peer_read(ptr::null_mut(), handle, None, ptr::null_mut()),
            -1
        );
        assert_eq!(peer_write(ptr::null_mut(), handle, [0.1].as_ptr(), 1), -1);
        peer_destroy(ptr::null_mut(), handle);
    }
}
