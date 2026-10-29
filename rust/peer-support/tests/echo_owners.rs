//! Shared host owners keep the provider/context alive until accounts are retired.
#![cfg(unix)]
use rptadv_peer_support::echolink::{abi, host::Provider};
use std::sync::atomic::{AtomicUsize, Ordering};
mod support;
#[test]
fn prepared_peer_retains_its_account_without_an_external_owner() {
    let fixture = support::Fixture::new("network");
    let peer = fixture.peer();
    assert_eq!(peer.revalidate(), Ok(()));
    drop(peer);
}

use support::Fixture;

#[test]
fn pending_handoff_handles_rejection_and_execution_before_submit_returns() {
    use rptadv_peer_support::echolink::host::Error;
    let fixture = Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &abi::AccountConfig {
                enabled: 1,
                local_node: abi::ByteSpan {
                    data: b"524950".as_ptr(),
                    length: 6,
                },
                ..abi::AccountConfig::default()
            },
            &abi::AccountSecrets {
                struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: abi::ByteSpan::default(),
                proxy_password: abi::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    let create =
        unsafe { library.get::<unsafe extern "C" fn() -> *mut abi::Qso>(b"fixture_peer\0") }
            .unwrap();
    let destroy =
        unsafe { library.get::<unsafe extern "C" fn(*mut abi::Qso)>(b"fixture_peer_destroy\0") }
            .unwrap();
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_peers\0") }.unwrap();
    for (accept, generation) in [(false, 9), (true, 8), (true, 9)] {
        let qso = unsafe { create() };
        let mut taken = 0;
        let event = abi::Event {
            struct_size: std::mem::size_of::<abi::Event>() as u32,
            kind: 1,
            qso,
            generation,
            status: abi::Status::Pending,
            taken: &mut taken,
        };
        let result = unsafe {
            account.dispatch_pending(&event, |pending| {
                if !accept {
                    return Err((Error::Status(abi::Status::Full), pending));
                }
                // Accepted control work may finish and release its payload before submit returns.
                let claimed = std::thread::spawn(move || pending.claim()).join().unwrap();
                if generation == 9 {
                    let peer = claimed.unwrap();
                    assert_eq!(peer.identity().source.to_string(), "127.0.0.2:49152");
                    drop(peer);
                } else {
                    assert!(
                        claimed.is_err(),
                        "the off-executor task must revalidate the exact generation"
                    );
                }
                Ok(())
            })
        };
        assert_eq!(taken, u32::from(accept));
        if accept {
            assert_eq!(result, Ok(()));
            assert_eq!(
                unsafe { count() },
                0,
                "accepted failure must retire its owned QSO"
            );
        } else {
            assert_eq!(result, Err(Error::Status(abi::Status::Full)));
            assert_eq!(
                unsafe { count() },
                1,
                "rejection must leave provider ownership intact"
            );
            unsafe { destroy(qso) };
        }
    }
}

#[test]
fn account_retains_context_after_the_host_releases_its_provider() {
    let fixture = Fixture::new("lifecycle");
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &abi::AccountConfig::default(),
            &abi::AccountSecrets {
                struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: abi::ByteSpan::default(),
                proxy_password: abi::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    drop(provider);
    assert_eq!(account.stop(), Ok(()));
    drop(account);
    // A second open also verifies balanced context/account destruction in the fixture.
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    drop(provider);
}

#[test]
fn incomplete_network_group_never_creates_a_context() {
    let fixture = Fixture::new("partial");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    assert!(matches!(
        unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) },
        Err(rptadv_peer_support::echolink::host::Error::Contract)
    ));
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_contexts\0") }.unwrap();
    assert_eq!(unsafe { count() }, 0);
}

#[test]
fn enabled_account_requires_complete_network_and_media_before_preparation() {
    let fixture = Fixture::new("lifecycle");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    assert!(matches!(
        unsafe {
            provider.prepare(
                &abi::AccountConfig {
                    enabled: 1,
                    ..abi::AccountConfig::default()
                },
                &abi::AccountSecrets {
                    struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                    abi_version: 1,
                    directory_password: abi::ByteSpan::default(),
                    proxy_password: abi::ByteSpan::default(),
                },
            )
        },
        Err(rptadv_peer_support::echolink::host::Error::Status(
            abi::Status::Unsupported
        ))
    ));
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_accounts\0") }.unwrap();
    assert_eq!(unsafe { count() }, 0);
}

#[test]
fn failed_account_stop_keeps_its_foreign_state_and_provider_alive() {
    let fixture = Fixture::new("lifecycle");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &abi::AccountConfig {
                node_number: 99,
                ..abi::AccountConfig::default()
            },
            &abi::AccountSecrets {
                struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: abi::ByteSpan::default(),
                proxy_password: abi::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    drop(provider);
    assert!(account.stop().is_err());
    drop(account);
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_contexts\0") }.unwrap();
    assert_eq!(
        unsafe { count() },
        1,
        "failed cleanup must retain the DSO/context owner"
    );
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_accounts\0") }.unwrap();
    assert_eq!(
        unsafe { count() },
        1,
        "failed cleanup must not destroy the foreign account"
    );
}

#[test]
fn incoming_ownership_is_claimed_only_after_fresh_provider_validation() {
    let fixture = Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &abi::AccountConfig {
                enabled: 1,
                local_node: abi::ByteSpan {
                    data: b"524950".as_ptr(),
                    length: 6,
                },
                ..abi::AccountConfig::default()
            },
            &abi::AccountSecrets {
                struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: abi::ByteSpan::default(),
                proxy_password: abi::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    let create =
        unsafe { library.get::<unsafe extern "C" fn() -> *mut abi::Qso>(b"fixture_peer\0") }
            .unwrap();
    let destroy =
        unsafe { library.get::<unsafe extern "C" fn(*mut abi::Qso)>(b"fixture_peer_destroy\0") }
            .unwrap();
    let expire =
        unsafe { library.get::<unsafe extern "C" fn(*mut abi::Qso)>(b"fixture_expire\0") }.unwrap();
    let mut claimed = None;
    for (generation, expired) in [(8, false), (9, true), (9, false)] {
        let qso = unsafe { create() };
        if expired {
            unsafe { expire(qso) };
        }
        let mut taken = 0;
        let event = abi::Event {
            struct_size: std::mem::size_of::<abi::Event>() as u32,
            kind: 1,
            qso,
            generation,
            status: abi::Status::Pending,
            taken: &mut taken,
        };
        let peer = unsafe { account.take_pending(&event) };
        if generation == 8 || expired {
            assert!(peer.is_err());
            assert_eq!(taken, 0, "rejected ownership remains with the provider");
            unsafe { destroy(qso) };
        } else {
            let peer = peer.unwrap();
            assert_eq!(taken, 1);
            assert_eq!(peer.identity().source.to_string(), "127.0.0.2:49152");
            assert_eq!(peer.identity().callsign, "W1XYZ-R");
            assert_eq!(peer.identity().node_number, u32::MAX);
            assert_eq!(
                peer.identity().host_capabilities(),
                23,
                "Echo must never gain ASL remote commands"
            );
            struct Trace {
                peer: std::sync::Arc<rptadv_peer_support::echolink::host::Peer>,
                calls: AtomicUsize,
                failures: AtomicUsize,
            }
            unsafe extern "C" fn step(context: *mut std::ffi::c_void) -> i32 {
                let trace = unsafe { &*context.cast::<Trace>() };
                if trace.peer.revalidate().is_err() {
                    trace.failures.fetch_add(1, Ordering::Relaxed);
                }
                trace.calls.fetch_add(1, Ordering::Relaxed);
                0
            }
            {
                let trace = Trace {
                    peer: std::sync::Arc::clone(&peer),
                    calls: AtomicUsize::new(0),
                    failures: AtomicUsize::new(0),
                };
                let registration = unsafe {
                    peer.attach(Some(step), std::ptr::from_ref(&trace).cast_mut().cast())
                }
                .unwrap();
                registration.wake();
                registration.stop();
                registration.wake();
                assert_eq!(trace.calls.load(Ordering::Relaxed), 2);
                assert_eq!(trace.failures.load(Ordering::Relaxed), 0);
                drop(registration);
                unsafe { expire(qso) };
                assert!(
                    unsafe {
                        peer.attach(Some(step), std::ptr::from_ref(&trace).cast_mut().cast())
                    }
                    .is_err(),
                    "final attach must revalidate, not trust the cached admission"
                );
            }
            claimed = Some(peer);
        }
    }
    drop(account);
    drop(provider);
    let contexts =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_contexts\0") }.unwrap();
    assert_eq!(
        unsafe { contexts() },
        1,
        "claimed QSO retains account/context/DSO"
    );
    drop(claimed);
    assert_eq!(unsafe { contexts() }, 0);
}
