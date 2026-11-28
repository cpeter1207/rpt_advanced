//! Deadline-bound HTTPS transport and credential-free system address resolution.

use super::{positive, resolve_registration};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type Addresses = io::Result<Vec<SocketAddr>>;

fn time_left(deadline: Instant) -> io::Result<Duration> {
    positive(deadline.saturating_duration_since(Instant::now()))
}

struct Lookup {
    netloc: String,
    deadline: Instant,
    reply: mpsc::SyncSender<Addresses>,
}

/// Exactly one active system lookup and one queue slot across registrar attempts.
struct DnsWorker {
    jobs: mpsc::SyncSender<Lookup>,
    _thread: JoinHandle<()>,
}

impl DnsWorker {
    fn start(lookup: impl Fn(&str) -> Addresses + Send + 'static) -> io::Result<Self> {
        let (jobs, incoming) = mpsc::sync_channel::<Lookup>(1);
        let handle = thread::Builder::new()
            .name("rpt-registrar-dns".into())
            .spawn(move || {
                for job in incoming {
                    let result = time_left(job.deadline).and_then(|_| lookup(&job.netloc));
                    let _ = job.reply.try_send(result);
                }
            })?;
        Ok(Self {
            jobs,
            _thread: handle,
        })
    }

    fn submit(&self, netloc: &str, deadline: Instant) -> io::Result<mpsc::Receiver<Addresses>> {
        time_left(deadline)?;
        let (reply, result) = mpsc::sync_channel(1);
        let job = Lookup {
            netloc: netloc.to_owned(),
            deadline,
            reply,
        };
        self.jobs.try_send(job).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => io::ErrorKind::WouldBlock,
            mpsc::TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
        })?;
        Ok(result)
    }
}

fn receive_addresses(result: mpsc::Receiver<Addresses>, deadline: Instant) -> Addresses {
    result
        .recv_timeout(time_left(deadline)?)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut,
            mpsc::RecvTimeoutError::Disconnected => io::ErrorKind::BrokenPipe,
        })?
}

/// Shared only by registrars in this frontend. Shutdown drops the sender and
/// retains the thread until a pending system resolver call really returns.
#[derive(Default)]
struct Resolver {
    active: Option<DnsWorker>,
    retiring: Option<JoinHandle<()>>,
}
static RESOLVER: Mutex<Resolver> = Mutex::new(Resolver {
    active: None,
    retiring: None,
});
fn system_addresses(netloc: &str, deadline: Instant) -> Addresses {
    let result = {
        let mut resolver = RESOLVER.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
        if resolver.retiring.is_some() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if resolver.active.is_none() {
            resolver.active = Some(DnsWorker::start(|netloc| {
                netloc.to_socket_addrs().map(Iterator::collect)
            })?);
        }
        resolver
            .active
            .as_ref()
            .expect("started resolver")
            .submit(netloc, deadline)?
    };
    receive_addresses(result, deadline)
}
impl Resolver {
    fn drain(&mut self) -> bool {
        if let Some(worker) = self.active.take() {
            drop(worker.jobs);
            self.retiring = Some(worker._thread);
        }
        if self
            .retiring
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return false;
        }
        self.retiring
            .take()
            .is_none_or(|handle| handle.join().is_ok())
    }
}
pub(super) fn drain_resolver() -> bool {
    RESOLVER
        .try_lock()
        .is_ok_and(|mut resolver| resolver.drain())
}

/// ureq's pinned HTTPS path supplies the raw TCP stream here before its rustls handshake.
struct DeadlineIo {
    inner: Box<dyn ureq::ReadWrite>,
    deadline: Instant,
}

impl std::fmt::Debug for DeadlineIo {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("registrar TLS transport")
    }
}

impl DeadlineIo {
    fn remaining_at(&self, now: Instant) -> io::Result<Duration> {
        positive(self.deadline.saturating_duration_since(now))
    }

    fn set_timeout(
        &self,
        set: fn(&TcpStream, Option<Duration>) -> io::Result<()>,
    ) -> io::Result<()> {
        let remaining = self.remaining_at(Instant::now())?;
        if let Some(socket) = self.inner.socket() {
            set(socket, Some(remaining))?;
        }
        Ok(())
    }
}

impl Read for DeadlineIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.set_timeout(TcpStream::set_read_timeout)?;
        self.inner.read(bytes)
    }
}

impl Write for DeadlineIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.set_timeout(TcpStream::set_write_timeout)?;
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.set_timeout(TcpStream::set_write_timeout)?;
        self.inner.flush()
    }
}

impl ureq::ReadWrite for DeadlineIo {
    fn socket(&self) -> Option<&TcpStream> {
        self.inner.socket()
    }
}

struct DeadlineTls {
    deadline: Instant,
    config: Arc<ureq::rustls::ClientConfig>,
}

impl ureq::TlsConnector for DeadlineTls {
    fn connect(
        &self,
        name: &str,
        io: Box<dyn ureq::ReadWrite>,
    ) -> Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
        let timed = DeadlineIo {
            inner: io,
            deadline: self.deadline,
        };
        ureq::TlsConnector::connect(&self.config, name, Box::new(timed))
    }
}

fn tls_config() -> Arc<ureq::rustls::ClientConfig> {
    use ureq::rustls::{ClientConfig, RootCertStore, crypto::ring, version};
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        // Match ureq 2.12.1's default ring provider, protocol versions and public roots.
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(&[&version::TLS12, &version::TLS13])
            .expect("ring supports both selected TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(config)
    }))
}

pub(super) fn agent(deadline: Instant) -> io::Result<ureq::Agent> {
    agent_with(deadline, system_addresses)
}

fn agent_with(
    deadline: Instant,
    lookup: impl Fn(&str, Instant) -> Addresses + Send + Sync + 'static,
) -> io::Result<ureq::Agent> {
    let tls = DeadlineTls {
        deadline,
        config: tls_config(),
    };
    let builder = ureq::AgentBuilder::new()
        .redirects(0)
        .tls_connector(Arc::new(tls))
        .resolver(move |netloc: &str| {
            resolve_registration(
                netloc,
                || deadline.saturating_duration_since(Instant::now()),
                |host| lookup(host, deadline),
            )
        });
    Ok(builder.timeout_connect(time_left(deadline)?).build())
}

#[cfg(test)]
mod tests;
