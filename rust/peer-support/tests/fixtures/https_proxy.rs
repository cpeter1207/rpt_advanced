//! Real staged nginx/Apache forwarding and negative upstream verification.
use super::*;
use rustix::fs::inotify;
use std::process::{Child, Stdio};

const PATCH: &str = "application/json-patch+json";
const ORIGIN: &str = "https://console.local";

#[path = "../support/https_frontend.rs"]
mod frontend;
use frontend::Proxy;
impl Proxy {
    fn curl(
        &self,
        pki: &Pki,
        write: bool,
        auth: bool,
        origin: &str,
        csrf: &str,
        media: &str,
    ) -> Child {
        let mut cmd = Command::new("curl");
        cmd.args([
            "--silent",
            "--noproxy",
            "*",
            "--max-time",
            "5",
            "--http1.1",
            "-4",
            "--cacert",
        ])
        .arg(pki.0.join("backend.pem"))
        .args(["-o", "/dev/null", "-w", "%{http_code}"]);
        if auth {
            cmd.args(["--user", "user:fixture-password"]);
        }
        if write {
            cmd.args([
                "-X",
                "PATCH",
                "-H",
                "If-Match: \"original\"",
                "-H",
                "X-Forwarded-Client-Cert: spoof",
                "-H",
                &format!("Origin: {origin}"),
                "-H",
                &format!("X-CSRF-Token: {csrf}"),
                "-H",
                &format!("Content-Type: {media}"),
                "-H",
                "Transfer-Encoding: chunked",
                "--data-binary",
                "@-",
            ])
            .stdin(Stdio::piped());
        }
        let mut child = cmd
            .arg(format!("https://localhost:{}/config", self.port))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut input) = child.stdin.take() {
            input.write_all(&vec![b' '; 256 * 1024]).unwrap();
            input
                .write_all(b"[{\"test-only\":\"body-marker\"}]")
                .unwrap();
        }
        child
    }

    fn status(child: Child) -> u16 {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "frontend TLS request failed");
        String::from_utf8(output.stdout).unwrap().parse().unwrap()
    }
}

fn backend(pki: &Pki, service: &str, name: &str) -> (Listener, CoordinatorInbox) {
    // Apache derives its upstream Host authority from the verified backend URL, with its port.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    Listener::start(
        addr,
        ServerIdentity::new(vec![pki.cert(name)], pki.key(name)),
        pki.trust("client", &[(service, service)]),
        OriginPolicy::new(
            vec!["localhost".into(), format!("localhost:{}", addr.port())],
            vec![ORIGIN.into()],
        )
        .unwrap(),
    )
    .unwrap()
}

fn expect_rejected_backend(
    service: &str,
    pki: &Pki,
    backend: SocketAddr,
    inbox: &mut CoordinatorInbox,
    ca: &str,
    name: &str,
) {
    let proxy = Proxy::start(service, &pki.0, backend, ca, name);
    let mut child = proxy.curl(pki, false, true, "", "", "");
    while child.try_wait().unwrap().is_none() {
        assert!(
            inbox.try_recv().is_none(),
            "{service} forwarded with invalid backend trust"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let status = Proxy::status(child);
    eprintln!("{service} rejected backend ca={ca} name={name}: {status}");
    assert!(
        (500..600).contains(&status),
        "{service} backend rejection returned {status}"
    );
    assert!(inbox.try_recv().is_none());
}

#[test]
#[ignore = "staged Debian nginx and apache2-bin packages required"]
fn real_proxies_enforce_frontend_policy_and_verified_upstream_mtls() {
    let pki = Pki::new();
    for service in ["nginx", "apache"] {
        let (mut listener, mut inbox) = backend(&pki, service, "server");
        let address = listener.local_addr();
        let proxy = Proxy::start(service, &pki.0, address, "backend", "localhost");
        assert_eq!(
            Proxy::status(proxy.curl(&pki, false, false, "", "", "")),
            401
        );
        for (origin, csrf, media) in [
            ("https://other.local", proxy.csrf.as_str(), PATCH),
            (ORIGIN, "incorrect", PATCH),
            (ORIGIN, proxy.csrf.as_str(), "text/plain"),
        ] {
            assert_eq!(
                Proxy::status(proxy.curl(&pki, true, true, origin, csrf, media)),
                403
            );
            assert!(inbox.try_recv().is_none());
        }
        // Observe transient creation too: checking only surviving temp files misses an unlink.
        let watch =
            inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).unwrap();
        for dir in [
            std::env::temp_dir(),
            proxy.dir.join("body"),
            proxy.dir.join("proxy"),
        ] {
            if dir.is_dir() {
                inotify::add_watch(&watch, &dir, inotify::WatchFlags::CREATE).unwrap();
            }
        }
        let sent = proxy.curl(&pki, true, true, ORIGIN, &proxy.csrf, PATCH);
        let request = claim(&mut inbox);
        assert_eq!(request.actor.as_str(), service);
        assert_eq!(request.headers["if-match"], "\"original\"");
        assert_eq!(
            &request.body.as_bytes()[256 * 1024..],
            b"[{\"test-only\":\"body-marker\"}]"
        );
        assert!(request.headers.get("authorization").is_none());
        assert!(request.headers.get("x-forwarded-client-cert").is_none());
        request.respond(
            hyper::Response::builder()
                .status(204)
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(Proxy::status(sent), 204);
        let mut buf = [std::mem::MaybeUninit::uninit(); 512];
        let mut events = inotify::Reader::new(watch, &mut buf);
        assert!(
            matches!(events.next(), Err(rustix::io::Errno::WOULDBLOCK)),
            "{service} created a staging file during the chunked write"
        );
        listener.replace_trust(pki.trust("client", &[])).unwrap();
        // A newly handshaken connection would receive a TLS alert. A 403 proves reuse.
        assert_eq!(
            Proxy::status(proxy.curl(&pki, false, true, "", "", "")),
            403,
            "{service} upstream reuse"
        );
        assert!(inbox.try_recv().is_none());
        for path in ["body", "proxy"] {
            if proxy.dir.join(path).exists() {
                assert_eq!(fs::read_dir(proxy.dir.join(path)).unwrap().count(), 0);
            }
        }
        assert!(log_without_marker(&proxy.dir.join("error.log")).unwrap());
        drop(proxy);
        listener
            .replace_trust(pki.trust("client", &[(service, service)]))
            .unwrap();
        for (ca, name) in [("foreign", "localhost"), ("backend", "127.0.0.1")] {
            expect_rejected_backend(service, &pki, address, &mut inbox, ca, name);
        }
        let (other, mut other_inbox) = backend(&pki, service, "other-server");
        expect_rejected_backend(
            service,
            &pki,
            other.local_addr(),
            &mut other_inbox,
            "backend",
            "localhost",
        );
    }
}

#[test]
#[ignore = "staged Debian nginx and apache2-bin packages required"]
fn real_proxies_keep_status_websocket_under_frontend_and_backend_policy() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let pki = Pki::new();
    for service in ["nginx", "apache"] {
        let (listener, mut inbox) = backend(&pki, service, "server");
        let proxy = Proxy::start(
            service,
            &pki.0,
            listener.local_addr(),
            "backend",
            "localhost",
        );
        let config = pki.client("backend", None);
        let addr = SocketAddr::from(([127, 0, 0, 1], proxy.port));
        let host = format!("localhost:{}", proxy.port);
        let auth = format!(
            "Authorization: Basic {}\r\n",
            STANDARD.encode("user:fixture-password")
        );

        let mut unadmitted = connect(addr, config.clone(), "localhost");
        send_status(&mut unadmitted, &host, &format!("Origin: {ORIGIN}\r\n"));
        assert_eq!(status(&mut unadmitted).unwrap(), 401);
        assert!(inbox.try_recv().is_none());

        let mut wrong_origin = connect(addr, config.clone(), "localhost");
        send_status(
            &mut wrong_origin,
            &host,
            &format!("{auth}Origin: https://other.local\r\n"),
        );
        assert_eq!(status(&mut wrong_origin).unwrap(), 403);
        assert!(inbox.try_recv().is_none());

        let mut connection = connect(addr, config, "localhost");
        send_status(
            &mut connection,
            &host,
            &format!("{auth}Origin: {ORIGIN}\r\n"),
        );
        let admitted = claim(&mut inbox);
        assert!(admitted.is_status_upgrade());
        assert_eq!(admitted.actor.as_str(), service);
        assert_eq!(admitted.target.query(), Some("port=example"));
        assert!(admitted.headers.get("authorization").is_none());
        let page =
            json!({"state_cursor":"42","complete":true,"status":{"site":{"quality":"valid"}}});
        let (_publisher, stream) =
            StatusStream::new("proxy-fixture".into(), vec![page.clone()], None);
        admitted.respond_status_stream(stream);
        let mut connection = status_socket(connection);
        assert_eq!(status_frame(&mut connection)["data"], page);
        connection.send(Message::Close(None)).unwrap();
    }
}

#[test]
#[ignore = "staged Debian nginx and apache2-bin packages required"]
fn real_proxies_replace_browser_principal_headers_with_the_authenticated_user() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let pki = Pki::new();
    for service in ["nginx", "apache"] {
        let (mut listener, mut inbox) = backend(&pki, service, "server");
        listener
            .replace_trust(
                pki.trust("client", &[(service, service)])
                    .with_browser_principal_proxies([ServiceTrust::leaf_pin(&pki.cert(service))])
                    .unwrap(),
            )
            .unwrap();
        let proxy = Proxy::start(
            service,
            &pki.0,
            listener.local_addr(),
            "backend",
            "localhost",
        );
        let path = proxy.dir.join("users");
        let row = fs::read_to_string(&path).unwrap();
        let second = row.replacen("user:", "other:", 1);
        assert_ne!(row, second);
        fs::write(&path, format!("{row}{second}")).unwrap();
        let mut principals = Vec::new();
        for user in ["user", "other", "user"] {
            let addr = SocketAddr::from(([127, 0, 0, 1], proxy.port));
            let mut browser = connect(addr, pki.client("backend", None), "localhost");
            let basic = STANDARD.encode(format!("{user}:fixture-password"));
            let request = format!(
                "GET /frontend-principal HTTP/1.1\r\nHost: localhost:{}\r\n\
                 Authorization: Basic {basic}\r\nOrigin: {ORIGIN}\r\n\
                 X-Rpt-Advanced-Oidc-Issuer: https://spoof.local\r\n\
                 X-Rpt-Advanced-Oidc-Subject: spoofed\r\n\r\n",
                proxy.port
            );
            send(&mut browser, request.as_bytes()).unwrap();
            let admitted = claim(&mut inbox);
            assert_eq!(admitted.actor.as_str(), service);
            assert!(admitted.headers.get("authorization").is_none());
            assert!(!admitted.headers.contains_key("x-rpt-advanced-oidc-issuer"));
            assert!(!admitted.headers.contains_key("x-rpt-advanced-oidc-subject"));
            principals.push(admitted.browser_principal().unwrap().clone());
            admitted.respond(
                hyper::Response::builder()
                    .status(204)
                    .body(Vec::new())
                    .unwrap(),
            );
            assert_eq!(status(&mut browser).unwrap(), 204);
        }
        assert_ne!(principals[0], principals[1]);
        assert_eq!(principals[0], principals[2]);
    }
}

fn log_without_marker(path: &std::path::Path) -> io::Result<bool> {
    let log = fs::read(path)?;
    Ok(!log
        .windows(b"body-marker".len())
        .any(|b| b == b"body-marker"))
}

#[test]
fn review_missing_proxy_log_is_not_a_clean_log() {
    let pki = Pki::new();
    let path = pki.0.join("error.log");
    assert!(log_without_marker(&path).is_err());
    fs::write(&path, b"ordinary event").unwrap();
    assert!(log_without_marker(&path).unwrap());
    fs::write(&path, b"body-marker").unwrap();
    assert!(!log_without_marker(&path).unwrap());
}
