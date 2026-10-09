//! ASL3-compatible HTTPS node registration outside audio callbacks.

use crate::configuration::ConfigError;
use crate::secrets::SecretsFile;
use crossbeam_queue::ArrayQueue;
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const RESPONSE_LIMIT: u64 = 64 * 1024;
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Safe diagnostics contain no credentials, URLs, or untrusted response text.
#[derive(Debug, PartialEq, Eq)]
enum RegistrationError {
    HttpStatus(u16),
    Transport(ureq::ErrorKind),
    Read(std::io::ErrorKind),
    InvalidResponse,
    Rejected,
}

/// One enabled node's registration details. Secret contents are never formatted or exposed.
#[derive(Clone)]
pub struct RegistrationTarget {
    node: String,
    url: String,
    local_port: u16,
    interval_seconds: u64,
    secret: String,
}

impl RegistrationTarget {
    /// Node number used as the AllStarLink registration username.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// HTTPS endpoint configured for this node.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Locally advertised IAX2 UDP port.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Configured refresh interval in seconds.
    pub fn interval_seconds(&self) -> u64 {
        self.interval_seconds
    }
}

/// Resolve enabled nodes that have both an HTTPS endpoint and a configured IAX secret.
pub fn targets(
    configuration: &str,
    secrets: &SecretsFile,
) -> Result<Vec<RegistrationTarget>, ConfigError> {
    let mut targets = Vec::new();
    for node in crate::configuration::resolve_radio_nodes(configuration)? {
        if !node.enabled || node.registration_url.is_empty() {
            continue;
        }
        let Some(secret) = secrets.for_node(&node.node) else {
            eprintln!(
                "node {}: HTTPS registration skipped; no IAX secret is configured",
                node.node
            );
            continue;
        };
        targets.push(RegistrationTarget {
            node: node.node,
            url: node.registration_url,
            local_port: node.iax_port,
            interval_seconds: node.registration_interval_seconds,
            secret: secret.to_owned(),
        });
    }
    Ok(targets)
}

/// Owns the bounded, blocking HTTP work on one non-audio worker thread.
pub struct RegistrationWorker {
    updates: Arc<ArrayQueue<Vec<RegistrationTarget>>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RegistrationWorker {
    /// Start registration immediately for the supplied node set.
    pub fn start(targets: Vec<RegistrationTarget>) -> std::io::Result<Self> {
        let updates = Arc::new(ArrayQueue::new(1));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("rpt-iax-registration".into())
            .spawn({
                let updates = Arc::clone(&updates);
                let stopping = Arc::clone(&stopping);
                move || registration_loop(updates, stopping, targets)
            })?;
        Ok(Self {
            updates,
            stopping,
            thread: Some(thread),
        })
    }

    /// Replace settings through a one-slot lock-free queue; latest pending settings win.
    pub fn replace(&self, targets: Vec<RegistrationTarget>) {
        let mut targets = targets;
        loop {
            match self.updates.push(targets) {
                Ok(()) => return,
                Err(updated) => {
                    targets = updated;
                    let _ = self.updates.pop();
                }
            }
        }
    }

    /// Stop the worker and wait for any bounded in-flight HTTPS request to finish.
    pub fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RegistrationWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn registration_loop(
    updates: Arc<ArrayQueue<Vec<RegistrationTarget>>>,
    stopping: Arc<AtomicBool>,
    mut targets: Vec<RegistrationTarget>,
) {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(15))
        .timeout_write(Duration::from_secs(15))
        .build();
    let mut due = vec![Instant::now(); targets.len()];
    let mut registered = vec![false; targets.len()];

    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        if let Some(replacement) = updates.pop() {
            targets = replacement;
            due = vec![Instant::now(); targets.len()];
            registered = vec![false; targets.len()];
        }
        let Some((index, deadline)) = due.iter().enumerate().min_by_key(|(_, when)| **when) else {
            thread::sleep(UPDATE_POLL_INTERVAL);
            continue;
        };
        let now = Instant::now();
        if *deadline > now {
            thread::sleep((*deadline - now).min(UPDATE_POLL_INTERVAL));
            continue;
        }
        let target = targets[index].clone();
        let interval = Duration::from_secs(target.interval_seconds);

        let refresh = match post_registration(&agent, &target) {
            Ok(refresh) => {
                if !registered[index] {
                    eprintln!("node {}: HTTPS registration succeeded", target.node);
                }
                registered[index] = true;
                refresh
            }
            Err(error) => {
                registered[index] = false;
                eprintln!("node {}: HTTPS registration failed: {error:?}", target.node);
                interval
            }
        };
        due[index] = Instant::now() + refresh;
    }
}

fn post_registration(
    agent: &ureq::Agent,
    target: &RegistrationTarget,
) -> Result<Duration, RegistrationError> {
    let body = request_body(target);
    let response = agent
        .post(&target.url)
        .set("Content-Type", "application/json")
        .send_bytes(&body)
        .map_err(|error| match error {
            ureq::Error::Status(status, _) => RegistrationError::HttpStatus(status),
            ureq::Error::Transport(transport) => RegistrationError::Transport(transport.kind()),
        })?;
    if !(200..300).contains(&response.status()) {
        return Err(RegistrationError::HttpStatus(response.status()));
    }
    let mut response_body = Vec::new();
    response
        .into_reader()
        .take(RESPONSE_LIMIT)
        .read_to_end(&mut response_body)
        .map_err(|error| RegistrationError::Read(error.kind()))?;
    response_refresh(&response_body, target.interval_seconds)
}

fn request_body(target: &RegistrationTarget) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "port": target.local_port,
        "data": {"nodes": {target.node.as_str(): {
            "node": target.node.as_str(),
            "passwd": target.secret.as_str(),
            "remote": 0
        }}}
    }))
    .expect("registration request contains only JSON-compatible values")
}

fn response_refresh(body: &[u8], fallback_seconds: u64) -> Result<Duration, RegistrationError> {
    let response: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| RegistrationError::InvalidResponse)?;
    let data = response
        .get("data")
        .ok_or(RegistrationError::InvalidResponse)?;
    let detail = match data {
        serde_json::Value::String(value) => {
            serde_json::from_str(value).unwrap_or_else(|_| data.clone())
        }
        value => value.clone(),
    };
    if !detail
        .to_string()
        .to_ascii_lowercase()
        .contains("successfully registered")
    {
        return Err(RegistrationError::Rejected);
    }
    Ok(Duration::from_secs(
        response
            .get("refresh")
            .and_then(serde_json::Value::as_u64)
            .filter(|seconds| (1..=86_400).contains(seconds))
            .unwrap_or(fallback_seconds),
    ))
}

#[cfg(test)]
mod tests {
    use super::{RegistrationError, RegistrationTarget, request_body, response_refresh, targets};
    use crate::secrets::SecretsFile;
    use std::time::Duration;

    fn target() -> RegistrationTarget {
        RegistrationTarget {
            node: "524950".into(),
            url: "https://register.example/".into(),
            local_port: 4569,
            interval_seconds: 60,
            secret: "test-secret".into(),
        }
    }

    #[test]
    fn targets_include_enabled_nodes_with_endpoint_and_inherited_or_local_secret() {
        let configuration = "[general]\niax_registration_url=https://register.example/\n[1000]\nnode_enabled=no\n[radio 1000]\n[2000]\niax_registration_url=\n[3000]\n[4000]\niax_local_port=4570\n";
        let secrets = SecretsFile::parse(
            "[general]\niax_secret=shared-secret\n[4000]\niax_secret=node-secret\n",
        )
        .unwrap();

        let selected = targets(configuration, &secrets).unwrap();

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].node(), "3000");
        assert_eq!(selected[0].url(), "https://register.example/");
        assert_eq!(selected[0].local_port(), 4569);
        assert_eq!(selected[0].interval_seconds(), 60);
        assert_eq!(selected[1].node(), "4000");
        assert_eq!(selected[1].local_port(), 4570);
        assert_eq!(selected[1].secret, "node-secret");
        assert_eq!(selected[0].secret, "shared-secret");
    }

    #[test]
    fn targets_skip_enabled_nodes_without_a_configured_secret() {
        let configuration = "[1000]\niax_registration_url=https://register.example/\n";
        let selected = targets(configuration, &SecretsFile::parse("").unwrap()).unwrap();
        assert!(selected.is_empty());
    }

    #[test]
    fn request_uses_the_asl_https_registration_json_shape() {
        let body = request_body(&target());
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(request["port"], 4569);
        assert_eq!(request["data"]["nodes"]["524950"]["node"], "524950");
        assert_eq!(request["data"]["nodes"]["524950"]["passwd"], "test-secret");
        assert_eq!(request["data"]["nodes"]["524950"]["remote"], 0);
    }

    #[test]
    fn response_uses_server_refresh_after_confirmed_registration() {
        assert_eq!(
            response_refresh(br#"{"refresh":75,"data":"successfully registered"}"#, 60),
            Ok(Duration::from_secs(75))
        );
        assert_eq!(
            response_refresh(br#"{"refresh":75,"data":"registration denied"}"#, 60),
            Err(RegistrationError::Rejected)
        );
    }

    #[test]
    fn response_accepts_asl_json_encoded_registration_detail() {
        assert_eq!(
            response_refresh(
                br#"{"refresh":45,"data":"{\"result\":\"successfully registered\"}"}"#,
                60,
            ),
            Ok(Duration::from_secs(45))
        );
    }

    #[test]
    fn malformed_or_unbounded_server_refresh_is_rejected() {
        assert_eq!(
            response_refresh(b"not json", 60),
            Err(RegistrationError::InvalidResponse)
        );
        assert_eq!(
            response_refresh(br#"{"refresh":86401,"data":"successfully registered"}"#, 60),
            Ok(Duration::from_secs(60))
        );
    }

    #[test]
    fn response_refresh_uses_fallback_for_missing_or_invalid_refresh_values() {
        for body in [
            br#"{"data":{"result":"successfully registered"}}"#.as_slice(),
            br#"{"refresh":0,"data":"successfully registered"}"#,
            br#"{"refresh":"45","data":"successfully registered"}"#,
        ] {
            assert_eq!(response_refresh(body, 60), Ok(Duration::from_secs(60)));
        }
        assert_eq!(
            response_refresh(br#"{"data":"not-json"}"#, 60),
            Err(RegistrationError::Rejected)
        );
    }

    #[test]
    fn registration_worker_uses_lock_free_bounded_control_state() {
        let source = include_str!("registration.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();

        assert!(source.contains("ArrayQueue"));
        assert!(source.contains("AtomicBool"));
        assert!(!source.contains("Mutex"));
        assert!(!source.contains("Condvar"));
    }

    #[test]
    fn registration_posts_json_and_accepts_the_asl_success_response() {
        use std::{
            io::{BufRead, BufReader, Read, Write},
            net::TcpListener,
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            let mut content_length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line
                    .strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
                {
                    content_length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["port"], 4569);
            assert_eq!(request["data"]["nodes"]["524950"]["passwd"], "test-secret");
            drop(reader);
            let body = br#"{"refresh":45,"data":"successfully registered"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let mut target = target();
        target.url = endpoint;
        let agent = ureq::AgentBuilder::new().redirects(0).build();

        assert_eq!(
            super::post_registration(&agent, &target),
            Ok(Duration::from_secs(45))
        );
        server.join().unwrap();
    }

    #[test]
    fn registration_ignores_unreachable_and_malformed_endpoints() {
        use std::{io::Write, net::TcpListener, thread};

        let unused = TcpListener::bind("127.0.0.1:0").unwrap();
        let unused_port = unused.local_addr().unwrap().port();
        drop(unused);
        let mut unreachable = target();
        unreachable.url = format!("http://127.0.0.1:{unused_port}/");
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_millis(100))
            .build();
        assert!(matches!(
            super::post_registration(&agent, &unreachable),
            Err(RegistrationError::Transport(_))
        ));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut malformed = target();
        malformed.url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_registration_request(&mut stream);
            let body = b"not json";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        assert_eq!(
            super::post_registration(&agent, &malformed),
            Err(RegistrationError::InvalidResponse)
        );
        server.join().unwrap();
    }

    #[test]
    fn registration_classifies_http_status_without_exposing_response() {
        for status in [302, 403] {
            let result = post_loopback_response(status, br#"{"error":"test-secret denied"}"#);
            assert_eq!(result, Err(RegistrationError::HttpStatus(status)));
            assert!(!format!("{result:?}").contains("test-secret"));
        }
    }

    #[test]
    fn registration_classifies_valid_rejection_without_exposing_response() {
        let result = post_loopback_response(200, br#"{"data":"test-secret registration denied"}"#);
        assert_eq!(result, Err(RegistrationError::Rejected));
        assert!(!format!("{result:?}").contains("test-secret"));
    }

    #[test]
    fn registration_read_timeout_bounds_a_stalled_server() {
        use std::{io::Write, net::TcpListener, thread, time::Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut target = target();
        target.url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_registration_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
                .unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_millis(50))
            .build();
        let started = Instant::now();

        assert!(matches!(
            super::post_registration(&agent, &target),
            Err(RegistrationError::Read(_))
        ));
        assert!(started.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }

    #[test]
    fn registration_worker_stops_while_idle() {
        let mut worker = super::RegistrationWorker::start(Vec::new()).unwrap();
        worker.stop();
        assert!(worker.thread.is_none());
    }

    #[test]
    fn registration_worker_posts_its_initial_target() {
        use std::{
            io::Write,
            net::{TcpListener, TcpStream},
            thread,
            time::Instant,
        };

        fn respond(mut stream: TcpStream) {
            read_registration_request(&mut stream);
            let body = br#"{"refresh":60,"data":"successfully registered"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut configured = target();
        configured.url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        respond(stream);
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return false;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("registration listener failed: {error}"),
                }
            }
        });

        let mut worker = super::RegistrationWorker::start(vec![configured]).unwrap();
        assert!(
            server.join().unwrap(),
            "initial target must be posted promptly"
        );
        worker.replace(Vec::new());
        thread::sleep(Duration::from_millis(100));
        worker.stop();
    }

    #[test]
    fn registration_worker_refreshes_an_already_registered_target() {
        use std::{io::Write, net::TcpListener, thread, time::Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut configured = target();
        configured.url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut count = 0;
            while count < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        read_registration_request(&mut stream);
                        let body = br#"{"refresh":1,"data":"successfully registered"}"#;
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        stream.write_all(body).unwrap();
                        count += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("registration listener failed: {error}"),
                }
            }
            count
        });

        let mut worker = super::RegistrationWorker::start(vec![configured]).unwrap();
        let requests = server.join().unwrap();
        worker.stop();
        assert_eq!(
            requests, 2,
            "the registered target must refresh successfully"
        );
    }

    #[test]
    fn registration_worker_backs_off_after_an_invalid_response() {
        use std::{io::Write, net::TcpListener, thread};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut configured = target();
        configured.url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_registration_request(&mut stream);
            let body = b"not json";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let mut worker = super::RegistrationWorker::start(vec![configured]).unwrap();
        server.join().unwrap();
        thread::sleep(Duration::from_millis(100));
        worker.stop();
    }

    #[test]
    fn registration_worker_replaces_the_full_pending_update_slot() {
        use std::sync::{Arc, atomic::AtomicBool};

        let updates = Arc::new(crossbeam_queue::ArrayQueue::new(1));
        assert!(updates.push(vec![target()]).is_ok());
        let worker = super::RegistrationWorker {
            updates: Arc::clone(&updates),
            stopping: Arc::new(AtomicBool::new(false)),
            thread: None,
        };
        let mut replacement = target();
        replacement.node = "508422".into();

        worker.replace(vec![replacement]);

        assert_eq!(updates.pop().unwrap()[0].node(), "508422");
    }

    fn read_registration_request(stream: &mut std::net::TcpStream) {
        use std::io::{BufRead, BufReader, Read};

        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        let mut content_length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line
                .strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
            {
                content_length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
    }

    fn post_loopback_response(
        status: u16,
        body: &'static [u8],
    ) -> Result<Duration, RegistrationError> {
        use std::{io::Write, net::TcpListener, thread};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut configured = target();
        configured.url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_registration_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        let result = super::post_registration(&agent, &configured);
        server.join().unwrap();
        result
    }
}
