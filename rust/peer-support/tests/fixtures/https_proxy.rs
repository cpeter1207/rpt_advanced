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
