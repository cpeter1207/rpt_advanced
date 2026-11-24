//! Both real management processes exercise the same authenticated partial-account selection.
use super::*;
use std::{io::ErrorKind, net::{SocketAddr, UdpSocket}};

#[path = "../../../../../../private-phone-host/tests/support/sip_digest.rs"]
mod digest;
#[path = "../../../../../../private-phone-host/tests/support/sip_registrar.rs"]
pub(super) mod registrar;

pub(super) fn register(socket: &UdpSocket) -> (String, SocketAddr) {
    let mut buffer = [0; 8192];
    let (first, source) = registrar::receive_register(socket, &mut buffer).unwrap();
    assert!(first.starts_with("REGISTER "));
    socket.send_to(registrar::response(&first, 401, true).as_bytes(), source).unwrap();
    let (second, authenticated) = registrar::receive_register(socket, &mut buffer).unwrap();
    assert_eq!(source, authenticated);
    digest::check(registrar::header(&second, "Authorization"));
    (second, authenticated)
}

fn probe_unpublished_call(socket: &UdpSocket, source: SocketAddr) {
    let media = UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = socket.local_addr().unwrap();
    let sdp = format!("v=0\r\no=probe 1 1 IN IP4 127.0.0.1\r\ns=probe\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=sendrecv\r\n",media.local_addr().unwrap().port());
    let request = format!("INVITE sip:desk@{source} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-early;rport\r\nMax-Forwards: 70\r\nFrom: <sip:probe@fixture.invalid>;tag=pending\r\nTo: <sip:desk@fixture.invalid>\r\nCall-ID: pending@fixture.invalid\r\nCSeq: 1 INVITE\r\nContact: <sip:probe@{local}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",sdp.len());
    socket.send_to(request.as_bytes(), source).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut bytes = [0; 8192];
    let to = loop {
        assert!(Instant::now() < deadline);
        socket.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now()))).unwrap();
        let (response, from) = registrar::receive_register(socket, &mut bytes).unwrap();
        assert_eq!(from, source);
        assert_eq!(registrar::header(&response, "Call-ID"), "pending@fixture.invalid");
        if response.starts_with("SIP/2.0 1") { continue; }
        assert!(response.starts_with("SIP/2.0 480 "), "an unpublished account must retire its unaccepted native INVITE: {}", response.lines().next().unwrap());
        break registrar::header(&response, "To").to_owned();
    };
    let ack = format!("ACK sip:desk@{source} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-early;rport\r\nMax-Forwards: 70\r\nFrom: <sip:probe@fixture.invalid>;tag=pending\r\nTo: {to}\r\nCall-ID: pending@fixture.invalid\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n");
    socket.send_to(ack.as_bytes(), source).unwrap();
    media.set_nonblocking(true).unwrap();
    assert_eq!(media.recv_from(&mut bytes).unwrap_err().kind(), ErrorKind::WouldBlock);
}

pub(super) fn check(scenario: &Scenario<'_>, client: &Client<'_>, process: &mut Foreground) {
    let sockets = ["call", "held"].map(|key| {
        let registrar = UdpSocket::bind("127.0.0.1:0").unwrap();
        let path = edit(client,"POST",PORTS,json!({"key":key,"configuration":{
            "name":key,"inherit_from":"site","enabled":false}})).headers["location"].clone();
        // A valid dormant selector reaches account selection. This unique fictitious
        // ALSA card must never open: the second registrar will refuse the candidate.
        put(client, &format!("{path}interface/"), json!({"audio":{"device_identifier":format!("hw:task48-{key}")}}));
        let resource = edit(client,"POST",&format!("{path}protocols/phone-patch/"),json!({"configuration":{
            "provider":{"protocol":"sip","server":"127.0.0.1","port":registrar.local_addr().unwrap().port(),"username":"desk"},
            "credentials":{"password":"fixture-only"}}}));
        assert!(!resource.body.to_string().contains("fixture-only"));
        registrar.set_nonblocking(true).unwrap();
        let mut byte = [0];
        assert_eq!(registrar.recv_from(&mut byte).unwrap_err().kind(), ErrorKind::WouldBlock,
            "a disabled port must not register a phone account");
        registrar.set_nonblocking(false).unwrap();
        registrar.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
        registrar
    });
    let previous = client.get("/api/v1/health/").body["status"]["active_revision"].clone();
    let ports = scenario.fixture.0.join("working/ports");
    let mut enabled = Vec::new();
    for item in fs::read_dir(ports).unwrap() {
        let file = item.unwrap().path().join("metadata.json");
        let mut metadata: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let key = metadata["key"].as_str().unwrap().to_owned();
        assert!(matches!(key.as_str(), "call"|"held"));
        metadata["configuration"]["enabled"] = json!(true);
        let relative = file.strip_prefix(&scenario.fixture.0).unwrap().to_str().unwrap();
        scenario.fixture.put(relative, &serde_json::to_vec(&metadata).unwrap());
        enabled.push(key);
    }
    enabled.sort();
    assert_eq!(enabled, ["call", "held"]);
    let deadline=Instant::now()+Duration::from_secs(12);
    while client.get(STORAGE).body["storage"]!="external_edit"{process.running(deadline);}
    thread::scope(|scope| {
        let upstream = scope.spawn(move || {
            let [first, hold] = sockets;
            let (accepted, a) = register(&first);
            let (waiting, b) = register(&hold);
            // Real native registration A now succeeds, while B cannot have confirmed.
            first.send_to(registrar::response(&accepted,200,false).as_bytes(),a).unwrap();
            probe_unpublished_call(&first,a);
            hold.send_to(registrar::response(&waiting,403,false).as_bytes(),b).unwrap();
            let mut bytes=[0;8192];
            first.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
            let (withdrawn, source)=registrar::receive_register(&first,&mut bytes).unwrap();
            assert_eq!(source,a);
            assert!(withdrawn.starts_with("REGISTER "));
            assert_eq!(registrar::header(&withdrawn,"Expires"),"0");
            first.send_to(registrar::response(&withdrawn,200,false).as_bytes(),source).unwrap();
        });
        let requested = recover(client, "reload_configuration", "desired");
        assert!((200..300).contains(&requested.code), "{}", requested.body);
        upstream.join().unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let health = client.get("/api/v1/health/").body;
        let state = &health["status"];
        if state["historical_activation"] == "failed" {
            assert_eq!(state["active_revision"], previous);
            assert_eq!(state["last_known_good"], previous);
            break;
        }
        process.running(deadline);
    }
}
