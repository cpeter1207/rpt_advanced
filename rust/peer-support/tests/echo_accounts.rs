//! Both hosts use one account/admission owner and the selected real control backend.
#![cfg(unix)]
use rptadv_peer_support::{
    control::Client,
    echolink::{
        abi,
        host::{Accounts, Error, Preparation, Provider},
    },
};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, mpsc},
    time::Duration,
};
#[path = "support/fixture.rs"]
mod support;

#[test]
fn preparation_is_lazy_and_uses_the_real_configuration_directory() {
    let fixture = support::Fixture::new("network");
    let directory = fixture.0.parent().unwrap();
    let configuration = directory.join("rpt_advanced.conf");
    let resources = abi::RuntimeConfig::default();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let mut disabled = unsafe {
        Preparation::new(
            &configuration,
            std::path::Path::new("missing-provider"),
            &resources,
            std::ptr::null(),
            |_| {},
        )
    };
    unsafe { disabled.prepare(&abi::AccountConfig::default(), None) }.unwrap();
    assert!(
        disabled.finish().is_none(),
        "disabled accounts must open nothing"
    );
    let path = directory.join("echolink-secrets.conf");
    std::fs::write(&path, "[524950]\ndirectory_password=fixture-value\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (send, receive) = mpsc::channel();
    let mut candidate = unsafe {
        Preparation::new(
            &configuration,
            &fixture.0,
            &resources,
            backend.cast(),
            move |peer| {
                send.send(peer.remote().to_owned()).unwrap();
            },
        )
    };
    let config = abi::AccountConfig {
        enabled: 1,
        node_number: 7,
        local_node: abi::ByteSpan {
            data: b"524950".as_ptr(),
            length: 6,
        },
        ..abi::AccountConfig::default()
    };
    unsafe { candidate.prepare(&config, None) }.unwrap();
    let accounts = candidate.finish().unwrap();
    assert!(
        receive.try_recv().is_err(),
        "preparation must not activate a callback"
    );
    unsafe { accounts.activate() }.unwrap();
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(5)).unwrap(),
        "4294967295"
    );
    accounts.stop().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn accounts_prepare_private_credentials_then_handoff_or_reject_without_leaking() {
    for rejected in [false, true] {
        let fixture = support::Fixture::new("network");
        let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
        let provider =
            unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
        let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let control =
            Arc::new(unsafe { Client::open(backend.cast(), "echo-admission-test", 1) }.unwrap());
        let (entered, ready) = mpsc::channel();
        let (release, held) = mpsc::channel();
        if rejected {
            assert!(
                control
                    .submit((entered, held), |(entered, held)| {
                        entered.send(()).unwrap();
                        held.recv_timeout(Duration::from_secs(5)).unwrap();
                    })
                    .is_ok()
            );
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let (send, receive) = mpsc::channel();
        let mut accounts = Accounts::new(provider, control.clone(), move |peer| {
            send.send((peer.identity().local.clone(), peer.remote().to_owned()))
                .unwrap();
        });
        let path = fixture.0.parent().unwrap().join("credentials.conf");
        let mut config = abi::AccountConfig {
            node_number: 7,
            local_node: abi::ByteSpan {
                data: b"524950".as_ptr(),
                length: 6,
            },
            ..abi::AccountConfig::default()
        };
        assert_eq!(
            unsafe { accounts.prepare(&config, &path) },
            Ok(()),
            "disabled accounts must not read a secrets file"
        );
        config.enabled = 1;
        assert_eq!(
            unsafe { accounts.prepare(&config, &path) },
            Err(Error::Secrets)
        );
        std::fs::write(&path, "[524950]\ndirectory_password=fixture-value\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(unsafe { accounts.prepare(&config, &path) }, Ok(()));
        assert_eq!(
            unsafe { accounts.prepare(&config, &path) },
            Err(Error::Contract)
        );
        assert!(accounts.account("524950").is_some());
        assert!(accounts.account("508422").is_none());
        assert_eq!(unsafe { accounts.activate() }, Ok(()));
        if rejected {
            release.send(()).unwrap();
        } else {
            assert_eq!(
                receive.recv_timeout(Duration::from_secs(5)).unwrap(),
                ("524950".into(), "4294967295".into())
            );
        }
        accounts.stop().unwrap();
        assert_eq!(
            unsafe { accounts.activate() },
            Err(Error::Status(abi::Status::Closed))
        );
        assert!(receive.try_recv().is_err());
        let count =
            unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_peers\0") }.unwrap();
        assert_eq!(
            unsafe { count() },
            0,
            "accepted or rejected incoming QSO leaked"
        );
        drop(accounts);
        drop(control);
        std::fs::remove_file(path).unwrap();
    }
}
