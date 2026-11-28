//! ASL3-compatible HTTPS node registration outside audio callbacks.

use crossbeam_queue::ArrayQueue;
use rptadv_peer_support::sealed_records::SecretBytes;
use std::{
    io::Read,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const RESPONSE_LIMIT: u64 = 64 * 1024;
const EXCHANGE_BUDGET: Duration = Duration::from_secs(15);
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(50);

mod transport;
/// Release the shared DNS worker after the last registrar withdraws. A busy
/// resolver retains this frontend and its code until a later successful retry.
pub fn drain_resolver() -> bool {
    transport::drain_resolver()
}

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
    secret: Arc<SecretBytes>,
    observation: Option<Arc<AtomicU8>>,
}

impl RegistrationTarget {
    /// Publish this attempt's result only while its observation remains current.
    fn record_observation(&self, registered: bool) -> bool {
        let Some(observation) = &self.observation else {
            return true;
        };
        let state = if registered { REGISTERED } else { RETRY };
        observation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
                (previous != WITHDRAWN).then_some(state)
            })
            .is_ok()
    }

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

/// Owns the bounded, blocking HTTP work on one non-audio worker thread.
pub struct RegistrationWorker {
    updates: Arc<ArrayQueue<Vec<RegistrationTarget>>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RegistrationWorker {
    /// Start registration immediately for the supplied node set.
    pub fn start(targets: Vec<RegistrationTarget>) -> std::io::Result<Self> {
        Self::start_with(targets, |target| {
            exchange(target, Instant::now() + EXCHANGE_BUDGET, transport::agent)
        })
    }

    fn start_with(
        targets: Vec<RegistrationTarget>,
        register: impl Fn(&RegistrationTarget) -> Result<Duration, RegistrationError> + Send + 'static,
    ) -> std::io::Result<Self> {
        let updates = Arc::new(ArrayQueue::new(1));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("rpt-iax-registration".into())
            .spawn({
                let updates = Arc::clone(&updates);
                let stopping = Arc::clone(&stopping);
                move || registration_loop(updates, stopping, targets, register)
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
    register: impl Fn(&RegistrationTarget) -> Result<Duration, RegistrationError>,
) {
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
        let target = &targets[index];
        let interval = Duration::from_secs(target.interval_seconds);

        let result = register(target);
        if stopping.load(Ordering::Acquire) {
            return;
        }
        if !target.record_observation(result.is_ok()) {
            return;
        }
        let refresh = match result {
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

/// A zero remaining duration expires the exchange, including an answer at its deadline.
fn positive(remaining: Duration) -> std::io::Result<Duration> {
    if remaining.is_zero() {
        return Err(std::io::ErrorKind::TimedOut.into());
    }
    Ok(remaining)
}

/// Accept only timely addresses, preserving the system resolver's candidate order.
fn resolve_registration(
    netloc: &str,
    remaining: impl Fn() -> Duration,
    lookup: impl FnOnce(&str) -> std::io::Result<Vec<SocketAddr>>,
) -> std::io::Result<Vec<SocketAddr>> {
    positive(remaining())?;
    let addresses = match netloc.parse::<SocketAddr>() {
        Ok(address) => vec![address],
        Err(_) => lookup(netloc)?,
    };
    positive(remaining())?;
    Ok(addresses)
}

/// One attempt owns its request and response through the same monotonic deadline.
fn exchange(
    target: &RegistrationTarget,
    deadline: Instant,
    make_agent: impl FnOnce(Instant) -> std::io::Result<ureq::Agent>,
) -> Result<Duration, RegistrationError> {
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let io_error = |error: std::io::Error| RegistrationError::Read(error.kind());
    positive(remaining()).map_err(io_error)?;
    let body = request_body(target);
    let agent = make_agent(deadline).map_err(io_error)?;
    let response = agent
        .post(&target.url)
        .timeout(positive(remaining()).map_err(io_error)?)
        .set("Content-Type", "application/json")
        .send_bytes(body.expose_bytes());
    drop(body);
    let response = response.map_err(|error| match error {
        ureq::Error::Status(status, _) => RegistrationError::HttpStatus(status),
        ureq::Error::Transport(transport) => RegistrationError::Transport(transport.kind()),
    })?;
    if !(200..300).contains(&response.status()) {
        return Err(RegistrationError::HttpStatus(response.status()));
    }
    read_registration_response(
        response.into_reader(),
        target.interval_seconds,
        target.node(),
        remaining,
    )
}

fn read_registration_response(
    mut reader: impl Read,
    fallback_seconds: u64,
    node: &str,
    remaining: impl Fn() -> Duration,
) -> Result<Duration, RegistrationError> {
    let capacity = RESPONSE_LIMIT as usize + 1;
    let response_body = SecretBytes::try_fill(capacity, |out| {
        // Keep the response and the single overflow byte in the same zeroizing allocation.
        out.resize(capacity, 0);
        let mut used = 0;
        while used < capacity {
            positive(remaining()).map_err(|error| RegistrationError::Read(error.kind()))?;
            match reader.read(&mut out[used..]) {
                Ok(0) => {
                    positive(remaining()).map_err(|error| RegistrationError::Read(error.kind()))?;
                    out.truncate(used);
                    return Ok(());
                }
                Ok(count) => used += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(RegistrationError::Read(error.kind())),
            }
        }
        Err(RegistrationError::InvalidResponse)
    })?;
    let refresh = response_refresh(response_body.expose_bytes(), fallback_seconds, node)?;
    positive(remaining()).map_err(|error| RegistrationError::Read(error.kind()))?;
    Ok(refresh)
}

fn request_body(target: &RegistrationTarget) -> SecretBytes {
    use std::io::Write;
    // A JSON string expands by at most six bytes per UTF-8 byte. Reserve once
    // before writing any password so reallocation cannot release unwiped bytes.
    let password =
        std::str::from_utf8(target.secret.expose_bytes()).expect("validated registrar password");
    let capacity = 128 + 12 * target.node.len() + 6 * password.len();
    SecretBytes::try_fill(capacity, |out| -> std::io::Result<()> {
        write!(
            out,
            "{{\"port\":{},\"data\":{{\"nodes\":{{",
            target.local_port
        )?;
        serde_json::to_writer(&mut *out, &target.node)?;
        out.extend_from_slice(b":{\"node\":");
        serde_json::to_writer(&mut *out, &target.node)?;
        out.extend_from_slice(b",\"passwd\":");
        serde_json::to_writer(&mut *out, password)?;
        out.extend_from_slice(b",\"remote\":0}}}}");
        Ok(())
    })
    .expect("writing a bounded registrar request to owned memory")
}

const DORMANT: u8 = 0;
const PENDING: u8 = 1;
const REGISTERED: u8 = 2;
const RETRY: u8 = 3;
const WITHDRAWN: u8 = 4;

/// One explicitly selected, encrypted-record-bound HTTPS registration attempt.
/// Each restoration creates a fresh attempt, even if it selects the same record.
pub struct ManagedRegistrar {
    identity: RegistrarIdentity,
    target: RegistrationTarget,
    state: Arc<AtomicU8>,
    worker: Option<RegistrationWorker>,
}

/// Opaque identities from one authenticated product selection; the record version
/// is never rendered in HTTP, status, errors or logs.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RegistrarIdentity {
    /// Registered station UUID.
    pub station: [u8; 16],
    /// Explicit AllStarLink resource UUID.
    pub account: [u8; 16],
    /// Exact version of the account-bound encrypted registrar credential.
    pub record_version: [u8; 16],
    /// Native adoption serial retained by this attempt.
    pub attempt: std::num::NonZeroU64,
}

/// Sanitized remote state, independent of local listener adoption.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RegistrarState {
    /// Prepared without starting network work.
    Dormant,
    /// A first request is scheduled or in flight.
    Pending,
    /// The selected registrar confirmed this attempt.
    Registered,
    /// A failed request will retry at the configured interval.
    Retry,
    /// This attempt cannot publish another remote result.
    Withdrawn,
}

impl ManagedRegistrar {
    /// Retain the previous selected target for reversal with a distinct result owner.
    pub(crate) fn replacement(&self, attempt: std::num::NonZeroU64) -> Self {
        let state = Arc::new(AtomicU8::new(DORMANT));
        let mut target = self.target.clone();
        target.observation = Some(state.clone());
        Self {
            identity: RegistrarIdentity {
                attempt,
                ..self.identity
            },
            target,
            state,
            worker: None,
        }
    }
    /// Borrow the resolved HTTPS endpoint and its own authenticated password.
    /// Missing or masked credentials select no registrar. No peer dial uses these bytes.
    pub fn prepare(
        identity: RegistrarIdentity,
        node: std::num::NonZeroU64,
        endpoint: Option<&str>,
        local_port: std::num::NonZeroU16,
        interval_seconds: u16,
        password: Option<&str>,
    ) -> Option<Self> {
        let endpoint = endpoint.filter(|url| url.starts_with("https://"))?;
        let password = password.filter(|value| !value.is_empty())?;
        let state = Arc::new(AtomicU8::new(DORMANT));
        Some(Self {
            identity,
            target: RegistrationTarget {
                node: node.to_string(),
                url: endpoint.to_owned(),
                local_port: local_port.get(),
                interval_seconds: u64::from(interval_seconds),
                secret: Arc::new(SecretBytes::new(password.as_bytes().to_vec())),
                observation: Some(Arc::clone(&state)),
            },
            state,
            worker: None,
        })
    }

    /// Match a status or release to this attempt and private record.
    pub fn selected(&self, identity: RegistrarIdentity) -> bool {
        self.identity == identity
    }

    /// Start the selected local worker after admission. Remote HTTP success is asynchronous.
    pub fn start(&mut self) -> std::io::Result<()> {
        self.start_with(RegistrationWorker::start)
    }

    fn start_with(
        &mut self,
        start: impl FnOnce(Vec<RegistrationTarget>) -> std::io::Result<RegistrationWorker>,
    ) -> std::io::Result<()> {
        if self.worker.is_some() || self.state.load(Ordering::Acquire) != DORMANT {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.state.store(PENDING, Ordering::Release);
        match start(vec![self.target.clone()]) {
            Ok(worker) => {
                self.worker = Some(worker);
                Ok(())
            }
            Err(error) => {
                self.state.store(DORMANT, Ordering::Release);
                Err(error)
            }
        }
    }

    /// Observe only this attempt, including when an older request finishes late.
    pub fn state(&self) -> RegistrarState {
        match self.state.load(Ordering::Acquire) {
            DORMANT => RegistrarState::Dormant,
            PENDING => RegistrarState::Pending,
            REGISTERED => RegistrarState::Registered,
            RETRY => RegistrarState::Retry,
            _ => RegistrarState::Withdrawn,
        }
    }

    /// Fence new results and refreshes. Retain this owner while bounded HTTP work drains.
    pub fn withdraw(&mut self) -> bool {
        self.state.store(WITHDRAWN, Ordering::Release);
        if let Some(worker) = &mut self.worker {
            worker.stopping.store(true, Ordering::Release);
            if worker
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
            {
                return false;
            }
            self.worker.take();
        }
        true
    }
}

fn response_refresh(
    body: &[u8],
    fallback_seconds: u64,
    node: &str,
) -> Result<Duration, RegistrationError> {
    let response =
        rptadv_json::decode_value(body).map_err(|_| RegistrationError::InvalidResponse)?;
    let data = response
        .get("data")
        .ok_or(RegistrationError::InvalidResponse)?;
    let decoded;
    let detail = match data {
        serde_json::Value::String(value) if value.trim_start().starts_with(['{', '[', '"']) => {
            decoded = rptadv_json::decode_value(value.as_bytes())
                .map_err(|_| RegistrationError::InvalidResponse)?;
            &decoded
        }
        value => value,
    };
    let registered = match detail {
        // One request registers one node. The public registrar returns its status in an array.
        serde_json::Value::Array(rows) => match rows.as_slice() {
            [serde_json::Value::String(row)] => row
                .strip_prefix(node)
                .and_then(|suffix| suffix.strip_prefix(" successfully registered @"))
                .map(|address| address.strip_suffix('.').unwrap_or(address))
                .and_then(|address| address.parse::<SocketAddr>().ok())
                .is_some_and(|address| address.port() != 0),
            _ => false,
        },
        _ => detail
            .as_str()
            .or_else(|| detail.get("result")?.as_str())
            .is_some_and(|text| text.trim().eq_ignore_ascii_case("successfully registered")),
    };
    if !registered {
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
fn post_registration(
    agent: &ureq::Agent,
    target: &RegistrationTarget,
) -> Result<Duration, RegistrationError> {
    post_registration_until(agent, target, Instant::now() + EXCHANGE_BUDGET)
}

#[cfg(test)]
fn post_registration_until(
    agent: &ureq::Agent,
    target: &RegistrationTarget,
    deadline: Instant,
) -> Result<Duration, RegistrationError> {
    exchange(target, deadline, |_| Ok(agent.clone()))
}

#[cfg(test)]
mod tests {
    use super::{RegistrationError, RegistrationTarget, request_body};
    use std::time::Duration;

    const TEST_NODE: &str = "524950";

    fn response_refresh(body: &[u8], fallback_seconds: u64) -> Result<Duration, RegistrationError> {
        super::response_refresh(body, fallback_seconds, TEST_NODE)
    }

    fn target() -> RegistrationTarget {
        RegistrationTarget {
            node: TEST_NODE.into(),
            url: "https://register.example/".into(),
            local_port: 4569,
            interval_seconds: 60,
            secret: std::sync::Arc::new(rptadv_peer_support::sealed_records::SecretBytes::new(
                b"test-secret".to_vec(),
            )),
            observation: None,
        }
    }

    #[test]
    fn request_uses_the_asl_https_registration_json_shape() {
        let body = request_body(&target());
        let request: serde_json::Value = serde_json::from_slice(body.expose_bytes()).unwrap();

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
    fn registrar_uses_only_the_authoritative_result() {
        for body in [
            br#"{"data":"not successfully registered"}"#.as_slice(),
            br#"{"data":{"result":"denied","detail":"successfully registered"}}"#,
            br#"{"data":"{\"result\":\"denied\",\"detail\":\"successfully registered\"}"}"#,
            br#"{"data":{"detail":"successfully registered"}}"#,
        ] {
            assert!(response_refresh(body, 60).is_err());
        }

        let accepted =
            br#"{"data":{"result":"successfully registered","detail":"previously denied"}}"#;
        assert_eq!(response_refresh(accepted, 60), Ok(Duration::from_secs(60)));
    }

    #[test]
    fn registrar_reads_through_eof_at_the_response_limit() {
        use std::io::Cursor;

        let limit = super::RESPONSE_LIMIT as usize;
        for length in [limit - 1, limit, limit + 1, limit + 17] {
            let mut body = br#"{"refresh":47,"data":"successfully registered"}"#.to_vec();
            body.resize(length, b' ');
            let mut reader = Cursor::new(body);
            let result = super::read_registration_response(&mut reader, 60, TEST_NODE, || {
                super::EXCHANGE_BUDGET
            });

            if length <= limit {
                assert_eq!(result, Ok(Duration::from_secs(47)));
            } else {
                assert!(result.is_err());
            }
            assert_eq!(reader.position(), length.min(limit + 1) as u64);
        }
    }

    #[test]
    fn expired_registrar_exchange_skips_lookup() {
        use std::{
            io,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::Instant,
        };

        let called = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&called);
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .resolver(move |_: &str| -> io::Result<Vec<std::net::SocketAddr>> {
                seen.store(true, Ordering::Relaxed);
                Err(io::ErrorKind::NotFound.into())
            })
            .build();
        let mut configured = target();
        configured.url = "https://fixture.invalid/".into();
        let result = super::post_registration_until(&agent, &configured, Instant::now());
        assert!(result.is_err());
        assert!(
            !called.load(Ordering::Relaxed),
            "an expired request entered DNS"
        );
    }

    #[test]
    fn registrar_discards_a_lookup_that_consumed_the_exchange_budget() {
        use std::{cell::Cell, io::ErrorKind, net::SocketAddr};

        let address = SocketAddr::from(([127, 0, 0, 1], 443));
        for (before_ms, after_ms) in [(5, 2), (5, 0), (0, 0)] {
            let remaining = Cell::new(Duration::from_millis(before_ms));
            let calls = Cell::new(0);
            let result = super::resolve_registration(
                "fixture.invalid:443",
                || remaining.get(),
                |netloc| {
                    assert_eq!(netloc, "fixture.invalid:443");
                    calls.set(calls.get() + 1);
                    remaining.set(Duration::from_millis(after_ms));
                    Ok(vec![address])
                },
            );
            assert_eq!(calls.get(), if before_ms == 0 { 0 } else { 1 });
            if after_ms == 0 {
                assert_eq!(
                    result.err().map(|error| error.kind()),
                    Some(ErrorKind::TimedOut),
                    "an expired lookup supplied usable addresses"
                );
            } else {
                assert_eq!(result.unwrap(), vec![address]);
            }
        }
    }

    #[test]
    fn registrar_response_consumes_the_budget_left_after_lookup() {
        use std::{
            cell::Cell,
            io::{self, Read},
        };

        struct Body<'a> {
            bytes: &'a [u8],
            remaining_ms: &'a Cell<u64>,
            reads: usize,
        }
        impl Read for Body<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                self.remaining_ms
                    .set(self.remaining_ms.get().saturating_sub(1));
                let size = output.len().min(24);
                self.bytes.read(&mut output[..size])
            }
        }

        // Two in-memory data reads and one EOF read, each costing one logical ms.
        let bytes = br#"{"refresh":47,"data":"successfully registered"}"#;
        for (after_lookup_ms, permitted_reads, on_time) in
            [(4, 3, true), (2, 2, false), (3, 3, false)]
        {
            let remaining_ms = Cell::new(5);
            let remaining = || Duration::from_millis(remaining_ms.get());
            super::resolve_registration("fixture.invalid:443", remaining, |_| {
                remaining_ms.set(after_lookup_ms);
                Ok(vec![std::net::SocketAddr::from(([127, 0, 0, 1], 443))])
            })
            .unwrap();
            let mut body = Body {
                bytes,
                remaining_ms: &remaining_ms,
                reads: 0,
            };
            let result = super::read_registration_response(&mut body, 60, TEST_NODE, remaining);
            assert_eq!(
                body.reads, permitted_reads,
                "response read continued after expiry"
            );
            if on_time {
                assert_eq!(result, Ok(Duration::from_secs(47)));
            } else {
                assert!(
                    result.is_err(),
                    "a successful response exceeded the exchange deadline"
                );
            }
        }
    }

    #[test]
    fn registrar_withdrawal_fences_late_success_and_retry() {
        use super::{ManagedRegistrar, RegistrarIdentity, RegistrarState};

        let identity = RegistrarIdentity {
            station: [1; 16],
            account: [2; 16],
            record_version: [3; 16],
            attempt: 1_u64.try_into().unwrap(),
        };
        let mut registrar = ManagedRegistrar::prepare(
            identity,
            716701_u64.try_into().unwrap(),
            Some("https://fixture.invalid/"),
            4569_u16.try_into().unwrap(),
            60,
            Some("fixture-only"),
        )
        .unwrap();
        let dispatched = registrar.target.clone();
        registrar
            .state
            .store(super::PENDING, super::Ordering::Release);
        assert!(dispatched.record_observation(false));
        assert_eq!(registrar.state(), RegistrarState::Retry);
        assert!(registrar.withdraw());
        for success in [true, false] {
            assert!(!dispatched.record_observation(success));
            assert_eq!(registrar.state(), RegistrarState::Withdrawn);
        }
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
    fn response_preserves_literal_json_member_names_in_encoded_detail() {
        assert_eq!(
            response_refresh(
                br#"{"refresh":45,"data":"{\"$serde_json::private::Number\":\"1\",\"result\":\"successfully regi\\u0073tered\"}"}"#,
                60,
            ),
            Ok(Duration::from_secs(45))
        );
    }

    #[test]
    fn response_rejects_malformed_encoded_details_with_success_text() {
        for body in [
            br#"{"data":" \t{\"result\":\"successfully registered\",\"result\":\"denied\"}"}"#
                .as_slice(),
            br#"{"data":"[\"successfully registered\""}"#,
        ] {
            assert_eq!(
                response_refresh(body, 60),
                Err(RegistrationError::InvalidResponse)
            );
        }
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
            br#"{"refresh":{"$serde_json::private::Number":"45"},"data":"successfully registered"}"#,
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
        let source = include_str!("mod.rs").split("#[cfg(test)]").next().unwrap();

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
    fn registration_accepts_the_selected_node_status_from_asl() {
        for body in [
            br#"{"data":["524951 successfully registered @192.0.2.15:4569."]}"#.as_slice(),
            br#"{"data":["524950 failed authentication. Please check your password and node number."]}"#,
            br#"{"data":["524950 successfully registered @not-a-socket."]}"#,
        ] {
            assert_eq!(post_loopback_response(200, body), Err(RegistrationError::Rejected));
        }

        let body = br#"{"ipaddr":"192.0.2.15","port":4569,"refresh":179,"data":["524950 successfully registered @192.0.2.15:4569."]}"#;
        assert_eq!(
            post_loopback_response(200, body),
            Ok(Duration::from_secs(179)),
        );
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
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        let started = Instant::now();
        assert!(matches!(
            super::post_registration_until(&agent, &target, started + Duration::from_millis(50)),
            Err(RegistrationError::Read(
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ))
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
    fn dormant_registrar_and_withdrawn_inflight_result_have_distinct_owners() {
        use super::{ManagedRegistrar, RegistrarState};
        use std::{io::Write, net::TcpListener, sync::mpsc, thread, time::Instant};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let node = 716701_u64.try_into().unwrap();
        let port = 4569_u16.try_into().unwrap();
        let endpoint = Some("https://registrar.example/");
        let owner = super::RegistrarIdentity {
            station: [1; 16],
            account: [2; 16],
            record_version: [3; 16],
            attempt: 1_u64.try_into().unwrap(),
        };
        assert!(ManagedRegistrar::prepare(owner, node, endpoint, port, 60, None).is_none());
        let mut target =
            ManagedRegistrar::prepare(owner, node, endpoint, port, 60, Some("fixture-only"))
                .unwrap();
        assert!(target.selected(owner));
        assert!(!target.selected(super::RegistrarIdentity {
            record_version: [4; 16],
            ..owner
        }));
        assert_eq!(target.state(), RegistrarState::Dormant);
        // Exercise the existing local HTTP receiver without external DNS or a trust-store edit.
        target.target.url = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();
        let (received, request) = mpsc::sync_channel(1);
        let (release, waiting) = mpsc::sync_channel(1);
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: serde_json::Value =
                serde_json::from_slice(&read_registration_body(&mut stream)).unwrap();
            assert_eq!(request["port"], 4569);
            let row = &request["data"]["nodes"]["716701"];
            assert_eq!(row["node"], "716701");
            assert!(row["passwd"] == "fixture-only");
            received.send(()).unwrap();
            waiting.recv_timeout(Duration::from_secs(3)).unwrap();
            let body = br#"{"refresh":1,"data":"successfully registered"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        target.start().unwrap();
        request.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(target.state(), RegistrarState::Pending);
        assert!(!target.withdraw());
        release.send(()).unwrap();
        receiver.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !target.withdraw() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(target.state(), RegistrarState::Withdrawn);
        assert!(target.start().is_err());
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
        drop(read_registration_body(stream));
    }

    fn read_registration_body(stream: &mut std::net::TcpStream) -> Vec<u8> {
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
        body
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
