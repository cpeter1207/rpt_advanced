//! Ephemeral real nginx/Apache frontends using the staged management mTLS templates.
use std::{
    fs,
    net::{SocketAddr, TcpStream},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub(crate) struct Proxy {
    process: Child,
    pub(crate) dir: PathBuf,
    pub(crate) port: u16,
    pub(crate) csrf: String,
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
    pub(crate) fn start(kind: &str, pki: &Path, backend: SocketAddr, ca: &str, name: &str) -> Self {
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
        let csrf = rptadv_peer_support::working_files::transaction::new_identity()
            .unwrap()
            .simple()
            .to_string();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (mut command, template) = if kind == "nginx" {
            let mut command = Command::new("/usr/sbin/nginx");
            command.arg("-p").arg(&dir).arg("-c");
            (command, include_str!("../fixtures/https-nginx.conf"))
        } else {
            let key = Command::new("openssl")
                .args(["pkey", "-traditional", "-in"])
                .arg(pki.join("apache.key"))
                .output()
                .unwrap();
            assert!(key.status.success());
            let mut pair = fs::read(pki.join("apache.pem")).unwrap();
            pair.extend_from_slice(&key.stdout);
            fs::write(pki.join("apache-pair.pem"), pair).unwrap();
            let mut command = Command::new("/usr/sbin/apache2");
            command.args(["-DFOREGROUND", "-f"]);
            (command, include_str!("../fixtures/https-apache.conf"))
        };
        let file = dir.join("server.conf");
        fs::write(
            &file,
            template
                .replace("@DIR@", dir.to_str().unwrap())
                .replace("@PKI@", pki.to_str().unwrap())
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
}
