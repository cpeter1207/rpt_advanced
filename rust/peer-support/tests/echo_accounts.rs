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
fn account_failures_are_logged_off_callback_once_and_old_generations_are_ignored() {
    let fixture = support::Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let event =
        unsafe { library.get::<unsafe extern "C" fn(u32, u64, u64)>(b"fixture_account_event\0") }
            .unwrap();
    let raw = unsafe { library.get::<unsafe extern "C" fn(u32)>(b"fixture_account_event_kind\0") }
        .unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let control = Arc::new(unsafe { Client::open(backend.cast(), "echo-notices", 8) }.unwrap());
    let (send, received) = mpsc::channel();
    let mut accounts =
        Accounts::new(provider, Arc::clone(&control), |_| {}).with_notice(move |text| {
            send.send(text.to_owned()).unwrap();
        });
    let path = fixture.0.parent().unwrap().join("credentials.conf");
    std::fs::write(&path, "[1000]\ndirectory_password=fixture-value\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = abi::AccountConfig {
        enabled: 1,
        local_node: abi::ByteSpan {
            data: b"1000".as_ptr(),
            length: 4,
        },
        ..Default::default()
    };
    unsafe { accounts.prepare(&config, &path) }.unwrap();
    unsafe { accounts.activate() }.unwrap();
    let drain = || {
        let (send, done) = mpsc::channel();
        assert!(control.submit(send, |send| send.send(()).unwrap()).is_ok());
        done.recv_timeout(Duration::from_secs(5)).unwrap();
    };
    drain();
    for kind in [u32::MAX, 2] {
        unsafe { raw(kind) };
        drain();
        assert!(received.try_recv().is_err());
    }
    unsafe { event(3, 4, 3) }; // A retired-generation event is not current evidence.
    drain();
    assert!(received.try_recv().is_err());
    unsafe { event(8, 4, 4) }; // Invalid foreign status cannot become a notice.
    drain();
    assert!(received.try_recv().is_err());
    let (entered, ready) = mpsc::channel();
    let (release, held) = mpsc::channel();
    assert!(
        control
            .submit((entered, held), |(entered, held)| {
                entered.send(()).unwrap();
                held.recv_timeout(Duration::from_secs(5)).unwrap();
            })
            .is_ok()
    );
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..3 {
        unsafe { event(3, 4, 4) };
    }
    release.send(()).unwrap();
    drain();
    let line = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(line.contains("state=retrying"), "{line}");
    assert!(!line.contains("fixture-value"));
    for _ in 0..3 {
        unsafe { event(3, 4, 4) };
        drain();
    }
    assert!(
        received.try_recv().is_err(),
        "repeat failure was not rate-limited"
    );
    unsafe { event(2, 4, 4) };
    drain();
    unsafe { event(3, 4, 4) };
    drain();
    assert!(
        received.recv_timeout(Duration::from_secs(5)).is_ok(),
        "healthy recovery must reset the failure episode"
    );
    accounts.stop().unwrap();
    unsafe { event(3, 4, 4) };
    assert!(received.try_recv().is_err());
}

#[test]
fn failed_replacement_retires_only_new_accounts_and_restores_the_product() {
    for (new_node, existing_node, rollback_failure) in
        [(3, 42, false), (42, 1, false), (99, 42, true)]
    {
        let fixture = support::Fixture::new("network");
        let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
        let count =
            unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_accounts\0") }
                .unwrap();
        let provider =
            unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
        let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let control =
            Arc::new(unsafe { Client::open(backend.cast(), "echo-failed-add-test", 8) }.unwrap());
        let mut accounts = Accounts::new(provider, control, |_| {});
        let path = fixture.0.parent().unwrap().join("credentials.conf");
        std::fs::write(
            &path,
            "[1000]\ndirectory_password=fixture-first\n[2000]\ndirectory_password=fixture-second\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = |local: &'static str, node_number| abi::AccountConfig {
            enabled: 1,
            node_number,
            local_node: abi::ByteSpan {
                data: local.as_ptr(),
                length: local.len(),
            },
            ..Default::default()
        };
        unsafe { accounts.prepare(&config("2000", 1), &path) }.unwrap();
        unsafe { accounts.activate() }.unwrap();
        let original = accounts.account("2000").unwrap();
        let mut update = accounts.prepare_update().unwrap();
        unsafe { update.prepare(&config("1000", new_node), &path) }.unwrap();
        unsafe { update.prepare(&config("2000", existing_node), &path) }.unwrap();
        let mut restored = false;
        let error = update
            .commit(
                || true,
                || {
                    restored = true;
                    true
                },
            )
            .unwrap_err();
        assert_eq!(error.cause, Error::Status(abi::Status::Full));
        assert_eq!(
            error.rollback,
            rollback_failure.then_some(Error::Status(abi::Status::Failed))
        );
        assert!(restored);
        assert!(accounts.account("1000").is_none());
        if rollback_failure {
            assert!(
                accounts.account("2000").is_none(),
                "failed rollback must gate admission"
            );
            assert!(accounts.observe("2000").unwrap().is_some());
        } else {
            assert!(Arc::ptr_eq(&original, &accounts.account("2000").unwrap()));
        }
        assert_eq!(
            unsafe { count() },
            if rollback_failure { 2 } else { 1 },
            "new callback owner survived rolled-back addition"
        );
        if rollback_failure {
            assert_eq!(accounts.stop(), Err(Error::Status(abi::Status::Failed)));
            drop(accounts);
            drop(original);
            assert_eq!(
                unsafe { count() },
                2,
                "failed quiescence must retain the whole callback set"
            );
        } else {
            accounts.stop().unwrap();
        }
    }
}

#[test]
fn reload_adds_disables_and_removes_accounts_without_replacing_the_control_owner() {
    let fixture = support::Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_accounts\0") }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let control =
        Arc::new(unsafe { Client::open(backend.cast(), "echo-replacement-test", 8) }.unwrap());
    let mut accounts = Accounts::new(provider, control.clone(), |_| {});
    let path = fixture.0.parent().unwrap().join("credentials.conf");
    std::fs::write(
        &path,
        "[1000]\ndirectory_password=fixture-first\n[2000]\ndirectory_password=fixture-second\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = |local: &'static str, enabled, node_number| abi::AccountConfig {
        enabled,
        node_number,
        local_node: abi::ByteSpan {
            data: local.as_ptr(),
            length: local.len(),
        },
        ..Default::default()
    };
    unsafe { accounts.prepare(&config("1000", 1, 1), &path) }.unwrap();
    unsafe { accounts.activate() }.unwrap();
    {
        let mut update = accounts.prepare_update().unwrap();
        unsafe { update.prepare(&config("1000", 1, 1), &path) }.unwrap();
        unsafe { update.prepare(&config("2000", 1, 3), &path) }.unwrap();
        unsafe { update.prepare(&config("3000", 0, 0), &path) }.unwrap(); // No secrets for disabled record.
        update.remove_unlisted().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(
            update
                .commit(|| true, || true)
                .unwrap()
                .retirement
                .is_none()
        );
    }
    assert!(accounts.account("1000").is_some());
    assert!(accounts.account("2000").is_some());
    assert!(accounts.account("3000").is_none());
    assert_eq!(
        unsafe { count() },
        2,
        "inactive snapshots were not released"
    );
    let mut update = accounts.prepare_update().unwrap();
    unsafe { update.prepare(&config("2000", 0, 0), &path) }.unwrap();
    update.remove_unlisted().unwrap();
    assert!(
        update
            .commit(|| true, || true)
            .unwrap()
            .retirement
            .is_none()
    );
    assert!(accounts.account("1000").is_none());
    assert!(accounts.account("2000").is_none());
    assert!(
        accounts.observe("2000").unwrap().is_some(),
        "disabled accounts remain diagnosable without reopening admission"
    );
    assert!(
        accounts.observe("1000").unwrap().is_none(),
        "removed accounts are not retained by diagnostics"
    );
    assert_eq!(
        unsafe { count() },
        1,
        "removed foreign account was not released"
    );
    let (sent, received) = mpsc::channel();
    assert!(
        control
            .submit(sent, |sent| {
                sent.send(()).unwrap();
            })
            .is_ok()
    );
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut remove_all = accounts.prepare_update().unwrap();
    remove_all.remove_unlisted().unwrap();
    let fault =
        unsafe { library.get::<unsafe extern "C" fn(u32)>(b"fixture_disabled_stop_failure\0") }
            .unwrap();
    unsafe { fault(1) };
    assert_eq!(
        remove_all.commit(|| true, || true).unwrap().retirement,
        Some(Error::Status(abi::Status::Failed))
    );
    assert!(accounts.account("2000").is_none());
    assert!(accounts.observe("2000").unwrap().is_none());
    assert_eq!(
        unsafe { count() },
        1,
        "failed retirement must retain callback storage"
    );
    unsafe { fault(0) };
    accounts.stop().unwrap();
    drop(accounts);
    assert_eq!(
        unsafe { count() },
        0,
        "successful retry must release the retained owner"
    );
}

#[test]
fn queued_incoming_is_revalidated_against_the_reload_admission_gate() {
    for reloading in [false, true] {
        let fixture = support::Fixture::new("network");
        let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
        let event = unsafe {
            library.get::<unsafe extern "C" fn(u32, u64, u64)>(b"fixture_account_event\0")
        }
        .unwrap();
        let provider =
            unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
        let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let control =
            Arc::new(unsafe { Client::open(backend.cast(), "echo-gated-test", 8) }.unwrap());
        let (entered, ready) = mpsc::channel();
        let (release, held) = mpsc::channel();
        assert!(
            control
                .submit((entered, held), |(entered, held)| {
                    entered.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(5)).unwrap();
                })
                .is_ok()
        );
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let (incoming, received) = mpsc::channel();
        let (notice, notices) = mpsc::channel();
        let mut accounts = Accounts::new(provider, control.clone(), move |_| {
            incoming.send(()).unwrap();
        })
        .with_notice(move |line| notice.send(line.to_owned()).unwrap());
        let path = fixture.0.parent().unwrap().join("credentials.conf");
        std::fs::write(
        &path,
        "[524950]\ndirectory_password=fixture-first\n[508422]\ndirectory_password=fixture-second\n",
    )
    .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut config = abi::AccountConfig {
            enabled: 1,
            node_number: 7,
            local_node: abi::ByteSpan {
                data: b"524950".as_ptr(),
                length: 6,
            },
            ..Default::default()
        };
        unsafe { accounts.prepare(&config, &path) }.unwrap();
        unsafe {
            accounts.prepare(
                &abi::AccountConfig {
                    enabled: 1,
                    node_number: if reloading { 7 } else { 1 },
                    local_node: abi::ByteSpan {
                        data: b"508422".as_ptr(),
                        length: 6,
                    },
                    ..Default::default()
                },
                &path,
            )
        }
        .unwrap();
        unsafe { accounts.activate() }.unwrap(); // Queued behind the held control task.
        if !reloading {
            accounts.account("524950").unwrap().stop().unwrap();
            release.send(()).unwrap();
            let (barrier, joined) = mpsc::channel();
            assert!(
                control
                    .submit(barrier, |barrier| barrier.send(()).unwrap())
                    .is_ok()
            );
            joined.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(
                received.try_recv().is_err(),
                "retired account admitted a queued QSO"
            );
            let count =
                unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_peers\0") }
                    .unwrap();
            assert_eq!(unsafe { count() }, 0, "failed pending claim leaked its QSO");
            accounts.stop().unwrap();
            continue;
        }
        unsafe { event(3, 4, 4) }; // Queued notices would log if reload gating failed.
        config.node_number = 8;
        let mut update = accounts.prepare_update().unwrap();
        unsafe { update.prepare(&config, &path) }.unwrap();
        update
            .commit(
                || {
                    release.send(()).unwrap();
                    let (barrier, joined) = mpsc::channel();
                    assert!(
                        control
                            .submit(barrier, |barrier| {
                                barrier.send(()).unwrap();
                            })
                            .is_ok()
                    );
                    joined.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(
                        received.try_recv().is_err(),
                        "queued QSO entered a half-reloaded product"
                    );
                    assert!(
                        notices.try_recv().is_err(),
                        "queued notice entered a half-reloaded product"
                    );
                    true
                },
                || true,
            )
            .unwrap();
        accounts.stop().unwrap();
    }
}

#[test]
fn prepared_updates_do_not_stop_admission_and_roll_back_before_reopening_it() {
    let fixture = support::Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_update_count\0") }
            .unwrap();
    let value =
        unsafe { library.get::<unsafe extern "C" fn(usize) -> u64>(b"fixture_update\0") }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let control = Arc::new(unsafe { Client::open(backend.cast(), "echo-update-test", 8) }.unwrap());
    let mut accounts = Accounts::new(provider, control.clone(), |_| {});
    let path = fixture.0.parent().unwrap().join("credentials.conf");
    let write_keys = || {
        std::fs::write(
            &path,
            "[1000]\ndirectory_password=fixture-first\n[2000]\ndirectory_password=fixture-second\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    let config = |local: &'static str, node_number| abi::AccountConfig {
        enabled: 1,
        node_number,
        local_node: abi::ByteSpan {
            data: local.as_ptr(),
            length: local.len(),
        },
        ..Default::default()
    };
    write_keys();
    unsafe { accounts.prepare(&config("1000", 1), &path) }.unwrap();
    unsafe { accounts.prepare(&config("2000", 3), &path) }.unwrap();
    unsafe { accounts.activate() }.unwrap();
    let original = accounts.account("1000").unwrap();
    {
        let mut discarded = accounts.prepare_update().unwrap();
        unsafe { discarded.prepare(&config("1000", 2), &path) }.unwrap();
        assert_eq!(
            unsafe { discarded.prepare(&config("1000", 2), &path) },
            Err(Error::Contract)
        );
    }
    let (sent, received) = mpsc::channel();
    assert!(
        control
            .submit(sent, |sent| {
                sent.send(()).unwrap();
            })
            .is_ok()
    );
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(unsafe { count() }, 0);

    let mut rejected = accounts.prepare_update().unwrap();
    unsafe { rejected.prepare(&config("1000", 2), &path) }.unwrap();
    let mut restored_rejection = false;
    let error = rejected
        .commit(
            || {
                assert!(accounts.account("1000").is_none());
                false
            },
            || {
                restored_rejection = true;
                true
            },
        )
        .unwrap_err();
    assert_eq!(error.cause, Error::Product);
    assert!(restored_rejection);
    assert!(error.rollback.is_none());
    assert!(accounts.account("1000").is_some());
    assert_eq!(unsafe { count() }, 0);

    let mut failed = accounts.prepare_update().unwrap();
    unsafe { failed.prepare(&config("1000", 2), &path) }.unwrap();
    unsafe { failed.prepare(&config("2000", 42), &path) }.unwrap();
    std::fs::remove_file(&path).unwrap();
    let mut restored = false;
    let error = failed
        .commit(
            || {
                assert!(accounts.account("1000").is_none());
                true
            },
            || {
                assert!(accounts.account("1000").is_none());
                restored = true;
                true
            },
        )
        .unwrap_err();
    assert!(restored);
    assert_eq!(error.cause, Error::Status(abi::Status::Full));
    assert!(error.rollback.is_none());
    assert!(Arc::ptr_eq(&original, &accounts.account("1000").unwrap()));
    assert_eq!(
        (0..unsafe { count() })
            .map(|index| unsafe { value(index) })
            .collect::<Vec<_>>(),
        [(1_u64 << 32) | 2, (3_u64 << 32) | 42, (2_u64 << 32) | 1]
    );

    write_keys();
    let mut stale = accounts.prepare_update().unwrap();
    unsafe { stale.prepare(&config("1000", 4), &path) }.unwrap();
    let mut current = accounts.prepare_update().unwrap();
    unsafe { current.prepare(&config("1000", 2), &path) }.unwrap();
    std::fs::remove_file(&path).unwrap();
    current
        .commit(|| true, || panic!("successful commit cannot roll back"))
        .unwrap();
    let error = stale
        .commit(
            || panic!("stale configuration cannot change product"),
            || true,
        )
        .unwrap_err();
    assert_eq!(error.cause, Error::Contract);
    write_keys();
    for foreign_failure in [false, true] {
        let mut rejected = accounts.prepare_update().unwrap();
        unsafe { rejected.prepare(&config("1000", 2), &path) }.unwrap();
        unsafe { rejected.prepare(&config("2000", if foreign_failure { 42 } else { 3 }), &path) }
            .unwrap();
        let error = rejected.commit(|| foreign_failure, || false).unwrap_err();
        assert_eq!(
            error.cause,
            if foreign_failure {
                Error::Status(abi::Status::Full)
            } else {
                Error::Product
            }
        );
        assert_eq!(error.rollback, Some(Error::Product));
        assert!(accounts.account("1000").is_none());
        assert!(accounts.observe("1000").unwrap().is_some());
        // An explicit successful reload can recover a previously gated owner.
        let mut recovered = accounts.prepare_update().unwrap();
        unsafe { recovered.prepare(&config("1000", 2), &path) }.unwrap();
        unsafe { recovered.prepare(&config("2000", 3), &path) }.unwrap();
        recovered.commit(|| true, || true).unwrap();
        assert!(accounts.account("1000").is_some());
    }
    let mut stopped = accounts.prepare_update().unwrap();
    unsafe { stopped.prepare(&config("1000", 4), &path) }.unwrap();
    accounts.stop().unwrap();
    let error = stopped
        .commit(
            || panic!("stopped accounts cannot change product"),
            || panic!("stopped accounts did not change product"),
        )
        .unwrap_err();
    assert_eq!(error.cause, Error::Status(abi::Status::Closed));
}

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
    std::fs::write(
        &path,
        "[524950]\ndirectory_password=fixture-value\n[508422]\ndirectory_password=fixture-second\n",
    )
    .unwrap();
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
    let second = abi::AccountConfig {
        node_number: 1,
        local_node: abi::ByteSpan {
            data: b"508422".as_ptr(),
            length: 6,
        },
        ..config
    };
    unsafe { candidate.prepare(&second, None) }.unwrap();
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
    drop(accounts);
    for account_builder in [false, true] {
        let mut candidate = unsafe {
            Preparation::new(
                &configuration,
                &fixture.0,
                &resources,
                backend.cast(),
                |_| {},
            )
        }
        .with_notice(|_| {});
        unsafe { candidate.prepare(&second, None) }.unwrap();
        let late_logger = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if account_builder {
                drop(candidate.finish().unwrap().with_notice(|_| {}));
            } else {
                drop(candidate.with_notice(|_| {}));
            }
        }));
        assert!(
            late_logger.is_err(),
            "logger replacement cannot bypass prepared callback ownership"
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn reload_preparation_lazily_adds_the_first_account_and_reuses_its_owner() {
    let fixture = support::Fixture::new("network");
    let directory = fixture.0.parent().unwrap();
    let configuration = directory.join("rpt_advanced.conf");
    let resources = abi::RuntimeConfig::default();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let preparation = || unsafe {
        Preparation::new(
            &configuration,
            &fixture.0,
            &resources,
            backend.cast(),
            |_| {},
        )
    };
    let disabled = unsafe {
        preparation().reload(
            None,
            |visit| visit(&abi::AccountConfig::default(), None),
            || true,
            || panic!("successful product reload cannot restore"),
        )
    }
    .unwrap();
    assert!(disabled.0.is_none());
    for restored in [true, false] {
        let failure = unsafe {
            preparation().reload(
                None,
                |visit| visit(&abi::AccountConfig::default(), None),
                || false,
                || restored,
            )
        }
        .err()
        .unwrap();
        assert_eq!(failure.cause, Error::Product);
        assert_eq!(failure.rollback, (!restored).then_some(Error::Product));
    }
    let path = directory.join("echolink-secrets.conf");
    std::fs::write(&path, "[524950]\ndirectory_password=fixture-value\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = abi::AccountConfig {
        enabled: 1,
        node_number: 1,
        local_node: abi::ByteSpan {
            data: b"524950".as_ptr(),
            length: 6,
        },
        ..Default::default()
    };
    config.node_number = 42; // The existing provider fixture rejects activation.
    for restored in [true, false] {
        let error = unsafe {
            preparation().reload(None, |visit| visit(&config, None), || true, || restored)
        }
        .err()
        .unwrap();
        assert_eq!(error.cause, Error::Status(abi::Status::Full));
        assert_eq!(error.rollback, (!restored).then_some(Error::Product));
    }
    config.node_number = 1;
    let owner =
        unsafe { preparation().reload(None, |visit| visit(&config, None), || true, || true) }
            .unwrap()
            .0
            .unwrap();
    assert!(owner.account("524950").is_some());
    config.enabled = 0;
    std::fs::remove_file(path).unwrap();
    let same = unsafe {
        preparation().reload(
            Some(owner.clone()),
            |visit| visit(&config, None),
            || true,
            || true,
        )
    }
    .unwrap()
    .0
    .unwrap();
    assert!(Arc::ptr_eq(&owner, &same));
    assert!(owner.account("524950").is_none());
    owner.stop().unwrap();
}

#[test]
fn accounts_prepare_private_credentials_then_handoff_or_reject_without_leaking() {
    unsafe extern "C" fn reject_backend(
        _: *mut std::ffi::c_void,
        _: rptadv_peer_support::control::Task,
    ) -> i32 {
        -1
    }
    for mode in 0..5 {
        let rejected = mode != 0;
        let fixture = support::Fixture::new("network");
        let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
        let provider =
            unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
        let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let mut backend = unsafe { *backend.cast::<rptadv_peer_support::control::Descriptor>() };
        if mode == 3 {
            backend.submit = Some(reject_backend);
        }
        let control =
            Arc::new(unsafe { Client::open(&backend, "echo-admission-test", 1) }.unwrap());
        if mode == 2 {
            control.stop_and_drain().unwrap();
        }
        let (entered, ready) = mpsc::channel();
        let (release, held) = mpsc::channel();
        if mode == 1 {
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
        if rejected {
            accounts = accounts.with_notice(|_| {});
        }
        let path = fixture.0.parent().unwrap().join("credentials.conf");
        let mut config = abi::AccountConfig {
            node_number: 7,
            local_node: abi::ByteSpan {
                data: b"524950".as_ptr(),
                length: 6,
            },
            ..abi::AccountConfig::default()
        };
        if mode == 4 {
            accounts.stop().unwrap();
            assert_eq!(
                unsafe { accounts.prepare(&config, &path) },
                Err(Error::Status(abi::Status::Closed)),
                "stop before activation must also prevent preparation"
            );
            continue;
        }
        assert!(matches!(
            accounts.prepare_update(),
            Err(Error::Status(abi::Status::Closed))
        ));
        let saved = config.local_node;
        for (enabled, local) in [(2, "524950"), (1, ""), (1, "52x")] {
            config.enabled = enabled;
            config.local_node = abi::ByteSpan {
                data: local.as_ptr(),
                length: local.len(),
            };
            assert_eq!(
                unsafe { accounts.prepare(&config, &path) },
                Err(Error::Contract)
            );
        }
        config.enabled = 0;
        config.local_node = saved;
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
        assert_eq!(unsafe { accounts.activate() }, Err(Error::Contract));
        assert_eq!(
            unsafe { accounts.prepare(&config, &path) },
            Err(Error::Status(abi::Status::Closed))
        );
        if mode == 1 {
            release.send(()).unwrap();
        } else if !rejected {
            assert_eq!(
                receive.recv_timeout(Duration::from_secs(5)).unwrap(),
                ("524950".into(), "4294967295".into())
            );
        }
        accounts.stop().unwrap();
        assert!(accounts.account("524950").is_none());
        assert!(matches!(
            accounts.prepare_update(),
            Err(Error::Status(abi::Status::Closed))
        ));
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
