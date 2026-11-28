//! Drive Sofia's actual registrar and pending call queue from two distinct UDP sources.
use super::registration::Worker;
use crate::accounts::Route;
use crate::test_sip_registrar as registrar;
use crate::{
    phone::{AccountRow, PreparedAccount, Protocol},
    sip_provider::Provider,
};
use std::{
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn invite(socket: &UdpSocket, target: SocketAddr, id: &str) {
    let port = socket.local_addr().unwrap().port();
    let body = "v=0\r\no=remote 1 1 IN IP4 127.0.0.1\r\ns=test\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 9000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n";
    let packet = format!(
        "INVITE sip:desk@{target} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bK-{id};rport\r\nMax-Forwards: 70\r\nFrom: <sip:caller@fixture.invalid>;tag=caller-{id}\r\nTo: <sip:desk@{target}>\r\nCall-ID: phone-{id}@fixture.invalid\r\nCSeq: 1 INVITE\r\nContact: <sip:caller@127.0.0.1:{port}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    socket.send_to(packet.as_bytes(), target).unwrap();
}

#[test]
fn only_registered_transport_source_can_present_a_private_invite() {
    let provider_path = std::env::var_os("RPTADV_SIP_TEST_PROVIDER")
        .map(PathBuf::from)
        .unwrap_or_else(|| "librptadv_sip_adapter.so.2".into());
    let provider = unsafe { Provider::load(&provider_path) }.unwrap();
    let upstream = UdpSocket::bind("127.0.0.1:0").unwrap();
    let foreign = UdpSocket::bind("127.0.0.1:0").unwrap();
    for socket in [&upstream, &foreign] {
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
    }
    let peer = upstream.local_addr().unwrap();
    let account = Arc::new(
        PreparedAccount::copy(AccountRow {
            port_id: [2; 16],
            account_id: [3; 16],
            generation: 7,
            protocol: Protocol::Sip,
            server: "127.0.0.1",
            port: peer.port(),
            username: "desk",
            credential: b"fixture",
            jitter_ms: [20, 60, 120],
        })
        .unwrap(),
    );
    let route = Arc::new(Route::new(account, peer).unwrap());
    let mut worker = Worker::from_provider(provider).unwrap();
    worker.apply(vec![Arc::clone(&route)]).unwrap();
    let mut bytes = [0; 8192];
    let (initial, target) = registrar::receive_register(&upstream, &mut bytes).unwrap();
    upstream
        .send_to(registrar::response(&initial, 401, true).as_bytes(), target)
        .unwrap();
    let (authenticated, target) = registrar::receive_register(&upstream, &mut bytes).unwrap();
    assert!(registrar::header(&authenticated, "Authorization").contains("username=\"desk\""));
    upstream
        .send_to(
            registrar::response(&authenticated, 200, false).as_bytes(),
            target,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let (quiescent, rows) = worker.observe().unwrap();
        if quiescent
            && rows
                .iter()
                .any(|(_, status)| status.generation == 7 && status.state == 2)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "registration did not reach the captured generation"
        );
        thread::sleep(Duration::from_millis(5));
    }
    invite(&foreign, target, "foreign");
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        assert!(
            worker.incoming().is_none(),
            "foreign transport obtained a receipt"
        );
        let (count, source) = foreign.recv_from(&mut bytes).unwrap();
        assert_eq!(source, target);
        if bytes[..count].starts_with(b"SIP/2.0 403 ") {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    invite(&upstream, target, "valid");
    let deadline = Instant::now() + Duration::from_secs(4);
    let receipt = loop {
        if let Some(receipt) = worker.incoming() {
            break receipt;
        }
        assert!(
            Instant::now() < deadline,
            "registered transport was refused"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert!(Arc::ptr_eq(&route, &receipt.account));
    drop(receipt);
    let retirer = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        let (withdrawal, target) = loop {
            let (packet, target) = registrar::receive_register(&upstream, &mut bytes).unwrap();
            if packet.starts_with("REGISTER ") {
                break (packet, target);
            }
            assert!(Instant::now() < deadline);
        };
        assert_eq!(registrar::header(&withdrawal, "Expires"), "0");
        upstream
            .send_to(
                registrar::response(&withdrawal, 200, false).as_bytes(),
                target,
            )
            .unwrap();
    });
    worker.stop().unwrap();
    retirer.join().unwrap();
}
