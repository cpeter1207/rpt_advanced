//! Actual shared host client, native Sofia account and an independent UDP registrar.
#[path = "support/sip_registrar.rs"]
mod registrar;

use rptadv_private_phone_host::sip_provider::{Provider, Settings};
use std::{
    net::UdpSocket,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn private_sip_account_registers_on_activation_and_retires_on_its_owner() {
    let path = std::env::var_os("RPTADV_SIP_TEST_PROVIDER")
        .map(PathBuf::from)
        .unwrap_or_else(|| "librptadv_sip_adapter.so.2".into());
    // SAFETY: the deployment/staged SDK selects this native provider.
    let provider = unsafe { Provider::load(&path) }.expect("load the real native SIP provider");
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let address = socket.local_addr().unwrap().to_string();
    let retained = Arc::clone(&provider);
    let registrar = thread::spawn(move || {
        let mut bytes = [0; 8192];
        let (initial, address) = registrar::receive_register(&socket, &mut bytes).unwrap();
        assert!(initial.starts_with("REGISTER "));
        socket
            .send_to(registrar::response(&initial, 401, true).as_bytes(), address)
            .unwrap();
        let (authenticated, peer) = registrar::receive_register(&socket, &mut bytes).unwrap();
        assert_eq!(peer, address);
        assert!(
            registrar::header(&authenticated, "Authorization").contains("username=\"synthetic\"")
        );
        socket
            .send_to(
                registrar::response(&authenticated, 200, false).as_bytes(),
                peer,
            )
            .unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(24)))
            .unwrap();
        let mut count = 0;
        let mut first_two = [None; 2];
        let (withdrawal, source) =
            registrar::receive_with_keepalives(&socket, &mut bytes, |source| {
                assert_eq!(source, peer);
                if let Some(time) = first_two.get_mut(count) {
                    *time = Some(Instant::now());
                }
                count += 1;
            })
            .unwrap();
        assert_eq!(source, peer);
        assert!(withdrawal.starts_with("REGISTER "));
        assert_eq!(registrar::header(&withdrawal, "Expires"), "0");
        assert_eq!(
            registrar::header(&withdrawal, "Call-ID"),
            registrar::header(&authenticated, "Call-ID")
        );
        socket
            .send_to(
                registrar::response(&withdrawal, 200, false).as_bytes(),
                peer,
            )
            .unwrap();
        (count, first_two)
    });
    // The outer provider lease survives the owning thread and its native TLS.
    thread::spawn(move || {
        let account = retained
            .prepare(&Settings {
                generation: 7,
                expires: 60,
                bind: "127.0.0.1:0",
                next_hop: &address,
                registrar: "sip:registrar.invalid",
                aor: "sip:synthetic@registrar.invalid",
                username: "synthetic",
                password: "test-only",
            })
            .unwrap();
        assert_eq!(account.status().unwrap().state, 0);
        account.activate().unwrap();
        let wait = |state| {
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                account.service().unwrap();
                let status = account.status().unwrap();
                assert_eq!(status.generation, 7);
                if status.state == state {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "account stalled at state {}",
                    status.state
                );
                thread::sleep(Duration::from_millis(5));
            }
        };
        let registered = wait(2);
        assert_eq!(registered.sip_status, 200);
        assert_eq!(registered.accepted_expires, 60);
        // Include the first periodic keepalive and stop before the next one is due.
        let observe_until = Instant::now() + Duration::from_secs(18);
        while Instant::now() < observe_until {
            account.service().unwrap();
            assert_eq!(account.status().unwrap().state, 2);
            thread::sleep(Duration::from_millis(5));
        }
        account.unregister().unwrap();
        assert_eq!(wait(4).sip_status, 200);
        account.shutdown().unwrap();
        assert_eq!(wait(7).sip_status, 200);
        account.destroy().unwrap();
    })
    .join()
    .unwrap();
    let (count, first_two) = registrar.join().unwrap();
    drop(provider);

    let gap = first_two[0]
        .zip(first_two[1])
        .map(|(first, next)| next - first);
    eprintln!(
        "SIP_KEEPALIVE_CLOSED options={} first_gap_ms={} registered_200=true withdrawal_200=true shutdown=true owners_joined=true",
        count,
        gap.map_or(0, |duration| duration.as_millis()),
    );
    assert!(
        count == 2
            && gap.is_some_and(
                |duration| (Duration::from_secs(12)..=Duration::from_secs(18)).contains(&duration)
            ),
        "expected an immediate OPTIONS and one periodic OPTIONS about 15 seconds later",
    );
}
