//! Shared account preparation and nonblocking host admission; no protocol state.
use super::{Account, Error, Peer, Provider, abi};
use crate::{
    control::{Client, RejectionReason},
    secrets::{account_secrets_path, load_account_secrets},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

type Incoming = Arc<dyn Fn(Arc<Peer>) + Send + Sync>;

/// Synchronous resolved-record visitor. Spans are copied before each call ends.
pub type AccountVisitor<'a> =
    dyn FnMut(&abi::AccountConfig, Option<&Path>) -> Result<(), Error> + 'a;

/// Synchronous configuration-visitor target shared by both hosts. Disabled
/// records do not load a provider, read credentials or open a control owner.
pub struct Preparation<'a> {
    configuration: &'a Path,
    library: &'a Path,
    resources: &'a abi::RuntimeConfig,
    control: *const crate::control::Descriptor,
    incoming: Incoming,
    accounts: Option<Accounts>,
}
impl<'a> Preparation<'a> {
    /// Borrow trusted composition inputs until the configuration visit finishes.
    /// # Safety
    /// The selected backend descriptor/code remains valid through account stop
    /// and drain. The incoming target is callable after product startup and
    /// remains live through that same barrier.
    pub unsafe fn new(
        configuration: &'a Path,
        library: &'a Path,
        resources: &'a abi::RuntimeConfig,
        control: *const crate::control::Descriptor,
        incoming: impl Fn(Arc<Peer>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            configuration,
            library,
            resources,
            control,
            incoming: Arc::new(incoming),
            accounts: None,
        }
    }

    /// Prepare a borrowed resolved record without retaining its spans or opening
    /// network I/O. Paths are relative to the actual host configuration file.
    /// # Safety
    /// The record's complete spans/lists remain readable for this call.
    pub unsafe fn prepare(
        &mut self,
        config: &abi::AccountConfig,
        configured_secrets: Option<&Path>,
    ) -> Result<(), Error> {
        if config.enabled == 0 {
            return Ok(());
        }
        if self.accounts.is_none() {
            let provider = unsafe { Provider::load(self.library, self.resources) }?;
            let control = Arc::new(
                unsafe { Client::open(self.control, "rpt_advanced/echolink", 512) }
                    .map_err(|_| Error::Control)?,
            );
            let incoming = Arc::clone(&self.incoming);
            self.accounts = Some(Accounts::new(provider, control, move |peer| incoming(peer)));
        }
        let path = account_secrets_path(self.configuration, configured_secrets);
        unsafe { self.accounts.as_mut().unwrap().prepare(config, &path) }
    }

    /// Transfer inactive owners into stable host storage before product startup.
    pub fn finish(self) -> Option<Accounts> {
        self.accounts
    }

    /// Reuse the live account owner, or lazily create it for the first enabled
    /// record. The host publishes the returned owner only on success. Disabled
    /// configurations require no provider or private file.
    /// # Safety
    /// The visitor supplies complete valid borrowed records. Product/backend
    /// code remains live through stop/drain. Serialize host lifecycle changes
    /// and call off provider, control and audio executors.
    pub unsafe fn reload(
        mut self,
        current: Option<Arc<Accounts>>,
        visit: impl FnOnce(&mut AccountVisitor<'_>) -> Result<(), Error>,
        product: impl FnOnce() -> bool,
        restore_product: impl FnOnce() -> bool,
    ) -> Result<(Option<Arc<Accounts>>, UpdateOutcome), UpdateError> {
        if let Some(accounts) = current {
            let mut update = accounts.prepare_update()?;
            visit(&mut |config, path| unsafe {
                update.prepare(config, &account_secrets_path(self.configuration, path))
            })?;
            update.remove_unlisted()?;
            let outcome = update.commit(product, restore_product)?;
            return Ok((Some(accounts), outcome));
        }
        visit(&mut |config, path| unsafe { self.prepare(config, path) })?;
        let accounts = self.finish().map(Arc::new);
        if !product() {
            return Err(UpdateError {
                cause: Error::Product,
                rollback: (!restore_product()).then_some(Error::Product),
            });
        }
        if let Some(accounts) = &accounts {
            accounts.updating.store(true, Ordering::Release);
            if let Err(cause) = unsafe { accounts.activate() } {
                let mut rollback = accounts.stop().err();
                if !restore_product() {
                    rollback.get_or_insert(Error::Product);
                }
                return Err(UpdateError { cause, rollback });
            }
            accounts.updating.store(false, Ordering::Release);
        }
        Ok((accounts, UpdateOutcome::default()))
    }
}

struct Notifications {
    account: Weak<Account>,
    control: Arc<Client>,
    incoming: Incoming,
    enabled: Arc<AtomicBool>,
    updating: Arc<AtomicBool>,
}
struct Entry {
    account: Arc<Account>,
    notifications: Arc<Notifications>,
}
impl Entry {
    fn new(account: Arc<Account>, owner: &Accounts) -> Arc<Self> {
        let notifications = Arc::new(Notifications {
            account: Arc::downgrade(&account),
            control: Arc::clone(&owner.control),
            incoming: Arc::clone(&owner.incoming),
            enabled: Arc::new(AtomicBool::new(false)),
            updating: Arc::clone(&owner.updating),
        });
        Arc::new(Self {
            account,
            notifications,
        })
    }

    unsafe fn activate(&self) -> Result<(), Error> {
        let context = Arc::as_ptr(&self.notifications).cast_mut().cast();
        unsafe { self.account.activate(Some(notify), context) }
    }
}

/// Configuration committed; a failed retired-owner join remains retained and is
/// reported independently rather than falsely claiming the update rolled back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateOutcome {
    /// Removed accounts whose callback storage must remain owned until stop.
    pub retirement: Option<Error>,
}

/// First commit failure and an independent rollback failure, without secrets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateError {
    /// Original product/account failure.
    pub cause: Error,
    /// A failed restoration keeps new admissions gated rather than claim success.
    pub rollback: Option<Error>,
}
impl From<Error> for UpdateError {
    fn from(cause: Error) -> Self {
        Self {
            cause,
            rollback: None,
        }
    }
}

struct Change {
    entry: Arc<Entry>,
    prepared: Option<Arc<Account>>,
    enabled: bool,
    was_enabled: bool,
    removed: bool,
}

/// Provider-owned snapshots only. Dropping a staged update never stops the
/// running admission executor or rereads credentials during commit/rollback.
pub struct PreparedUpdate<'a> {
    owner: &'a Accounts,
    revision: u64,
    changes: BTreeMap<String, Change>,
    visited: BTreeSet<String>,
}
impl PreparedUpdate<'_> {
    /// Validate/copy a resolved record while its spans and private file exist.
    /// # Safety
    /// The complete record and nested spans/lists are readable for this call.
    pub unsafe fn prepare(
        &mut self,
        config: &abi::AccountConfig,
        secrets: &Path,
    ) -> Result<(), Error> {
        let local = unsafe { local(config) }?;
        if !self.visited.insert(local.clone()) {
            return Err(Error::Contract);
        }
        let entry = self
            .owner
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&local)
            .cloned();
        if entry.is_none() && config.enabled == 0 {
            return Ok(());
        }
        let prepared = unsafe { self.owner.prepare_account(config, &local, secrets) }?;
        let (entry, prepared) = match entry {
            Some(entry) => (entry, Some(prepared)),
            None => (Entry::new(prepared, self.owner), None),
        };
        let was_enabled = entry.notifications.enabled.load(Ordering::Acquire);
        self.changes.insert(
            local,
            Change {
                entry,
                prepared,
                enabled: config.enabled == 1,
                was_enabled,
                removed: false,
            },
        );
        Ok(())
    }

    /// After visiting a complete host configuration, stage absent accounts as
    /// disabled snapshots. Permanent retirement follows successful commit only.
    pub fn remove_unlisted(&mut self) -> Result<(), Error> {
        let absent: Vec<_> = self
            .owner
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .filter(|local| !self.visited.contains(*local))
            .cloned()
            .collect();
        for local in absent {
            let config = abi::AccountConfig {
                local_node: abi::ByteSpan {
                    data: local.as_ptr(),
                    length: local.len(),
                },
                ..Default::default()
            };
            unsafe { self.prepare(&config, Path::new("")) }?;
            self.changes.get_mut(&local).unwrap().removed = true;
        }
        Ok(())
    }

    /// Commit product configuration and account snapshots with admission gated.
    /// A product error may follow publication, so it also requires restoration.
    /// Later account failure restores snapshots in reverse before restoring the
    /// product. Call both closures off executors. Failed restoration keeps the
    /// admission gate closed.
    pub fn commit(
        self,
        product: impl FnOnce() -> bool,
        restore_product: impl FnOnce() -> bool,
    ) -> Result<UpdateOutcome, UpdateError> {
        let _operation = self.owner.operation.lock().map_err(|_| Error::Control)?;
        if self.owner.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed).into());
        }
        if self.owner.revision.load(Ordering::Acquire) != self.revision {
            return Err(Error::Contract.into());
        }
        let next = self.revision.checked_add(1).ok_or(Error::Contract)?;
        self.owner.revision.store(next, Ordering::Release);
        self.owner.updating.store(true, Ordering::Release);
        for change in self.changes.values() {
            change
                .entry
                .notifications
                .enabled
                .store(false, Ordering::Release);
        }
        if !product() {
            let rollback = (!restore_product()).then_some(Error::Product);
            if rollback.is_none() {
                for change in self.changes.values() {
                    change
                        .entry
                        .notifications
                        .enabled
                        .store(change.was_enabled, Ordering::Release);
                }
                self.owner.updating.store(false, Ordering::Release);
            }
            return Err(UpdateError {
                cause: Error::Product,
                rollback,
            });
        }
        let mut applied: Vec<&Change> = Vec::with_capacity(self.changes.len());
        for change in self.changes.values() {
            let result = if let Some(prepared) = &change.prepared {
                change.entry.account.apply_prepared(prepared)
            } else {
                // Even a failed activation may have started a callback producer.
                applied.push(change);
                unsafe { change.entry.activate() }
            };
            if let Err(cause) = result {
                let mut failure = UpdateError::from(cause);
                for change in applied.into_iter().rev() {
                    let restored = match &change.prepared {
                        Some(prepared) => change.entry.account.apply_prepared(prepared),
                        None => self.owner.retire(Arc::clone(&change.entry)),
                    };
                    if let Err(error) = restored {
                        failure.rollback.get_or_insert(error);
                    }
                }
                if !restore_product() {
                    failure.rollback.get_or_insert(Error::Product);
                }
                if failure.rollback.is_none() {
                    for change in self.changes.values() {
                        change
                            .entry
                            .notifications
                            .enabled
                            .store(change.was_enabled, Ordering::Release);
                    }
                    self.owner.updating.store(false, Ordering::Release);
                }
                return Err(failure);
            }
            if change.prepared.is_some() {
                applied.push(change);
            }
        }
        {
            let mut entries = self
                .owner
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for (local, change) in &self.changes {
                if change.removed {
                    entries.remove(local);
                } else {
                    entries.insert(local.clone(), Arc::clone(&change.entry));
                }
                change
                    .entry
                    .notifications
                    .enabled
                    .store(change.enabled, Ordering::Release);
            }
        }
        let mut outcome = UpdateOutcome::default();
        for change in self.changes.values().filter(|change| change.removed) {
            if let Err(error) = self.owner.retire(Arc::clone(&change.entry)) {
                outcome.retirement.get_or_insert(error);
            }
        }
        self.owner.updating.store(false, Ordering::Release);
        Ok(outcome)
    }
}

unsafe fn local(config: &abi::AccountConfig) -> Result<String, Error> {
    let local = unsafe { super::text(config.local_node, 32) }?;
    if config.enabled > 1 || local.is_empty() || !local.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Contract);
    }
    Ok(local)
}

/// One host's prepared accounts. Prepare before product startup, activate only
/// after it succeeds, then stop notifications and drain before product teardown.
/// The selected control backend is shared code, not a new queue implementation.
pub struct Accounts {
    provider: Arc<Provider>,
    control: Arc<Client>,
    incoming: Incoming,
    entries: Mutex<BTreeMap<String, Arc<Entry>>>,
    retired: Mutex<Vec<Arc<Entry>>>,
    activated: AtomicBool,
    stopped: AtomicBool,
    updating: Arc<AtomicBool>,
    operation: Mutex<()>,
    revision: AtomicU64,
}
impl Accounts {
    /// Keep the actual product admission target, rather than a host-global lookup
    /// that might reacquire a lifecycle lock during startup/shutdown.
    pub fn new(
        provider: Arc<Provider>,
        control: Arc<Client>,
        incoming: impl Fn(Arc<Peer>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            provider,
            control,
            incoming: Arc::new(incoming),
            entries: Mutex::new(BTreeMap::new()),
            retired: Mutex::new(Vec::new()),
            activated: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            updating: Arc::new(AtomicBool::new(false)),
            operation: Mutex::new(()),
            revision: AtomicU64::new(0),
        }
    }

    /// Copy one resolved account and its private credentials without login. A
    /// disabled record neither reads credentials nor creates a foreign account.
    /// # Safety
    /// Record spans/lists are valid for this synchronous preparation only.
    pub unsafe fn prepare(
        &mut self,
        config: &abi::AccountConfig,
        secrets: &Path,
    ) -> Result<(), Error> {
        if self.activated.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        if config.enabled == 0 {
            return Ok(());
        }
        let local = unsafe { local(config) }?;
        if self
            .entries
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&local)
        {
            return Err(Error::Contract);
        }
        let account = unsafe { self.prepare_account(config, &local, secrets) }?;
        let entry = Entry::new(account, self);
        self.entries
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .insert(local, entry);
        Ok(())
    }

    unsafe fn prepare_account(
        &self,
        config: &abi::AccountConfig,
        local: &str,
        path: &Path,
    ) -> Result<Arc<Account>, Error> {
        let secrets = if config.enabled == 0 {
            None
        } else {
            Some(load_account_secrets(path, local).map_err(|_| Error::Secrets)?)
        };
        let bytes = |value: &str| abi::ByteSpan {
            data: value.as_ptr(),
            length: value.len(),
        };
        let keys = abi::AccountSecrets {
            struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
            abi_version: 1,
            directory_password: bytes(
                secrets
                    .as_ref()
                    .map_or("", |keys| keys.directory_password()),
            ),
            proxy_password: bytes(
                secrets
                    .as_ref()
                    .and_then(|keys| keys.proxy_password())
                    .unwrap_or(""),
            ),
        };
        unsafe { self.provider.prepare(config, &keys) }
    }

    /// Stage updates to running accounts without opening another executor.
    pub fn prepare_update(&self) -> Result<PreparedUpdate<'_>, Error> {
        if self.stopped.load(Ordering::Acquire) || !self.activated.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        Ok(PreparedUpdate {
            owner: self,
            revision: self.revision.load(Ordering::Acquire),
            changes: BTreeMap::new(),
            visited: BTreeSet::new(),
        })
    }

    /// Lookup owns its account/DSO through an independent outbound control call.
    pub fn account(&self, local: &str) -> Option<Arc<Account>> {
        if self.stopped.load(Ordering::Acquire) || self.updating.load(Ordering::Acquire) {
            return None;
        }
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(local)
            .filter(|entry| {
                entry.notifications.enabled.load(Ordering::Acquire)
                    || !self.activated.load(Ordering::Acquire)
            })
            .map(|entry| Arc::clone(&entry.account))
    }

    /// Copy diagnostics even when admission is disabled or a reload is in
    /// progress. Release the map lock before querying the account's owner.
    /// Removed entries are not retained merely for status. Host control only.
    pub fn observe(&self, local: &str) -> Result<Option<abi::AccountObservation>, Error> {
        let account = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(local)
            .map(|entry| Arc::clone(&entry.account));
        account.map(|account| account.observe()).transpose()
    }

    /// Start each prepared account only when the captured product is callable.
    /// Partial startup failure stops all accounts before reporting the first error.
    /// # Safety
    /// The product target and selected backend code stay live through stop/drain.
    /// Call off provider/audio/control executors; lifecycle owns this set through
    /// every callback and must retain it if stop cannot quiesce.
    pub unsafe fn activate(&self) -> Result<(), Error> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        if self.activated.swap(true, Ordering::AcqRel) {
            return Err(Error::Contract);
        }
        for entry in self.current_entries() {
            entry.notifications.enabled.store(true, Ordering::Release);
            if let Err(error) = unsafe { entry.activate() } {
                let _ = self.stop();
                return Err(error);
            }
        }
        Ok(())
    }

    /// Gate notification producers, join them, then drain accepted admissions
    /// while the captured product is still callable. Preserve the first failure.
    pub fn stop(&self) -> Result<(), Error> {
        let _operation = self.operation.lock().map_err(|_| Error::Control)?;
        self.stopped.store(true, Ordering::Release);
        let mut entries = self.current_entries();
        entries.extend(
            self.retired
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .cloned(),
        );
        for entry in &entries {
            entry.notifications.enabled.store(false, Ordering::Release);
        }
        let mut result = Ok(());
        for entry in &entries {
            result = result.and(entry.account.stop());
        }
        result.and(self.control.stop_and_drain().map_err(|_| Error::Control))
    }

    fn current_entries(&self) -> Vec<Arc<Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn retire(&self, entry: Arc<Entry>) -> Result<(), Error> {
        let result = entry.account.stop();
        if result.is_err() {
            self.retired
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(entry);
        }
        result
    }
}
impl Drop for Accounts {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // Failed quiescence cannot free callback storage/provider code. Hosts
            // also retain their product/backend code until explicit stop succeeds.
            for entry in self.current_entries().into_iter().chain(
                self.retired
                    .get_mut()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                    .cloned(),
            ) {
                std::mem::forget(entry);
            }
            std::mem::forget(Arc::clone(&self.control));
        }
    }
}

unsafe extern "C" fn notify(context: *mut c_void, event: *const abi::Event) {
    let notifications = unsafe { &*context.cast::<Notifications>() };
    let Some(event) = (unsafe { event.as_ref() }) else {
        return;
    };
    if event.kind != 1
        || !notifications.enabled.load(Ordering::Acquire)
        || notifications.updating.load(Ordering::Acquire)
    {
        return;
    }
    let Some(account) = notifications.account.upgrade() else {
        return;
    };
    let _ = unsafe {
        account.dispatch_pending(event, |pending| {
            notifications
                .control
                .submit(
                    (
                        pending,
                        Arc::clone(&notifications.incoming),
                        Arc::clone(&notifications.enabled),
                        Arc::clone(&notifications.updating),
                    ),
                    |(pending, incoming, enabled, updating)| {
                        if !enabled.load(Ordering::Acquire) || updating.load(Ordering::Acquire) {
                            return;
                        }
                        if let Ok(peer) = pending.claim() {
                            incoming(peer);
                        }
                    },
                )
                .map_err(|rejected| {
                    let status = match rejected.reason {
                        RejectionReason::Full => abi::Status::Full,
                        RejectionReason::Stopped => abi::Status::Closed,
                        RejectionReason::Backend => abi::Status::Failed,
                    };
                    (Error::Status(status), rejected.payload.0)
                })
        })
    };
}
