//! Exercise admission through an actual rustls client and TCP connection.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rptadv_peer_support::https::{
    CoordinatorInbox, Listener, OriginPolicy, ServerIdentity, ServiceId, ServiceTrust,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{
    Message, WebSocket,
    protocol::{Role, frame::coding::CloseCode},
};

use rptadv_peer_support::https::StatusStream;

struct Pki(PathBuf);

impl Pki {
    fn new() -> Self {
        let dir = Command::new("mktemp").arg("-d").output().unwrap();
        assert!(dir.status.success());
        let pki = Self(PathBuf::from(String::from_utf8(dir.stdout).unwrap().trim()));
        let output = Command::new("sh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/https-pki.sh"))
            .arg(&pki.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "test PKI creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        pki
    }

    fn cert(&self, name: &str) -> CertificateDer<'static> {
        CertificateDer::from_pem_file(self.0.join(format!("{name}.pem"))).unwrap()
    }

    fn key(&self, name: &str) -> PrivateKeyDer<'static> {
        PrivateKeyDer::from_pem_file(self.0.join(format!("{name}.key"))).unwrap()
    }

    fn trust(&self, ca: &str, names: &[(&str, &str)]) -> ServiceTrust {
        let pins = names
            .iter()
            .map(|(name, actor)| {
                (
                    ServiceTrust::leaf_pin(&self.cert(name)),
                    ServiceId::new(actor).unwrap(),
                )
            })
            .collect();
        ServiceTrust::new(vec![self.cert(ca)], pins).unwrap()
    }

    fn start(&self, trust: ServiceTrust) -> (Listener, CoordinatorInbox) {
        Listener::start(
            "127.0.0.1:0".parse().unwrap(),
            ServerIdentity::new(vec![self.cert("server")], self.key("server")),
            trust,
            OriginPolicy::new(
                vec!["localhost".into()],
                vec!["https://console.local".into()],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn client(&self, ca: &str, name: Option<&str>) -> Arc<ClientConfig> {
        let mut roots = RootCertStore::empty();
        roots.add(self.cert(ca)).unwrap();
        let builder =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots);
        Arc::new(match name {
            Some(name) => builder
                .with_client_auth_cert(vec![self.cert(name)], self.key(name))
                .unwrap(),
            None => builder.with_no_client_auth(),
        })
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

type Client = BufReader<StreamOwned<ClientConnection, TcpStream>>;

fn connect(addr: SocketAddr, config: Arc<ClientConfig>, name: &'static str) -> Client {
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(4))).unwrap();
    let conn = ClientConnection::new(config, ServerName::try_from(name).unwrap()).unwrap();
    BufReader::new(StreamOwned::new(conn, tcp))
}

const GET: &[u8] = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";

fn send(client: &mut Client, request: &[u8]) -> io::Result<()> {
    client.get_mut().write_all(request)?;
    client.get_mut().flush()
}

fn status(client: &mut Client) -> io::Result<u16> {
    let mut line = String::new();
    client.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .ok_or(io::ErrorKind::InvalidData)?;
    loop {
        line.clear();
        if client.read_line(&mut line)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if line == "\r\n" {
            return Ok(status);
        }
    }
}

fn claim(inbox: &mut CoordinatorInbox) -> rptadv_peer_support::https::AdmittedRequest {
    let start = Instant::now();
    loop {
        if let Some(request) = inbox.try_recv() {
            return request;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "no authenticated request admitted"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn invalid_status_stream_upgrades_are_rejected_before_claim() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let config = pki.client("backend", Some("cli"));
    for (version, protocol, origin, expected) in [
        ("12", "rptadv.status.v1", "https://console.local", 400),
        ("13", "other", "https://console.local", 400),
        ("13", "rptadv.status.v1", "https://other.local", 403),
    ] {
        let mut conn = connect(listener.local_addr(), config.clone(), "localhost");
        let request = format!(
            "GET /api/v1/status/stream/ HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\n\
             Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: {version}\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
        );
        send(&mut conn, request.as_bytes()).unwrap();
        assert_eq!(status(&mut conn).unwrap(), expected);
        assert!(inbox.try_recv().is_none());
    }
}

type StatusSocket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

fn send_status(client: &mut Client, host: &str, other: &str) {
    let request = format!(
        "GET /api/v1/status/stream/?port=example HTTP/1.1\r\nHost: {host}\r\n{other}\
         Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: rptadv.status.v1\r\n\r\n"
    );
    send(client, request.as_bytes()).unwrap();
}

fn status_socket(mut client: Client) -> StatusSocket {
    let mut line = String::new();
    client.read_line(&mut line).unwrap();
    assert_eq!(line.split_whitespace().nth(1), Some("101"));
    let mut found = false;
    let mut negotiated = false;
    loop {
        line.clear();
        assert!(client.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("sec-websocket-accept") {
            assert_eq!(value.trim(), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
            found = true;
        }
        if name.eq_ignore_ascii_case("sec-websocket-protocol") {
            assert_eq!(value.trim(), "rptadv.status.v1");
            negotiated = true;
        }
    }
    assert!(found && negotiated);
    let initial = client.buffer().to_vec();
    WebSocket::from_partially_read(client.into_inner(), initial, Role::Client, None)
}

fn status_frame(client: &mut StatusSocket) -> Value {
    serde_json::from_str(client.read().unwrap().to_text().unwrap()).unwrap()
}

fn expect_close(client: &mut StatusSocket, expected: CloseCode) {
    let Message::Close(Some(close)) = client.read().unwrap() else {
        panic!("expected a WebSocket Close frame");
    };
    assert_eq!(close.code, expected);
}

#[test]
fn status_stream_orders_snapshot_pages_state_and_latest_meters() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let mut conn = connect(
        listener.local_addr(),
        pki.client("backend", Some("cli")),
        "localhost",
    );
    send_status(&mut conn, "localhost", "");
    let admitted = claim(&mut inbox);
    assert!(admitted.is_status_upgrade());
    assert_eq!(admitted.actor.as_str(), "cli");
    assert_eq!(admitted.target.query(), Some("port=example"));
    let page =
        |index, complete| json!({"state_cursor":"20","page_index":index,"complete":complete});
    let (publisher, mut stream) = StatusStream::new("boot".into(), Vec::new(), Some(10));
    assert!(publisher.event(Arc::new(json!({"state_cursor":"21","kind":"rx.keyed"}))));
    assert!(publisher.event(Arc::new(json!({"state_cursor":"22","kind":"rx.unkeyed"}))));
    publisher.meters(Arc::from([Arc::new(
        json!({"epoch":"1","peak_dbfs":-12.0}),
    )]));
    publisher.invalidate_meters();
    publisher.meters(Arc::from([Arc::new(json!({"epoch":"2","peak_dbfs":-6.0}))]));
    publisher.meters(Arc::from([Arc::new(json!({"epoch":"2","peak_dbfs":-3.0}))]));
    let pages = vec![page(0, false), page(1, true)];
    stream.set_snapshot_pages(pages.clone());
    admitted.respond_status_stream(stream);

    let mut socket = status_socket(conn);
    for (idx, expected) in pages.into_iter().enumerate() {
        assert_eq!(
            status_frame(&mut socket),
            json!({"type":"snapshot","boot_id":"boot","seq":(idx+1).to_string(),"data":expected})
        );
    }
    assert_eq!(
        status_frame(&mut socket),
        json!({"type":"event","boot_id":"boot","seq":"3","state_cursor":"21","kind":"rx.keyed"})
    );
    assert_eq!(
        status_frame(&mut socket),
        json!({"type":"event","boot_id":"boot","seq":"4","state_cursor":"22","kind":"rx.unkeyed"})
    );
    assert_eq!(
        status_frame(&mut socket),
        json!({"type":"meters","boot_id":"boot","seq":"5","superseded_meters":"1",
               "data":[{"epoch":"2","peak_dbfs":-3.0}]})
    );
    socket
        .send(Message::Ping(b"through-mtls".to_vec().into()))
        .unwrap();
    assert_eq!(
        socket.read().unwrap(),
        Message::Pong(b"through-mtls".to_vec().into())
    );
    socket
        .send(Message::text(r#"{"action":"radio.key"}"#))
        .unwrap();
    expect_close(&mut socket, CloseCode::Policy);
    assert!(
        inbox.try_recv().is_none(),
        "application frame reached the coordinator"
    );
}

#[test]
fn status_stream_divides_large_meter_batches_without_resync() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let mut connection = connect(
        listener.local_addr(),
        pki.client("backend", Some("cli")),
        "localhost",
    );
    send_status(&mut connection, "localhost", "");
    let admitted = claim(&mut inbox);
    let (publisher, stream) =
        StatusStream::new("boot".into(), vec![json!({"complete":true})], Some(20));
    publisher.meters(Arc::from([
        Arc::new(json!({"signal":"a","fixture":"a".repeat(2 * 1024 * 1024)})),
        Arc::new(json!({"signal":"b","fixture":"b".repeat(2 * 1024 * 1024 + 8192)})),
    ]));
    admitted.respond_status_stream(stream);

    let mut socket = status_socket(connection);
    assert_eq!(status_frame(&mut socket)["type"], "snapshot");
    for (seq, signal) in [("2", "a"), ("3", "b")] {
        let message = socket.read().unwrap();
        let text = message.to_text().unwrap();
        assert!(text.len() <= 4 * 1024 * 1024);
        let frame: Value = serde_json::from_str(text).unwrap();
        assert_eq!(frame["type"], "meters");
        assert_eq!(frame["seq"], seq);
        assert_eq!(frame["data"].as_array().unwrap().len(), 1);
        assert_eq!(frame["data"][0]["signal"], signal);
    }
}

#[test]
fn status_stream_reports_overflow_while_regular_get_remains_available() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let config = pki.client("backend", Some("cli"));
    let mut slow = connect(listener.local_addr(), config.clone(), "localhost");
    send_status(&mut slow, "localhost", "");
    let admitted = claim(&mut inbox);
    let (publisher, stream) = StatusStream::new(
        "boot".into(),
        vec![json!({"complete":true,"fixture":"x".repeat(4 * 1024 * 1024 - 100)})],
        None,
    );
    admitted.respond_status_stream(stream);
    let mut slow = status_socket(slow);
    let event = Arc::new(json!({"kind":"test"}));
    assert!((0..2048).any(|_| !publisher.event(event.clone())));

    let mut fast = connect(listener.local_addr(), config, "localhost");
    send(&mut fast, GET).unwrap();
    claim(&mut inbox).respond(
        hyper::Response::builder()
            .status(204)
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(status(&mut fast).unwrap(), 204);

    assert_eq!(status_frame(&mut slow)["type"], "snapshot");
    assert_eq!(
        status_frame(&mut slow),
        json!({"type":"resync_required","boot_id":"boot","seq":"2","reason":"state_overflow"})
    );
    expect_close(&mut slow, CloseCode::Again);
}

#[test]
fn status_stream_viewers_do_not_consume_regular_http_capacity() {
    const VIEWERS: usize = 24;
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let config = pki.client("backend", Some("cli"));
    let connect_local = || connect(listener.local_addr(), config.clone(), "localhost");
    let open = |inbox: &mut CoordinatorInbox| {
        let mut conn = connect_local();
        send_status(&mut conn, "localhost", "");
        let request = claim(inbox);
        let proof = request.connection.clone();
        let (publisher, stream) =
            StatusStream::new("boot".into(), vec![json!({"complete":true})], None);
        request.respond_status_stream(stream);
        let mut socket = status_socket(conn);
        assert_eq!(status_frame(&mut socket)["type"], "snapshot");
        (socket, publisher, proof)
    };
    let mut viewers = (0..VIEWERS).map(|_| open(&mut inbox)).collect::<Vec<_>>();

    let mut http = connect_local();
    send(&mut http, GET).unwrap();
    let ordinary = claim(&mut inbox);
    assert!(!ordinary.is_status_upgrade());
    ordinary.respond(
        hyper::Response::builder()
            .status(204)
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(status(&mut http).unwrap(), 204);
    assert!(viewers.iter().all(|(_, _, proof)| proof.is_alive()));

    let (mut closed, publisher, proof) = viewers.pop().unwrap();
    closed.close(None).unwrap();
    assert!(closed.read().unwrap().is_close());
    drop(closed);
    let deadline = Instant::now() + Duration::from_secs(2);
    while proof.is_alive() {
        assert!(Instant::now() < deadline, "closed viewer was not drained");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!publisher.is_open());
    viewers.push(open(&mut inbox));
    drop(listener);
    for (mut socket, publisher, proof) in viewers {
        expect_close(&mut socket, CloseCode::Away);
        assert!(!publisher.is_open());
        assert!(!proof.is_alive());
    }
}

#[test]
fn status_stream_revalidates_trust_and_shuts_down_authenticated_connections() {
    let pki = Pki::new();
    let allow = &[("cli", "cli")];
    let (mut listener, mut inbox) = pki.start(pki.trust("client", allow));
    let config = pki.client("backend", Some("cli"));
    let mut open = |listener: &Listener| {
        let mut conn = connect(listener.local_addr(), config.clone(), "localhost");
        send_status(&mut conn, "localhost", "");
        let admitted = claim(&mut inbox);
        let proof = admitted.connection.clone();
        let (publisher, stream) =
            StatusStream::new("boot".into(), vec![json!({"complete":true})], None);
        admitted.respond_status_stream(stream);
        let mut socket = status_socket(conn);
        assert_eq!(status_frame(&mut socket)["type"], "snapshot");
        (socket, publisher, proof)
    };
    let (mut revoked, publisher, proof) = open(&listener);
    listener.replace_trust(pki.trust("client", &[])).unwrap();
    publisher.event(Arc::new(
        json!({"kind":"must_not_be_written_after_revocation"}),
    ));
    expect_close(&mut revoked, CloseCode::Policy);
    listener.replace_trust(pki.trust("client", allow)).unwrap();
    let (mut reconnected, new_publisher, new_proof) = open(&listener);
    assert!(!proof.same_connection(&new_proof));
    drop(listener);
    expect_close(&mut reconnected, CloseCode::Away);
    assert!(!publisher.is_open());
    assert!(!new_publisher.is_open());
    assert!(!proof.is_alive());
    assert!(!new_proof.is_alive());
}

#[test]
fn upgraded_status_stream_cannot_outlive_its_certificate() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("short", "short")]));
    let mut conn = connect(
        listener.local_addr(),
        pki.client("backend", Some("short")),
        "localhost",
    );
    conn.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(7)))
        .unwrap();
    send_status(&mut conn, "localhost", "");
    let admitted = claim(&mut inbox);
    let (_publisher, stream) =
        StatusStream::new("boot".into(), vec![json!({"complete":true})], None);
    admitted.respond_status_stream(stream);
    let mut socket = status_socket(conn);
    assert_eq!(status_frame(&mut socket)["type"], "snapshot");
    expect_close(&mut socket, CloseCode::Policy);
}

#[test]
#[ignore = "runs the real 15-second idle and Pong deadlines"]
fn status_stream_requires_pong_for_server_idle_ping() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let mut conn = connect(
        listener.local_addr(),
        pki.client("backend", Some("cli")),
        "localhost",
    );
    conn.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    send_status(&mut conn, "localhost", "");
    let admitted = claim(&mut inbox);
    let (_publisher, stream) =
        StatusStream::new("boot".into(), vec![json!({"complete":true})], None);
    admitted.respond_status_stream(stream);
    let mut socket = status_socket(conn);
    assert_eq!(status_frame(&mut socket)["type"], "snapshot");

    let idle = Instant::now();
    assert!(matches!(socket.read().unwrap(), Message::Ping(_)));
    assert!(idle.elapsed() >= Duration::from_millis(14_950));
    // Never flush tungstenite's automatically queued Pong. The server should close.
    let sent = Instant::now();
    std::thread::sleep(Duration::from_millis(15_100));
    expect_close(&mut socket, CloseCode::Away);
    assert!(sent.elapsed() < Duration::from_secs(18));
}

#[test]
fn authenticates_distinct_services_and_withdraws_open_connections() {
    let pki = Pki::new();
    let allowed = &[
        ("cli", "cli"),
        ("apache", "apache"),
        ("nginx", "nginx"),
        ("wrong", "cli"),
        ("expired", "cli"),
    ];
    let (mut listener, mut inbox) = pki.start(pki.trust("client", allowed));
    for (client, actor) in &allowed[..3] {
        let config = pki.client("backend", Some(client));
        let mut conn = connect(listener.local_addr(), Arc::clone(&config), "localhost");
        send(&mut conn, GET).unwrap();
        let admitted = claim(&mut inbox);
        assert_eq!(admitted.actor.as_str(), *actor);
        assert_eq!(admitted.target.as_str(), "/health");
        admitted.respond(
            hyper::Response::builder()
                .status(204)
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(status(&mut conn).unwrap(), 204);
        assert_eq!(
            conn.get_ref().conn.handshake_kind(),
            Some(rustls::HandshakeKind::Full)
        );
        listener.replace_trust(pki.trust("client", &[])).unwrap();
        send(&mut conn, GET).unwrap();
        assert_eq!(status(&mut conn).unwrap(), 403);
        let mut fresh = connect(listener.local_addr(), config, "localhost");
        assert!(
            send(&mut fresh, GET)
                .and_then(|()| status(&mut fresh))
                .is_err()
        );
        assert!(inbox.try_recv().is_none());
        listener
            .replace_trust(pki.trust("client", allowed))
            .unwrap();
    }
    for (ca, name, dns) in [
        ("backend", None, "localhost"),
        ("backend", Some("unlisted"), "localhost"),
        ("backend", Some("stranger"), "localhost"),
        ("backend", Some("expired"), "localhost"),
        ("backend", Some("wrong"), "localhost"),
        ("foreign", Some("cli"), "localhost"),
        ("backend", Some("cli"), "wrong.local"),
    ] {
        let mut conn = connect(listener.local_addr(), pki.client(ca, name), dns);
        assert!(
            send(&mut conn, GET)
                .and_then(|()| status(&mut conn))
                .is_err()
        );
        assert!(inbox.try_recv().is_none());
    }
}
#[test]
fn enforces_origin_media_uri_and_streamed_body_bounds_before_claim() {
    let pki = Pki::new();
    let (mut listener, mut inbox) =
        pki.start(pki.trust("client", &[("cli", "provisioned-service")]));
    let config = pki.client("backend", Some("cli"));
    let valid = |target: &str, other: &str| {
        format!(
            "PATCH {target} HTTP/1.1\r\nHost: localhost\r\n{other}Content-Type: application/json-patch+json\r\nContent-Length: 2\r\n\r\n[]"
        )
    };
    for (wire, expected) in [
        ("GET / HTTP/1.1\r\nHost: untrusted.local\r\n\r\n".into(), 403),
        ("GET / HTTP/1.1\r\nHost: localhost\r\nHost: localhost\r\n\r\n".into(), 400),
        (valid("/", "Origin: https://untrusted.local\r\n"), 403),
        (valid("/", "X-HTTP-Method-Override: GET\r\n"), 400),
        (valid("https://elsewhere.local/a", ""), 400),
        (valid("//user@elsewhere.local/a", ""), 400),
        (valid(&format!("/{}", "x".repeat(3000)), ""), 414),
        ("PATCH / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n".into(), 415),
        ("PUT / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hal+json\r\nContent-Length: 1048577\r\n\r\n".into(), 413),
        ("GET / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\n\r\nx".into(), 400),
        (format!("GET / HTTP/1.1\r\nHost: localhost\r\n{}\r\n", (0..65).map(|i|format!("X-{i}: 0\r\n")).collect::<String>()), 431),
        (format!("GET / HTTP/1.1\r\nHost: localhost\r\nX-Fill: {}\r\n\r\n", "x".repeat(17000)), 431),
    ] {
        let mut client = connect(listener.local_addr(), Arc::clone(&config), "localhost");
        send(&mut client, wire.as_bytes()).unwrap();
        assert_eq!(status(&mut client).unwrap(), expected);
        assert!(inbox.try_recv().is_none());
    }

    let mut client = connect(listener.local_addr(), Arc::clone(&config), "localhost");
    send(&mut client, valid("/profile#fragment", "Origin: https://console.local\r\nIf-Match: \"v1\"\r\nAuthorization: Bearer synthetic\r\nX-Forwarded-Client-Cert: synthetic\r\n").as_bytes()).unwrap();
    let request = claim(&mut inbox);
    assert_eq!(request.target.as_str(), "/profile");
    assert_eq!(request.actor.as_str(), "provisioned-service");
    assert_eq!(request.headers["if-match"], "\"v1\"");
    assert!(request.headers.get("authorization").is_none());
    assert!(request.headers.get("x-forwarded-client-cert").is_none());
    assert_eq!(request.body.as_bytes(), b"[]");
    assert_eq!(format!("{:?}", request.body), "RequestBody([redacted])");
    request.respond(
        hyper::Response::builder()
            .status(204)
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(status(&mut client).unwrap(), 204);

    listener
        .replace_trust(pki.trust("foreign", &[("cli", "provisioned-service")]))
        .unwrap();
    send(&mut client, GET).unwrap();
    assert_eq!(status(&mut client).unwrap(), 403);
    assert!(inbox.try_recv().is_none());

    let result = Listener::start(
        "0.0.0.0:0".parse().unwrap(),
        ServerIdentity::new(vec![pki.cert("server")], pki.key("server")),
        pki.trust("client", &[]),
        OriginPolicy::new(vec!["localhost".into()], vec![]).unwrap(),
    );
    assert!(result.is_err());
}
#[test]
fn kept_session_expires_and_reconnect_performs_full_handshake() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("short", "short")]));
    let config = pki.client("backend", Some("short"));
    let mut kept = None;
    for _ in 0..2 {
        let mut client = connect(listener.local_addr(), Arc::clone(&config), "localhost");
        send(&mut client, GET).unwrap();
        claim(&mut inbox).respond(
            hyper::Response::builder()
                .status(204)
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(status(&mut client).unwrap(), 204);
        assert_eq!(
            client.get_ref().conn.handshake_kind(),
            Some(rustls::HandshakeKind::Full)
        );
        kept = Some(client);
    }
    let mut kept = kept.unwrap();
    std::thread::sleep(Duration::from_secs(5));
    send(&mut kept, GET).unwrap();
    assert_eq!(status(&mut kept).unwrap(), 403);
    assert!(inbox.try_recv().is_none());
}

#[test]
fn queued_work_expires_or_is_withdrawn_and_claimed_work_survives_disconnect() {
    let pki = Pki::new();
    let (mut listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let config = pki.client("backend", Some("cli"));
    let address = listener.local_addr();
    let open = || connect(address, Arc::clone(&config), "localhost");
    let mut accepted_client = open();
    send(&mut accepted_client, GET).unwrap();
    let accepted = claim(&mut inbox);
    drop(accepted_client);

    for outcome in ["expiry", "renew", "withdraw"] {
        let mut connected = Vec::new();
        for _ in 0..9 {
            let mut client = open();
            send(&mut client, GET).unwrap();
            claim(&mut inbox).respond(
                hyper::Response::builder()
                    .status(204)
                    .body(Vec::new())
                    .unwrap(),
            );
            assert_eq!(status(&mut client).unwrap(), 204);
            connected.push(client);
        }
        // Scheduling across sockets is unordered. The first immediate 503 proves the
        // queue filled, independent of which client was the ninth to reach dispatch.
        std::thread::scope(|scope| {
            let (ready, results) = std::sync::mpsc::channel();
            for mut client in connected {
                send(&mut client, GET).unwrap();
                let ready = ready.clone();
                scope.spawn(move || ready.send(status(&mut client).unwrap()).unwrap());
            }
            assert_eq!(results.recv_timeout(Duration::from_secs(1)).unwrap(), 503);
            if outcome == "renew" {
                listener
                    .replace_trust(pki.trust("client", &[("cli", "cli")]))
                    .unwrap();
                for _ in 0..8 {
                    claim(&mut inbox).respond(
                        hyper::Response::builder()
                            .status(204)
                            .body(Vec::new())
                            .unwrap(),
                    );
                }
            } else if outcome == "withdraw" {
                listener.replace_trust(pki.trust("client", &[])).unwrap();
                assert!(inbox.try_recv().is_none());
            }
            for _ in 0..8 {
                assert_eq!(
                    results.recv_timeout(Duration::from_secs(4)).unwrap(),
                    match outcome {
                        "renew" => 204,
                        "withdraw" => 403,
                        _ => 503,
                    }
                );
            }
        });
        assert!(inbox.try_recv().is_none());
    }
    drop(listener);
    assert!(inbox.try_recv().is_none());
    assert_eq!(accepted.actor.as_str(), "cli");
    accepted.respond(
        hyper::Response::builder()
            .status(204)
            .body(Vec::new())
            .unwrap(),
    );
}

#[test]
fn bounds_chunked_bodies_and_stops_workers_with_stalled_connections() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let addr = listener.local_addr();
    let config = pki.client("backend", Some("cli"));
    let mut client = connect(addr, Arc::clone(&config), "localhost");
    send(&mut client, b"PATCH / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json-patch+json\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n[\r\n1\r\n]\r\n0\r\n\r\n").unwrap();
    let request = claim(&mut inbox);
    assert_eq!(request.body.as_bytes(), b"[]");
    request.respond(
        hyper::Response::builder()
            .status(204)
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(status(&mut client).unwrap(), 204);
    let block = vec![b' '; 65536];
    send(&mut client, b"PATCH / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json-patch+json\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
    for _ in 0..16 {
        send(&mut client, b"10000\r\n").unwrap();
        send(&mut client, &block).unwrap();
        send(&mut client, b"\r\n").unwrap();
    }
    send(&mut client, b"1\r\nx\r\n0\r\n\r\n").unwrap();
    assert_eq!(status(&mut client).unwrap(), 413);
    assert!(inbox.try_recv().is_none());

    let mut slow = connect(addr, Arc::clone(&config), "localhost");
    slow.get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    send(&mut slow, b"PUT / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hal+json\r\nContent-Length: 2\r\n\r\n{").unwrap();
    assert_eq!(status(&mut slow).unwrap(), 408);
    assert!(inbox.try_recv().is_none());
    let mut waiting = connect(addr, config, "localhost");
    send(&mut waiting, GET).unwrap();
    let mut stalled = Vec::new();
    for _ in 0..4 {
        stalled.push(TcpStream::connect(addr).unwrap());
    }
    let start = Instant::now();
    drop(listener);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(inbox.try_recv().is_none());
    assert!(TcpStream::connect(addr).is_err());
}

#[path = "fixtures/https_proxy.rs"]
mod proxy;
#[test]
fn bounds_handshakes_connections_and_incomplete_request_headers() {
    use std::io::Read;
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let addr = listener.local_addr();
    let cfg = pki.client("backend", Some("cli"));
    let mut stalled = Vec::new();
    for _ in 0..4 {
        let socket = TcpStream::connect(addr).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stalled.push(socket);
    }
    let mut extra = connect(addr, cfg.clone(), "localhost");
    assert!(
        send(&mut extra, GET)
            .and_then(|()| status(&mut extra))
            .is_err()
    );
    for mut socket in stalled {
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    }
    let mut clients = Vec::new();
    for _ in 0..16 {
        let mut client = connect(addr, cfg.clone(), "localhost");
        send(&mut client, GET).unwrap();
        claim(&mut inbox).respond(
            hyper::Response::builder()
                .status(204)
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(status(&mut client).unwrap(), 204);
        clients.push(client);
    }
    let mut extra = connect(addr, cfg.clone(), "localhost");
    assert!(
        send(&mut extra, GET)
            .and_then(|()| status(&mut extra))
            .is_err()
    );
    let mut partial = clients.pop().unwrap();
    partial
        .get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_secs(12)))
        .unwrap();
    send(&mut partial, b"GET / HTTP/1.1\r\nHost:").unwrap();
    let start = Instant::now();
    let result = status(&mut partial);
    let elapsed = start.elapsed();
    match result {
        Ok(status) => assert_eq!(status, 408),
        Err(error) => assert!(!matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        )),
    }
    assert!(elapsed < Duration::from_secs(11));
    assert!(inbox.try_recv().is_none());
}

#[test]
fn rejects_invalid_method_syntax_before_handoff_and_checks_local_policy() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let mut client = connect(
        listener.local_addr(),
        pki.client("backend", Some("cli")),
        "localhost",
    );
    send(&mut client, b"G(ET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    assert_eq!(status(&mut client).unwrap(), 400);
    assert!(inbox.try_recv().is_none());
    assert!(ServiceId::new("header name\n").is_err());
    for (host, origin) in [
        ("a@localhost", "https://console.local"),
        ("localhost", "https://x@console.local"),
        ("localhost", "https://console.local/other"),
    ] {
        assert!(OriginPolicy::new(vec![host.into()], vec![origin.into()]).is_err());
    }
}
#[test]
fn revalidates_actor_after_receiving_a_streamed_body() {
    let pki = Pki::new();
    let (mut listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "before")]));
    let mut client = connect(
        listener.local_addr(),
        pki.client("backend", Some("cli")),
        "localhost",
    );
    send(&mut client, b"PUT / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hal+json\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\n").unwrap();
    assert_eq!(status(&mut client).unwrap(), 100);
    listener
        .replace_trust(pki.trust("client", &[("cli", "after")]))
        .unwrap();
    send(&mut client, b"{}").unwrap();
    assert_eq!(status(&mut client).unwrap(), 403);
    assert!(inbox.try_recv().is_none());
}
#[test]
fn review_real_http_response_preserves_representation_and_bounds_head_and_body() {
    use std::io::Read;
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let config = pki.client("backend", Some("cli"));
    let mut client = connect(listener.local_addr(), config.clone(), "localhost");
    send(&mut client, GET).unwrap();
    let mut response = hyper::Response::builder()
        .status(201)
        .version(hyper::Version::HTTP_10)
        .header("etag", "\"current\"")
        .header("content-type", "application/hal+json")
        .body(b"{}".to_vec())
        .unwrap();
    response
        .extensions_mut()
        .insert(hyper::ext::ReasonPhrase::from_static(b"Saved"));
    claim(&mut inbox).respond(response);
    let mut line = String::new();
    client.read_line(&mut line).unwrap();
    assert_eq!(line, "HTTP/1.1 201 Created\r\n");
    let mut headers = String::new();
    loop {
        line.clear();
        assert!(client.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        headers.push_str(&line.to_ascii_lowercase());
    }
    assert!(headers.contains("etag: \"current\"\r\n"));
    assert!(headers.contains("content-type: application/hal+json\r\n"));
    let mut body = [0; 2];
    client.read_exact(&mut body).unwrap();
    assert_eq!(&body, b"{}");
    for reply in [
        hyper::Response::new(vec![0; 4 * 1024 * 1024 + 1]),
        hyper::Response::builder()
            .header("etag", "x".repeat(16 * 1024))
            .body(Vec::new())
            .unwrap(),
    ] {
        let mut client = connect(listener.local_addr(), config.clone(), "localhost");
        send(&mut client, GET).unwrap();
        claim(&mut inbox).respond(reply);
        assert_eq!(status(&mut client).unwrap(), 500);
    }
}
#[test]
fn review_queued_request_cannot_outlive_short_certificate() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let pki = Pki::new();
    let end = fs::read_to_string(pki.0.join("short.expiry"))
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("short", "short")]));
    let mut client = connect(
        listener.local_addr(),
        pki.client("backend", Some("short")),
        "localhost",
    );
    send(&mut client, GET).unwrap();
    claim(&mut inbox).respond(
        hyper::Response::builder()
            .status(204)
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(status(&mut client).unwrap(), 204);
    let not_after = UNIX_EPOCH + Duration::from_secs(end);
    std::thread::sleep(
        not_after
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    );
    // A 100 proves admission of the head during the certificate's last valid second.
    send(&mut client, b"PUT / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hal+json\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\n").unwrap();
    assert_eq!(status(&mut client).unwrap(), 100);
    send(&mut client, b"{}").unwrap();
    std::thread::sleep(
        (not_after + Duration::from_millis(1120))
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    );
    assert!(inbox.try_recv().is_none());
    assert_eq!(status(&mut client).unwrap(), 403);
}

#[test]
fn admitted_requests_share_only_their_own_live_tls_connection() {
    let pki = Pki::new();
    let (listener, mut inbox) = pki.start(pki.trust("client", &[("cli", "cli")]));
    let config = pki.client("backend", Some("cli"));
    let mut first = connect(listener.local_addr(), Arc::clone(&config), "localhost");
    let mut second = connect(listener.local_addr(), config, "localhost");
    let mut issue = |client: &mut Client| {
        send(client, GET).unwrap();
        let request = claim(&mut inbox);
        assert_eq!(request.actor.as_str(), "cli");
        let connection = request.connection.clone();
        request.respond(
            hyper::Response::builder()
                .status(204)
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(status(client).unwrap(), 204);
        connection
    };
    let initial = issue(&mut first);
    let renewed = issue(&mut first);
    let other = issue(&mut second);
    assert!(initial.same_connection(&renewed));
    assert!(!initial.same_connection(&other));
    assert!(initial.is_alive() && other.is_alive());
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(3);
    while initial.is_alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!initial.is_alive() && !renewed.is_alive());
    assert!(other.is_alive());
}
