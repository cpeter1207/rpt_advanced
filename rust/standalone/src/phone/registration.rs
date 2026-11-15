//! One standalone SIP reactor, with preparation before product publication.
//! Contexts never leave their creating thread. The executable retains the provider
//! lease until that thread is joined; future unloadable hosts need their own bootstrap.
use crate::{
    configuration::PhoneAccountProtocol,
    phone_accounts::{PreparedAccount, PreparedAccounts},
};
use rptadv_peer_support::sip::registration::{Account, Provider, Settings, Status};
#[cfg(test)]
use std::path::Path;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt,
    rc::Rc,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::mpsc as mailbox;

#[path = "registration_calls.rs"]
pub(crate) mod calls;

pub(crate) const SERVICE_INTERVAL: Duration = Duration::from_millis(10);
// Native shutdown has a documented 30-second bound; allow scheduling headroom.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(35);
type Reply<T> = mpsc::SyncSender<Result<T, Error>>;
type Owner = Rc<RefCell<Entry>>;

/// Redacted startup, publication or cleanup failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Provider(rptadv_peer_support::sip::registration::Error),
    Owner,
    Shutdown,
    Preparation,
    Call,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(f),
            Self::Call => f.write_str("SIP call is unavailable"),
            Self::Owner => f.write_str("SIP registration owner is unavailable"),
            Self::Preparation => {
                f.write_str("phone account preparation is still running; resolver lease retained")
            }
            Self::Shutdown => f.write_str("SIP shutdown did not quiesce; provider lease retained"),
        }
    }
}
impl From<rptadv_peer_support::sip::registration::Error> for Error {
    fn from(error: rptadv_peer_support::sip::registration::Error) -> Self {
        Self::Provider(error)
    }
}

enum Command {
    Prepare(Vec<Arc<PreparedAccount>>, Reply<()>),
    Publish(bool, Reply<()>),
    #[cfg(test)]
    Observe(Reply<Vec<(String, Status)>>),
    Stop,
    Call(calls::Command),
}

/// Composition-owned reactor; no account owns an OS thread or a runtime.
pub(crate) struct Worker {
    sender: mailbox::Sender<Command>,
    thread: Option<JoinHandle<Result<(), Error>>>,
    // Retain code beyond join, including Rust thread-local cleanup in the host.
    _provider: Arc<Provider>,
    _catalog: Arc<rptadv_codec_client::CodecCatalog>,
    resolver: Arc<crate::providers::ResolverLibrary>,
    runtime: tokio::runtime::Handle,
    preparation: Option<tokio::task::JoinHandle<()>>,
    incoming: mpsc::Receiver<calls::Receipt>,
    client: calls::Client,
}
impl Worker {
    #[cfg(test)]
    fn start(path: &Path) -> Result<Self, Error> {
        Self::from_provider(unsafe { Provider::load(path) }?)
    }
    pub(crate) fn from_provider(provider: Arc<Provider>) -> Result<Self, Error> {
        let catalog = Arc::new(super::codec::load_catalog().map_err(|_| Error::Owner)?);
        let resolver =
            Arc::new(crate::providers::ResolverLibrary::load().map_err(|_| Error::Owner)?);
        let (sender, receiver) = mailbox::channel(1);
        let calls = calls::Client::new(sender.clone());
        let client = calls.clone();
        let (incoming, receipts) = mpsc::sync_channel(8);
        let owned = Arc::clone(&provider);
        let owned_catalog = Arc::clone(&catalog);
        let (ready, started) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("rpt-sip-accounts".into())
            .spawn(move || {
                let _ = calls.owner.set(std::thread::current().id());
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .max_blocking_threads(1)
                    .build()
                    .map_err(|_| Error::Owner);
                let _ = ready.send(
                    runtime
                        .as_ref()
                        .map(|runtime| runtime.handle().clone())
                        .map_err(|error| *error),
                );
                let runtime = runtime?;
                let result = runtime
                    .block_on(Reactor::new(owned, calls, incoming, owned_catalog).run(receiver));
                // A timed-out system lookup cannot be canceled. Its actual job owns
                // the resolver lease and allocator until it finishes; never block SIP teardown.
                runtime.shutdown_background();
                result
            })
            .map_err(|_| Error::Owner)?;
        match started.recv().unwrap_or(Err(Error::Owner)) {
            Ok(runtime) => Ok(Self {
                sender,
                thread: Some(thread),
                _provider: provider,
                _catalog: catalog,
                resolver,
                runtime,
                preparation: None,
                incoming: receipts,
                client,
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
    /// One serialized preparation job owns the allocator and resolver lease even
    /// if foreground stops waiting. The caller retains its receiver before another job.
    pub(crate) fn prepare_accounts(
        &mut self,
        mut preparer: crate::phone_accounts::AccountPreparer,
        path: std::path::PathBuf,
        records: Vec<crate::configuration::ResolvedPhoneAccount>,
        active: PreparedAccounts,
    ) -> mpsc::Receiver<(
        crate::phone_accounts::AccountPreparer,
        Result<PreparedAccounts, crate::phone_accounts::PreparationError>,
    )> {
        let resolver = Arc::clone(&self.resolver);
        let (reply, result) = mpsc::sync_channel(1);
        self.preparation = Some(self.runtime.spawn_blocking(move || {
            let prepared = crate::directory::Resolver::new(resolver.api())
                .map_err(|_| {
                    crate::phone_accounts::PreparationError::Invalid(
                        "cannot prepare phone account resolver",
                    )
                })
                .and_then(|resolver| {
                    preparer.prepare(&path, records, &active, |endpoint| {
                        resolver
                            .addresses(endpoint)
                            .ok()
                            .and_then(|addresses| addresses.into_iter().next())
                            .ok_or_else(|| {
                                std::io::Error::other("cannot resolve phone account endpoint")
                            })
                    })
                });
            let _ = reply.send((preparer, prepared));
        }));
        result
    }

    /// Clone only the existing serial reactor's checked command sender.
    pub(crate) fn client(&self) -> calls::Client {
        self.client.clone()
    }

    /// Drain bounded signaling receipts on foreground, outside the callback owner.
    pub(crate) fn incoming(&self) -> Option<calls::Receipt> {
        self.incoming.try_recv().ok()
    }
    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, Error> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .blocking_send(command(reply))
            .map_err(|_| Error::Owner)?;
        result.recv().map_err(|_| Error::Owner)?
    }
    /// Reserve every candidate context before product application; drop aborts it.
    pub(crate) fn prepare(&mut self, accounts: &PreparedAccounts) -> Result<Update<'_>, Error> {
        let accounts = accounts
            .as_slice()
            .iter()
            .filter(|account| account.settings().protocol == PhoneAccountProtocol::Sip)
            .cloned()
            .collect();
        self.request(|reply| Command::Prepare(accounts, reply))?;
        Ok(Update {
            worker: self,
            pending: true,
        })
    }
    #[cfg(test)]
    pub(crate) fn observe(&self) -> Result<Vec<(String, Status)>, Error> {
        self.request(Command::Observe)
    }
    /// Request removal, service shutdown, then join before releasing the code lease.
    pub(crate) fn stop(&mut self) -> Result<(), Error> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.sender.blocking_send(Command::Stop);
        let result = thread.join().map_err(|_| Error::Owner)?;
        if self
            .preparation
            .as_ref()
            .is_some_and(|job| !job.is_finished())
        {
            eprintln!("phone account preparation is still running; resolver lease retained");
            result.and(Err(Error::Preparation))
        } else {
            result
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("{error}");
        }
    }
}

/// Prepared cohort, consumed exactly once after the product's publication outcome.
pub(crate) struct Update<'a> {
    worker: &'a Worker,
    pending: bool,
}
impl Update<'_> {
    pub(crate) fn commit(mut self) -> Result<(), Error> {
        self.pending = false;
        self.worker.request(|reply| Command::Publish(true, reply))
    }
}
impl Drop for Update<'_> {
    fn drop(&mut self) {
        if self.pending
            && let Err(error) = self.worker.request(|reply| Command::Publish(false, reply))
        {
            eprintln!("{error}");
        }
    }
}

struct Candidate {
    accounts: BTreeMap<String, Owner>,
    added: Vec<Owner>,
    removed: Vec<Owner>,
}
struct Reactor {
    catalog: Arc<rptadv_codec_client::CodecCatalog>,
    provider: Arc<Provider>,
    active: BTreeMap<String, Owner>,
    candidate: Option<Candidate>,
    retiring: Vec<Owner>,
    failed: bool,
    calls: calls::Client,
    incoming: mpsc::SyncSender<calls::Receipt>,
}
impl Reactor {
    fn new(
        provider: Arc<Provider>,
        calls: calls::Client,
        incoming: mpsc::SyncSender<calls::Receipt>,
        catalog: Arc<rptadv_codec_client::CodecCatalog>,
    ) -> Self {
        Self {
            catalog,
            provider,
            active: BTreeMap::new(),
            candidate: None,
            retiring: Vec::new(),
            failed: false,
            calls,
            incoming,
        }
    }
    fn prepare(&mut self, accounts: Vec<Arc<PreparedAccount>>) -> Result<(), Error> {
        if self.candidate.is_some() {
            return Err(Error::Owner);
        }
        // Reserve both commit and abort retirement before product publication.
        self.retiring.reserve(self.active.len() + accounts.len());
        let mut candidate = Candidate {
            accounts: BTreeMap::new(),
            added: Vec::with_capacity(accounts.len()),
            removed: Vec::with_capacity(self.active.len()),
        };
        for account in accounts {
            let id = account.settings().id.clone();
            let retained = self
                .active
                .get(&id)
                .filter(|entry| entry.borrow().settings.generation() == account.generation());
            let entry = if let Some(entry) = retained {
                Rc::clone(entry)
            } else {
                match Entry::new(&self.provider, account) {
                    Ok(entry) => {
                        let entry = Rc::new(RefCell::new(entry));
                        candidate.added.push(Rc::clone(&entry));
                        entry
                    }
                    Err(error) => {
                        self.retire(candidate.added);
                        return Err(error);
                    }
                }
            };
            candidate.accounts.insert(id, entry);
        }
        for (id, old) in &self.active {
            if !candidate
                .accounts
                .get(id)
                .is_some_and(|new| Rc::ptr_eq(old, new))
            {
                candidate.removed.push(Rc::clone(old));
            }
        }
        self.candidate = Some(candidate);
        Ok(())
    }
    fn retire(&mut self, entries: Vec<Owner>) {
        let now = Instant::now();
        for entry in entries {
            entry.borrow_mut().retire(now);
            self.retiring.push(entry);
        }
    }
    fn publish(&mut self, committed: bool) -> Result<(), Error> {
        let candidate = self.candidate.take().ok_or(Error::Owner)?;
        if !committed {
            self.retire(candidate.added);
            return Ok(());
        }
        self.active = candidate.accounts;
        self.retire(candidate.removed);
        let mut result = Ok(());
        for entry in candidate.added {
            if let Err(error) = entry.borrow_mut().activate(Instant::now()) {
                result = result.and(Err(error));
            }
        }
        result
    }
    async fn service(&mut self) {
        let now = Instant::now();
        for entry in self.active.values().chain(self.retiring.iter()) {
            entry.borrow_mut().service(now);
        }
        self.service_outgoing().await;
        self.service_calls();
        self.retiring.retain(|entry| {
            let entry = entry.borrow();
            self.failed |= entry.abandoned;
            !entry.done
        });
    }
    async fn run(mut self, mut receiver: mailbox::Receiver<Command>) -> Result<(), Error> {
        let mut ticker = tokio::time::interval(SERVICE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut stopping = false;
        let wake = Arc::clone(&self.calls.wake);
        loop {
            tokio::select! {
                _ = ticker.tick() => self.service().await,
                _ = wake.notified() => self.service().await,
                command = receiver.recv(), if !stopping => match command {
                    Some(Command::Prepare(accounts, reply)) => { let _ = reply.send(self.prepare(accounts)); }
                    Some(Command::Publish(committed, reply)) => { let _ = reply.send(self.publish(committed)); }
                    #[cfg(test)]
                    Some(Command::Observe(reply)) => { let _ = reply.send(Ok(self.active.iter().map(|(id, owner)| (id.clone(), owner.borrow().last)).collect())); }
                    Some(Command::Call(command)) => self.call_command(command).await,
                    Some(Command::Stop) | None => {
                        for entry in self.active.values().chain(&self.retiring) { entry.borrow_mut().stop_callbacks(); }
                        stopping = true;
                        receiver.close();
                        if self.candidate.is_some() { let _ = self.publish(false); }
                        let entries = std::mem::take(&mut self.active).into_values().collect();
                        self.retire(entries);
                    }
                }
            }
            if stopping && self.retiring.is_empty() {
                return if self.failed {
                    Err(Error::Shutdown)
                } else {
                    Ok(())
                };
            }
        }
    }
}

struct Entry {
    account: Account,
    settings: Arc<PreparedAccount>,
    last: Status,
    authentication: Option<Instant>,
    binding: Option<Instant>,
    shutdown: Option<Instant>,
    retiring: bool,
    done: bool,
    abandoned: bool,
    calls: BTreeMap<u64, calls::Slot>,
}
impl Entry {
    fn new(provider: &Arc<Provider>, settings: Arc<PreparedAccount>) -> Result<Self, Error> {
        let public = settings.settings();
        let next_hop = settings.remote().to_string();
        let bind = if settings.remote().is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let registrar = format!("sip:{}", public.server);
        let mut user = String::with_capacity(public.username.len());
        use std::fmt::Write;
        for byte in public.username.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                user.push(char::from(byte));
            } else {
                write!(user, "%{byte:02X}").expect("writing a String cannot fail");
            }
        }
        let aor = format!("sip:{user}@{}", public.server);
        let account = provider.prepare(&Settings {
            generation: settings.generation(),
            expires: public.registration_interval_s,
            bind,
            next_hop: &next_hop,
            registrar: &registrar,
            aor: &aor,
            username: &public.username,
            password: settings.secret(),
        })?;
        let last = account.status()?;
        Ok(Self {
            account,
            settings,
            last,
            authentication: None,
            binding: None,
            shutdown: None,
            retiring: false,
            done: false,
            abandoned: false,
            calls: calls::empty(),
        })
    }
    fn auth_deadline(&self, now: Instant) -> Instant {
        now + Duration::from_millis(u64::from(
            self.settings.settings().authentication_timeout_ms,
        ))
    }
    fn activate(&mut self, now: Instant) -> Result<(), Error> {
        if self.settings.settings().register {
            if let Err(error) = self.account.activate() {
                self.begin_shutdown(now);
                return Err(error.into());
            }
            self.authentication = Some(self.auth_deadline(now));
            self.last = self.account.status()?;
        }
        Ok(())
    }
    fn begin_shutdown(&mut self, now: Instant) {
        if self.shutdown.is_some() || self.done {
            return;
        }
        self.retire_calls();
        self.authentication = None;
        self.binding = None;
        if self.account.shutdown().is_err() {
            self.abandon();
        } else {
            self.shutdown = Some(now + SHUTDOWN_LIMIT);
        }
    }
    fn abandon(&mut self) {
        self.done = true;
        self.abandoned = true;
        eprintln!(
            "phone account {}: SIP shutdown did not quiesce; retaining provider",
            self.settings.settings().id
        );
    }
    fn retire(&mut self, now: Instant) {
        if self.retiring || self.done {
            return;
        }
        self.retiring = true;
        self.retire_calls();
        if matches!(self.last.state, 1 | 2) && self.account.unregister().is_ok() {
            self.authentication = Some(self.auth_deadline(now));
        } else {
            self.begin_shutdown(now);
        }
    }
    fn expired(&mut self, now: Instant) -> bool {
        if self.done {
            return true;
        }
        if self.shutdown.is_some_and(|deadline| now >= deadline) {
            self.abandon();
            return true;
        }
        if self.authentication.is_some_and(|deadline| now >= deadline)
            || self.binding.is_some_and(|deadline| now >= deadline)
        {
            eprintln!(
                "phone account {}: SIP authentication or registration lifetime expired",
                self.settings.settings().id
            );
            self.begin_shutdown(now);
            return true;
        }
        false
    }
    fn service(&mut self, now: Instant) {
        let now = now.max(Instant::now());
        // Check existing deadlines before native work or a queued success can renew them.
        if self.expired(now) {
            return;
        }
        let observed = self.account.service().and_then(|()| self.account.status());
        let status = match observed {
            Ok(status) => status,
            Err(error) => {
                eprintln!("phone account {}: {error}", self.settings.settings().id);
                self.begin_shutdown(now);
                return;
            }
        };
        let now = now.max(Instant::now());
        if !matches!(status.state, 7 | 8) && self.expired(now) {
            return;
        }
        if status.state != self.last.state || status.sip_status != self.last.sip_status {
            eprintln!(
                "phone account {}: SIP state {} response {}",
                self.settings.settings().id,
                status.state,
                status.sip_status
            );
        }
        // A reauthentication challenge preserves the old accepted binding deadline.
        // Silent automatic refresh uses the native transaction timer, never a fabricated start event.
        if status.state == 2
            && (status.registrations != self.last.registrations
                || status.sequence != self.last.sequence)
        {
            self.binding = Some(now + Duration::from_secs(u64::from(status.accepted_expires)));
        }
        self.last = status;
        if let Err(error) = self.collect_calls() {
            eprintln!("phone account {}: {error}", self.settings.settings().id);
            self.begin_shutdown(now);
        }
        match status.state {
            7 => {
                if self.account.destroy().is_ok() {
                    self.done = true;
                } else {
                    self.abandon();
                }
            }
            8 => self.abandon(),
            5 => self.begin_shutdown(now),
            4 if self.retiring => self.begin_shutdown(now),
            1 if self.authentication.is_none() => {
                self.authentication = Some(self.auth_deadline(now))
            }
            2 if !self.retiring => self.authentication = None,
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "registration_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "registration_outgoing_tests.rs"]
mod outgoing_tests;
