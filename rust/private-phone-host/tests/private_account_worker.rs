//! Actual shared frontend participant against an independent UDP registrar and the selected native DSOs.
#[path = "support/sip_digest.rs"]
mod digest;
#[path = "support/sip_registrar.rs"]
mod registrar;
use rptadv_private_phone_host::{
    phone::{AccountRow, PreparedAccount, Protocol},
    selection::{NativePaths, PhoneHost},
};
use std::{
    net::UdpSocket,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn shared_account_worker_requires_regack_and_withdraws_the_exact_authenticated_socket() {
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let remote = server.local_addr().unwrap();
    let path = |var: &str, default: &str| {
        std::env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from)
    };
    let owner = PhoneHost::new(
        NativePaths {
            sip: path("RPTADV_SIP_TEST_PROVIDER", "librptadv_sip_adapter.so.2"),
            jitter: path(
                "RPTADV_JITTER_TEST_PROVIDER",
                "librptadv_jitter_adapter.so.1",
            ),
            // A SIP-only candidate must never open an unrelated protocol.
            iax: "/does-not-exist/iax".into(),
            g711: "/does-not-exist/unused-iax-catalog".into(),
        },
        Arc::new(|_| panic!("unoffered incoming call")),
    );
    const PORT: [u8; 16] = [1; 16];
    const ACCOUNT: [u8; 16] = [2; 16];
    let account = |jitter_ms, credential: &[u8]| {
        PreparedAccount::copy(AccountRow {
            port_id: PORT,
            account_id: ACCOUNT,
            generation: 7,
            protocol: Protocol::Sip,
            server: "127.0.0.1",
            port: remote.port(),
            username: "desk",
            credential,
            jitter_ms,
        })
        .unwrap()
    };
    let row = |jitter_ms| account(jitter_ms, b"fixture-only");
    let ticket = owner.prepare(vec![row([20, 40, 100])]).unwrap();
    assert!(owner.observe(&PORT, &ACCOUNT, 7).is_err());
    assert!(!owner.drive(ticket, false).unwrap());
    let mut input = [0; 8192];
    let (initial, source) = registrar::receive_register(&server, &mut input).unwrap();
    assert!(owner.observe(&PORT, &ACCOUNT, 7).is_err());
    server
        .send_to(registrar::response(&initial, 401, true).as_bytes(), source)
        .unwrap();
    let (response, same_source) = registrar::receive_register(&server, &mut input).unwrap();
    assert_eq!(source, same_source);
    digest::check(registrar::header(&response, "Authorization"));
    server
        .send_to(
            registrar::response(&response, 200, false).as_bytes(),
            source,
        )
        .unwrap();
    let wait = |id| {
        let limit = Instant::now() + Duration::from_secs(4);
        while !owner.drive(id, false).unwrap() {
            assert!(
                Instant::now() < limit,
                "real account transition did not settle"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };
    wait(ticket);
    assert_eq!(owner.observe(&PORT, &ACCOUNT, 7).unwrap(), 2);
    assert!(owner.observe(&[3; 16], &ACCOUNT, 7).is_err());
    assert!(owner.observe(&PORT, &ACCOUNT, 8).is_err());
    owner.release(ticket).unwrap();
    assert!(
        owner
            .prepare(vec![account([20, 40, 100], b"other-fixture-secret")])
            .is_err()
    );
    assert_eq!(owner.observe(&PORT, &ACCOUNT, 7).unwrap(), 2);
    let retained = owner.prepare(vec![row([20, 40, 100])]).unwrap();
    wait(retained);
    owner.release(retained).unwrap();
    // A packet-buffer policy change has no registrar transaction. The independent
    // remote will acknowledge only the original contact and its final withdrawal.
    let changed = owner.prepare(vec![row([30, 85, 140])]).unwrap();
    wait(changed);
    assert_eq!(owner.observe(&PORT, &ACCOUNT, 7).unwrap(), 2);
    let limit = Instant::now() + Duration::from_secs(4);
    while !owner.drive(changed, true).unwrap() {
        assert!(
            Instant::now() < limit,
            "policy rollback waited on a spurious REGISTER"
        );
        thread::sleep(Duration::from_millis(5));
    }
    owner.release(changed).unwrap();
    assert_eq!(owner.observe(&PORT, &ACCOUNT, 7).unwrap(), 2);
    let withdraw = owner.prepare(vec![]).unwrap();
    assert!(!owner.drive(withdraw, false).unwrap());
    let (release, original) = registrar::receive_register(&server, &mut input).unwrap();
    assert_eq!(source, original);
    assert_eq!(registrar::header(&release, "Expires"), "0");
    server
        .send_to(registrar::response(&release, 200, false).as_bytes(), source)
        .unwrap();
    wait(withdraw);
    owner.release(withdraw).unwrap();
    assert!(owner.observe(&PORT, &ACCOUNT, 7).is_err());
    let limit = Instant::now() + Duration::from_secs(4);
    while !owner.stop().unwrap() {
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(5));
    }
}
