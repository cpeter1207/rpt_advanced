//! Echo handles use provider metadata and never enter IAX-only callbacks.
use super::*;
use rptadv_peer_support::echolink::{abi as echo, host::Provider};
#[path = "../../../peer-support/tests/support/mod.rs"]
mod support;
use crate::abi as host_abi;
#[path = "../../../peer-support/tests/support/host_media.rs"]
mod media;

#[test]
fn echo_media_and_registration_dispatch_preserves_the_provider_contract() {
    let host = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
    unsafe { media::check(host.descriptor().context) };
}

#[test]
fn echo_handles_never_enter_iax_only_callbacks() {
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
    let host = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
    let context = host.descriptor().context;
    let mut info: abi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
    info.struct_size = size_of::<abi::rptadv_peer_info_v1>() as u32;
    info.abi_version = 1;
    unsafe {
        assert_eq!(peer_info(context, handle, &mut info), 0);
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
            input(info.remote.data.cast(), info.remote.length),
            Some("4294967295")
        );
        assert_eq!(
            input(info.local.data.cast(), info.local.length),
            Some("524950")
        );
        assert_eq!(
            input(info.callsign.data.cast(), info.callsign.length),
            Some("W1XYZ-R")
        );
        assert_eq!(peer_rate(context, handle), 8000);
        assert_eq!(peer_ready(context, handle), -1);
        assert_eq!(peer_send_digit(context, handle, b'5'), -1);
        assert_eq!(peer_read(context, handle, None, ptr::null_mut()), -1);
        peer_destroy(context, handle);
    }
}
