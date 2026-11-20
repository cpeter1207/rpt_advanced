//! Real staged nginx/Apache forwarding and negative upstream verification.
use super::*;
use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::inotify;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::process::{Child, Stdio};

const PATCH: &str = "application/json-patch+json";
const ORIGIN: &str = "https://console.local";

struct Proxy {
    process: Child,
    dir: PathBuf,
    port: u16,
    csrf: String,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // The test creates an isolated process group containing only its frontend.
        let group = format!("-{}", self.process.id());
        let _ = Command::new("/bin/kill")
            .args(["-TERM", "--", &group])
            .status();
        let limit = Instant::now() + Duration::from_secs(2);
        while self.process.try_wait().unwrap().is_none() && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.process.try_wait().unwrap().is_none() {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &group])
                .status();
        }
        self.process.wait().unwrap();
        fs::remove_dir_all(&self.dir).unwrap();
    }
}

impl Proxy {
    fn start(kind: &str, pki: &Pki, backend: SocketAddr, ca: &str, name: &str) -> Self {
        let dir = Command::new("mktemp").arg("-d").output().unwrap();
        assert!(dir.status.success());
        let dir = PathBuf::from(String::from_utf8(dir.stdout).unwrap().trim());
        // Workers read the disposable frontend password hash. TLS keys stay in the other 0700 directory.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o711)).unwrap();
        let password = Command::new("openssl")
            .args(["passwd", "-apr1", "fixture-password"])
            .output()
            .unwrap();
        assert!(password.status.success());
        fs::write(
            dir.join("users"),
            format!("user:{}", String::from_utf8(password.stdout).unwrap()),
        )
        .unwrap();
        fs::set_permissions(dir.join("users"), fs::Permissions::from_mode(0o644)).unwrap();
        let mut token = [0; 16];
        SystemRandom::new().fill(&mut token).unwrap();
        let csrf = token.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (mut command, template) = if kind == "nginx" {
            let mut command = Command::new("/usr/sbin/nginx");
            command.arg("-p").arg(&dir).arg("-c");
            (command, include_str!("https-nginx.conf"))
        } else {
            let key = Command::new("openssl")
                .args(["pkey", "-traditional", "-in"])
                .arg(pki.0.join("apache.key"))
                .output()
                .unwrap();
            assert!(key.status.success());
            let mut pair = fs::read(pki.0.join("apache.pem")).unwrap();
            pair.extend_from_slice(&key.stdout);
            fs::write(pki.0.join("apache-pair.pem"), pair).unwrap();
            let mut command = Command::new("/usr/sbin/apache2");
            command.args(["-DFOREGROUND", "-f"]);
            (command, include_str!("https-apache.conf"))
        };
        let file = dir.join("server.conf");
        fs::write(
            &file,
            template
                .replace("@DIR@", dir.to_str().unwrap())
                .replace("@PKI@", pki.0.to_str().unwrap())
                .replace("@BACK@", &backend.port().to_string())
                .replace("@FRONT@", &port.to_string())
                .replace("@CSRF@", &csrf)
                .replace("@CA@", ca)
                .replace("@NAME@", name),
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let process = command
            .arg(file)
            .process_group(0)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut proxy = Self {
            process,
            dir,
            port,
            csrf,
        };
        let until = Instant::now() + Duration::from_secs(3);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                proxy.process.try_wait().unwrap().is_none(),
                "staged frontend failed to start"
            );
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        proxy
    }

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
    let proxy = Proxy::start(service, pki, backend, ca, name);
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
        let proxy = Proxy::start(service, &pki, address, "backend", "localhost");
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
