use crate::accounts::Route as PreparedAccount;
use crate::sip_provider::{Account, Provider, Status};
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
    Provider(crate::sip_provider::Error),
    Owner,
    Shutdown,
    Call,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(f),
            Self::Call => f.write_str("SIP call is unavailable"),
            Self::Owner => f.write_str("SIP registration owner is unavailable"),
            Self::Shutdown => f.write_str("SIP shutdown did not quiesce; provider lease retained"),
        }
    }
}
impl From<crate::sip_provider::Error> for Error {
    fn from(error: crate::sip_provider::Error) -> Self {
        Self::Provider(error)
    }
}

enum Command {
    Prepare(Vec<Arc<PreparedAccount>>, Reply<()>),
    Publish(bool, Reply<()>),
    Observe(Reply<(bool, Vec<(String, Status)>)>),
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
    stop_failed: bool,
    incoming: mpsc::Receiver<calls::Receipt>,
    client: calls::Client,
}
impl Worker {
    pub(crate) fn from_provider(provider: Arc<Provider>) -> Result<Self, Error> {
        let catalog = Arc::new(super::codec::load_catalog().map_err(|_| Error::Owner)?);
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
                        .map(|_| ())
                        .map_err(|error| *error),
                );
                let runtime = runtime?;
                let result = runtime
                    .block_on(Reactor::new(owned, calls, incoming, owned_catalog).run(receiver));
                // Network resolution belongs to the frontend's bounded route owner.
                drop(runtime);
                result
            })
            .map_err(|_| Error::Owner)?;
        match started.recv().unwrap_or(Err(Error::Owner)) {
            Ok(()) => Ok(Self {
                sender,
                thread: Some(thread),
                _provider: provider,
                _catalog: catalog,
                stop_failed: false,
                incoming: receipts,
                client,
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
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
    /// Run only on adoption. Dormant product configuration owns immutable
    /// rows; allocating native Sofia contexts belongs to this reactor alone.
    pub(crate) fn apply(&self, accounts: Vec<Arc<PreparedAccount>>) -> Result<(), Error> {
        self.request(|reply| Command::Prepare(accounts, reply))?;
        self.request(|reply| Command::Publish(true, reply))
    }
    pub(crate) fn observe(&self) -> Result<(bool, Vec<(String, Status)>), Error> {
        self.request(Command::Observe)
    }
    /// Request removal, service shutdown, then join before releasing the code lease.
    pub(crate) fn stop(&mut self) -> Result<(), Error> {
        if self.stop_failed { return Err(Error::Shutdown); }
        let Some(thread) = self.thread.take() else { return Ok(()); };
        let _ = self.sender.blocking_send(Command::Stop);
        let result = thread.join().map_err(|_| Error::Owner).and_then(|r|r);
        self.stop_failed = result.is_err();
        result
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("{error}");
        }
    }
}

struct Candidate {
    retained: Vec<(Owner, [u32; 3], [u32; 3])>,
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
    activating: Vec<Owner>,
    selection_failed: bool,
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
            activating: Vec::new(),
            selection_failed: false,
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
            retained: Vec::new(),
            accounts: BTreeMap::new(),
            added: Vec::with_capacity(accounts.len()),
            removed: Vec::with_capacity(self.active.len()),
        };
        for account in accounts {
            let id = account.account.account_id().to_string();
            let retained = self
                .active
                .get(&id)
                .filter(|entry| entry.borrow().settings.generation() == account.generation());
            let entry = if let Some(entry) = retained {
                candidate.retained.push((Rc::clone(entry), entry.borrow().delays_ms, account.account.jitter_ms()));
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
        for (index, (entry, _, delays_ms)) in candidate.retained.iter().enumerate() {
            let result = entry.borrow_mut().set_delays(*delays_ms);
            if let Err(error) = result {
                // Restore every touched slot before the reactor services any callback.
                // The product can retry the old selection if this restoration also fails.
                let mut restore = Ok(());
                for (touched, old, _) in candidate.retained[..=index].iter().rev() {
                    if let Err(error) = touched.borrow_mut().set_delays(*old) {
                        restore = Err(error);
                    }
                }
                self.retire(candidate.added);
                return restore.and(Err(error));
            }
        }
        self.active = candidate.accounts;
        self.retire(candidate.removed);
        self.selection_failed = false;
        self.activating = candidate.added;
        Ok(())
    }
    async fn service(&mut self) {
        let now = Instant::now();
        for entry in self.active.values().chain(self.retiring.iter()) {
            entry.borrow_mut().service(now);
        }
        // A late withdrawal for the same registrar identity can remove a new
        // binding. Wait for the actual old account retirement before activation.
        if self.retiring.is_empty() && !self.failed {
            for entry in std::mem::take(&mut self.activating) {
                if entry.borrow_mut().activate(now).is_err() { self.selection_failed = true; }
            }
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
            let deadline = self.earliest_call_deadline();
            let due = async {
                if let Some(at) = deadline { tokio::time::sleep_until(at.into()).await; }
                else { std::future::pending::<()>().await; }
            };
            tokio::select! {
                _ = due => self.service().await,
                _ = ticker.tick() => self.service().await,
                _ = wake.notified() => self.service().await,
                command = receiver.recv(), if !stopping => match command {
                    Some(Command::Prepare(accounts, reply)) => { let _ = reply.send(self.prepare(accounts)); }
                    Some(Command::Publish(committed, reply)) => { let _ = reply.send(self.publish(committed)); }
                    Some(Command::Observe(reply)) => {
                        let state = if self.failed { Err(Error::Shutdown) } else if self.selection_failed { Err(Error::Call) } else {
                            Ok((self.activating.is_empty() && self.retiring.is_empty(),
                                self.active.iter().map(|(id, owner)| (id.clone(), owner.borrow().last)).collect()))
                        };
                        let _ = reply.send(state);
                    }
                    Some(Command::Call(command)) => self.call_command(command).await,
                    Some(Command::Stop) | None => {
                        for entry in self.active.values().chain(&self.retiring) { entry.borrow_mut().stop_callbacks(); }
                        stopping = true;
                        receiver.close();
                        if self.candidate.is_some() { let _ = self.publish(false); }
                        let entries = std::mem::take(&mut self.active).into_values().collect();
                        self.activating.clear();
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
    delays_ms: [u32; 3],
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
        let account = settings.account.open_sip(provider, settings.remote)?;
        let last = account.status()?;
        Ok(Self {
            account,
            delays_ms: settings.account.jitter_ms(),
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
    fn set_delays(&mut self, delays_ms: [u32; 3]) -> Result<(), Error> {
        // The same pass also restores any partially applied selection on rollback.
        for slot in self.calls.values() {
            slot.set_delays(delays_ms)?;
        }
        self.delays_ms = delays_ms;
        Ok(())
    }
    fn auth_deadline(&self, now: Instant) -> Instant {
        now + Duration::from_secs(30)
    }
    fn activate(&mut self, now: Instant) -> Result<(), Error> {
        {
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
            self.settings.account.account_id()
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
                self.settings.account.account_id()
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
                eprintln!("phone account {}: {error}", self.settings.account.account_id());
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
                self.settings.account.account_id(),
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
            eprintln!("phone account {}: {error}", self.settings.account.account_id());
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
