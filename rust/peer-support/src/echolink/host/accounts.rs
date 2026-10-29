//! Shared account preparation and nonblocking host admission; no protocol state.
use super::{Account, Error, Peer, Provider, abi};
use crate::{
    control::{Client, RejectionReason},
    secrets::{account_secrets_path, load_account_secrets},
};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    path::Path,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

type Incoming = Arc<dyn Fn(Arc<Peer>) + Send + Sync>;

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
}

struct Notifications {
    account: Weak<Account>,
    control: Arc<Client>,
    incoming: Incoming,
    enabled: AtomicBool,
}
struct Entry {
    account: Arc<Account>,
    notifications: Arc<Notifications>,
}

/// One host's prepared accounts. Prepare before product startup, activate only
/// after it succeeds, then stop notifications and drain before product teardown.
/// The selected control backend is shared code, not a new queue implementation.
pub struct Accounts {
    provider: Arc<Provider>,
    control: Arc<Client>,
    incoming: Incoming,
    entries: BTreeMap<String, Arc<Entry>>,
    activated: AtomicBool,
    stopped: AtomicBool,
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
            entries: BTreeMap::new(),
            activated: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
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
        let local = unsafe { super::text(config.local_node, 32) }?;
        if config.enabled != 1
            || local.is_empty()
            || !local.bytes().all(|byte| byte.is_ascii_digit())
            || self.entries.contains_key(&local)
        {
            return Err(Error::Contract);
        }
        let secrets = load_account_secrets(secrets, &local).map_err(|_| Error::Secrets)?;
        let bytes = |value: &str| abi::ByteSpan {
            data: value.as_ptr(),
            length: value.len(),
        };
        let keys = abi::AccountSecrets {
            struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
            abi_version: 1,
            directory_password: bytes(secrets.directory_password()),
            proxy_password: bytes(secrets.proxy_password().unwrap_or("")),
        };
        let account = unsafe { self.provider.prepare(config, &keys) }?;
        let notifications = Arc::new(Notifications {
            account: Arc::downgrade(&account),
            control: Arc::clone(&self.control),
            incoming: Arc::clone(&self.incoming),
            enabled: AtomicBool::new(false),
        });
        self.entries.insert(
            local,
            Arc::new(Entry {
                account,
                notifications,
            }),
        );
        Ok(())
    }

    /// Lookup owns its account/DSO through an independent outbound control call.
    pub fn account(&self, local: &str) -> Option<Arc<Account>> {
        self.entries
            .get(local)
            .map(|entry| Arc::clone(&entry.account))
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
        for entry in self.entries.values() {
            entry.notifications.enabled.store(true, Ordering::Release);
            let context = Arc::as_ptr(&entry.notifications).cast_mut().cast();
            if let Err(error) = unsafe { entry.account.activate(Some(notify), context) } {
                let _ = self.stop();
                return Err(error);
            }
        }
        Ok(())
    }

    /// Gate notification producers, join them, then drain accepted admissions
    /// while the captured product is still callable. Preserve the first failure.
    pub fn stop(&self) -> Result<(), Error> {
        self.stopped.store(true, Ordering::Release);
        for entry in self.entries.values() {
            entry.notifications.enabled.store(false, Ordering::Release);
        }
        let mut result = Ok(());
        for entry in self.entries.values() {
            result = result.and(entry.account.stop());
        }
        result.and(self.control.stop_and_drain().map_err(|_| Error::Control))
    }
}
impl Drop for Accounts {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // Failed quiescence cannot free callback storage/provider code. Hosts
            // also retain their product/backend code until explicit stop succeeds.
            for entry in self.entries.values() {
                std::mem::forget(Arc::clone(entry));
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
    if event.kind != 1 || !notifications.enabled.load(Ordering::Acquire) {
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
                    (pending, Arc::clone(&notifications.incoming)),
                    |(pending, incoming)| {
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
