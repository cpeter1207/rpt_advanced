//! Mandatory mTLS loopback transport. Only the host's serialized executor claims requests.
mod http;
#[cfg(test)]
mod review_tests;
mod status;
mod trust;
pub use self::http::OriginPolicy;
pub use status::{StatusPublisher, StatusResyncReason, StatusStream};
pub use trust::{ServerIdentity, ServiceId, ServiceTrust};

use std::collections::VecDeque;
use std::fmt;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub use hyper::http::uri::PathAndQuery;
pub use hyper::{HeaderMap, Method, Response, StatusCode};
use rustls::pki_types::UnixTime;
pub use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, sign::SingleCertAndKey};
use tokio::sync::{Notify, oneshot};
use zeroize::Zeroizing;

const CONNECTIONS: usize = 16;
const HANDSHAKES: usize = 4;
const QUEUE: usize = 8;
const ADMISSION: Duration = Duration::from_secs(2);
const REPLY: Duration = Duration::from_secs(15);

/// Value-free host setup errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// Invalid authority, origin, address or service identity.
    Policy,
    /// Invalid server identity or trust store.
    Tls,
    /// The socket, runtime or worker could not start.
    Network,
}

/// Credential-bearing request bytes, wiped on every drop path.
pub struct RequestBody(Zeroizing<Vec<u8>>);
impl RequestBody {
    /// Borrow the original bytes for the bounded resource decoder.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequestBody([redacted])")
    }
}

/// An ephemeral dispatch correlation identifier scoped to this listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryId(u64);

/// Identity and liveness of one authenticated TLS connection.
#[derive(Clone)]
pub struct ConnectionProof(Arc<AtomicBool>);

impl ConnectionProof {
    /// True until the TLS task exits, including shutdown and timeout.
    pub fn is_alive(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    /// True only for requests using this exact connection.
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct ConnectionOwner(ConnectionProof);
impl Drop for ConnectionOwner {
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}

/// Stable per-request browser identity asserted by the authenticated, authorized proxy.
#[derive(Clone, Eq, PartialEq)]
pub struct BrowserPrincipal {
    issuer: String,
    subject: String,
}

impl fmt::Debug for BrowserPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrowserPrincipal([redacted])")
    }
}

/// A request owned by the coordinator after admission, independently of its client.
pub struct AdmittedRequest {
    /// Transport correlation only. Durable submission identity belongs to the resource.
    pub id: DeliveryId,
    /// Currently authorized TLS service.
    pub actor: ServiceId,
    /// Authenticated connection which delivered this request.
    pub connection: ConnectionProof,
    /// Original HTTP method.
    pub method: Method,
    /// Canonical origin-form path and query.
    pub target: PathAndQuery,
    /// Conditional, representation and negotiation headers only.
    pub headers: HeaderMap,
    /// Owned bounded input.
    pub body: RequestBody,
    browser_principal: Option<BrowserPrincipal>,
    status_upgrade: bool,
    reply: oneshot::Sender<CoordinatorReply>,
}

enum CoordinatorReply {
    Http(Response<Vec<u8>>),
    Status(StatusStream),
}

impl AdmittedRequest {
    /// Complete once. A client disconnect only discards the response.
    pub fn respond(self, response: Response<Vec<u8>>) {
        let _ = self.reply.send(CoordinatorReply::Http(response));
    }

    /// Exact issuer/subject of this request's verified browser, when the proxy asserted it.
    pub fn browser_principal(&self) -> Option<&BrowserPrincipal> {
        self.browser_principal.as_ref()
    }

    /// Whether the request offered the authorized status WebSocket subprotocol.
    pub fn is_status_upgrade(&self) -> bool {
        self.status_upgrade
    }

    /// Upgrade this request after selection validation and capture registration.
    pub fn respond_status_stream(self, stream: StatusStream) {
        if !self.status_upgrade {
            self.respond(http::empty(StatusCode::BAD_REQUEST));
        } else if !stream.ready() {
            self.respond(http::empty(StatusCode::SERVICE_UNAVAILABLE));
        } else {
            let _ = self.reply.send(CoordinatorReply::Status(stream));
        }
    }
}

struct Snapshot {
    config: Arc<ServerConfig>,
    trust: Arc<ServiceTrust>,
}
impl Snapshot {
    fn new(server: &Arc<SingleCertAndKey>, trust: ServiceTrust) -> Result<Self, TransportError> {
        let trust = Arc::new(trust);
        Ok(Self {
            config: Arc::new(trust::configure(server.clone(), trust.clone())?),
            trust,
        })
    }
}

type Chain = Arc<[CertificateDer<'static>]>;
// WebPKI measures certificate validity in whole Unix seconds. The network owner
// verifies each possible claim second rather than parsing X.509 a second way.
struct ClaimProof {
    trust: Arc<ServiceTrust>,
    first: u64,
    last: u64,
}
impl ClaimProof {
    fn verify(
        trust: Arc<ServiceTrust>,
        chain: &Chain,
        actor: &ServiceId,
        requires_browser_proxy: bool,
    ) -> Option<Self> {
        if requires_browser_proxy && !trust.allows_browser_principal(chain) {
            return None;
        }
        let first = UnixTime::now().as_secs();
        let last = (first..=first.saturating_add(ADMISSION.as_secs()))
            .take_while(|&second| {
                trust
                    .identify(
                        chain,
                        UnixTime::since_unix_epoch(Duration::from_secs(second)),
                    )
                    .as_ref()
                    == Some(actor)
            })
            .last()?;
        Some(Self { trust, first, last })
    }
}
struct Pending {
    request: AdmittedRequest,
    chain: Chain,
    proof: ClaimProof,
    deadline: Instant,
}
struct State {
    snapshot: Option<Snapshot>,
    pending: VecDeque<Pending>,
    last_id: u64,
    refresh: Arc<Notify>,
}
type Shared = Arc<Mutex<State>>;

impl State {
    fn trust(&self) -> Option<Arc<ServiceTrust>> {
        self.snapshot.as_ref().map(|s| s.trust.clone())
    }
    fn uses(&self, trust: &Arc<ServiceTrust>) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.trust, trust))
    }
    fn close(&mut self) {
        self.snapshot = None;
        self.pending.clear();
    }
}

/// The serialized host polls this receiver from its existing control-plane executor.
pub struct CoordinatorInbox(Shared);
impl CoordinatorInbox {
    /// Claim the next live request. Withdrawal, expiry and failed admission have no domain effect.
    pub fn try_recv(&mut self) -> Option<AdmittedRequest> {
        let mut state = match self.0.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
        };
        while let Some(front) = state.pending.front() {
            if !state.uses(&front.proof.trust) {
                // The worker refreshes queued proofs outside the queue lock on reload.
                return None;
            }
            let pending = state.pending.pop_front().expect("front was present");
            if pending.request.reply.is_closed() {
                continue;
            }
            if Instant::now() >= pending.deadline {
                pending
                    .request
                    .respond(http::empty(StatusCode::SERVICE_UNAVAILABLE));
            } else if !(pending.proof.first..=pending.proof.last)
                .contains(&UnixTime::now().as_secs())
            {
                pending.request.respond(http::empty(StatusCode::FORBIDDEN));
            } else {
                return Some(pending.request);
            }
        }
        None
    }
}
impl Drop for CoordinatorInbox {
    fn drop(&mut self) {
        self.0.lock().expect("management state").close();
    }
}

/// Value-free listener events for host logging. Repeated failures are coalesced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TransportEvent {
    /// Log an error when accepting new TCP connections first fails.
    AcceptFailed = 1,
    /// Log recovery when the listener next accepts a connection.
    AcceptRecovered = 2,
    /// Log an error when a connection worker exits abnormally.
    ConnectionTaskFailed = 4,
}
impl TransportEvent {
    /// Static diagnostic suitable for the host's logging facility.
    pub fn message(self) -> &'static str {
        match self {
            Self::AcceptFailed => "management listener failed to accept connections",
            Self::AcceptRecovered => "management listener resumed accepting connections",
            Self::ConnectionTaskFailed => "management connection task failed",
        }
    }
}
const ACCEPT_UNAVAILABLE: u8 = 8;
/// Last observable availability and coalesced events since the previous host poll.
pub struct ListenerDiagnostics(u8);
impl ListenerDiagnostics {
    /// Whether the last attempted accept failed.
    pub fn accept_unavailable(&self) -> bool {
        self.0 & ACCEPT_UNAVAILABLE != 0
    }
    /// Events to log, independent of their final availability state.
    pub fn events(&self) -> impl Iterator<Item = TransportEvent> {
        [
            TransportEvent::AcceptFailed,
            TransportEvent::AcceptRecovered,
            TransportEvent::ConnectionTaskFailed,
        ]
        .into_iter()
        .filter(|event| self.0 & (*event as u8) != 0)
    }
}
#[derive(Default)]
struct Health(AtomicU8);
impl Health {
    fn take(&self) -> ListenerDiagnostics {
        ListenerDiagnostics(self.0.fetch_and(ACCEPT_UNAVAILABLE, Ordering::AcqRel))
    }
    fn accept_failed(&self, failed: bool) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bits| {
                match (bits & ACCEPT_UNAVAILABLE != 0, failed) {
                    (false, true) => {
                        Some(bits | ACCEPT_UNAVAILABLE | TransportEvent::AcceptFailed as u8)
                    }
                    (true, false) => {
                        Some((bits & !ACCEPT_UNAVAILABLE) | TransportEvent::AcceptRecovered as u8)
                    }
                    _ => None,
                }
            });
    }
    fn joined(&self, result: Result<(), tokio::task::JoinError>) {
        if result.is_err() {
            self.0.fetch_or(
                TransportEvent::ConnectionTaskFailed as u8,
                Ordering::Release,
            );
        }
    }
}
/// A single dedicated networking thread. Dropping it closes admission and joins every connection.
pub struct Listener {
    address: SocketAddr,
    identity: Arc<SingleCertAndKey>,
    state: Shared,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    health: Arc<Health>,
}
impl Listener {
    /// Bind a loopback address only after constructing the required client-auth configuration.
    pub fn start(
        address: SocketAddr,
        identity: ServerIdentity,
        trust: ServiceTrust,
        policy: OriginPolicy,
    ) -> Result<(Self, CoordinatorInbox), TransportError> {
        if !address.ip().is_loopback() {
            return Err(TransportError::Policy);
        }
        let identity = identity.into_resolver()?;
        let snapshot = Snapshot::new(&identity, trust)?;
        let socket = TcpListener::bind(address).map_err(|_| TransportError::Network)?;
        socket
            .set_nonblocking(true)
            .map_err(|_| TransportError::Network)?;
        let address = socket.local_addr().map_err(|_| TransportError::Network)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| TransportError::Network)?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(socket).map_err(|_| TransportError::Network)?
        };
        let state = Arc::new(Mutex::new(State {
            snapshot: Some(snapshot),
            pending: VecDeque::new(),
            last_id: 0,
            refresh: Arc::new(Notify::new()),
        }));
        let network = state.clone();
        let (stop, stopped) = oneshot::channel();
        let health = Arc::new(Health::default());
        let network_health = health.clone();
        let worker = thread::Builder::new()
            .name("rpt-management".into())
            .spawn(move || {
                runtime.block_on(http::serve(
                    || listener.accept(),
                    network,
                    policy,
                    stopped,
                    network_health,
                ))
            })
            .map_err(|_| TransportError::Network)?;
        let inbox = CoordinatorInbox(state.clone());
        Ok((
            Self {
                address,
                identity,
                state,
                stop: Some(stop),
                worker: Some(worker),
                health,
            },
            inbox,
        ))
    }

    /// Take coalesced events and the current listener availability for host logging.
    pub fn take_diagnostics(&self) -> ListenerDiagnostics {
        self.health.take()
    }

    /// Actual local socket address, including a kernel-assigned port.
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Replace issuer trust and exact service pins together for new and open connections.
    pub fn replace_trust(&mut self, trust: ServiceTrust) -> Result<(), TransportError> {
        let replacement = Snapshot::new(&self.identity, trust)?;
        let mut state = self.state.lock().expect("management state");
        *state.snapshot.as_mut().ok_or(TransportError::Network)? = replacement;
        state.refresh.notify_one();
        Ok(())
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.state.lock().expect("management state").close();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Dropping the connection future cancels only work still waiting for its coordinator.
struct Waiting {
    id: DeliveryId,
    state: Shared,
}
impl Waiting {
    fn cancel(&self) -> bool {
        let mut state = self.state.lock().expect("management state");
        let Some(pos) = state.pending.iter().position(|p| p.request.id == self.id) else {
            return false;
        };
        state.pending.remove(pos);
        true
    }
}
impl Drop for Waiting {
    fn drop(&mut self) {
        self.cancel();
    }
}

// Runs only on the network worker. A newer replacement can supersede this pass.
fn refresh_pending(state: &Shared) {
    let (trust, pending) = {
        let queue = state.lock().expect("management state");
        let Some(trust) = queue.trust() else {
            return;
        };
        let pending: Vec<_> = queue
            .pending
            .iter()
            .filter(|p| !Arc::ptr_eq(&p.proof.trust, &trust))
            .map(|p| {
                (
                    p.request.id,
                    p.chain.clone(),
                    p.request.actor.clone(),
                    p.request.browser_principal.is_some(),
                )
            })
            .collect();
        (trust, pending)
    };
    for (id, chain, actor, requires_browser_proxy) in pending {
        let proof = ClaimProof::verify(trust.clone(), &chain, &actor, requires_browser_proxy);
        let mut queue = state.lock().expect("management state");
        if !queue.uses(&trust) {
            return;
        }
        let Some(index) = queue.pending.iter().position(|p| p.request.id == id) else {
            continue;
        };
        if let Some(proof) = proof {
            queue.pending[index].proof = proof;
        } else if let Some(rejected) = queue.pending.remove(index) {
            rejected.request.respond(http::empty(StatusCode::FORBIDDEN));
        }
    }
}

struct HttpInput {
    method: Method,
    target: PathAndQuery,
    headers: HeaderMap,
    body: RequestBody,
    browser_principal: Option<BrowserPrincipal>,
    status_upgrade: bool,
}

async fn dispatch(
    state: Shared,
    chain: Chain,
    identity: (ServiceId, ConnectionProof),
    input: HttpInput,
) -> CoordinatorReply {
    let error = |status| CoordinatorReply::Http(http::empty(status));
    let (actor, connection) = identity;
    let deadline = Instant::now() + ADMISSION;
    let trust = state.lock().expect("management state").trust();
    let Some(proof) = trust.and_then(|trust| {
        ClaimProof::verify(trust, &chain, &actor, input.browser_principal.is_some())
    }) else {
        return error(StatusCode::FORBIDDEN);
    };
    let (reply, mut result) = oneshot::channel();
    let waiting = {
        let mut state_guard = state.lock().expect("management state");
        if !state_guard.uses(&proof.trust) {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        }
        if state_guard.pending.len() >= QUEUE {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        }
        let Some(next) = state_guard.last_id.checked_add(1) else {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        };
        state_guard.last_id = next;
        let id = DeliveryId(next);
        let request = AdmittedRequest {
            id,
            actor,
            connection,
            method: input.method,
            target: input.target,
            headers: input.headers,
            body: input.body,
            browser_principal: input.browser_principal,
            status_upgrade: input.status_upgrade,
            reply,
        };
        state_guard.pending.push_back(Pending {
            request,
            chain,
            proof,
            deadline,
        });
        Waiting {
            id,
            state: state.clone(),
        }
    };
    // The queue lock decides whether the host claimed before the admission deadline.
    match tokio::time::timeout(ADMISSION, &mut result).await {
        Ok(reply) => reply.unwrap_or_else(|_| error(StatusCode::SERVICE_UNAVAILABLE)),
        Err(_) if waiting.cancel() => error(StatusCode::SERVICE_UNAVAILABLE),
        Err(_) => match tokio::time::timeout(REPLY, result).await {
            Ok(Ok(reply)) => reply,
            _ => error(StatusCode::GATEWAY_TIMEOUT),
        },
    }
}
