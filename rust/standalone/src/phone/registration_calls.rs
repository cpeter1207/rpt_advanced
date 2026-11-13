//! Call admission and retained product steps on the existing SIP reactor.
use super::{Command as OwnerCommand, Entry, Error, Reactor, Reply};
use crate::{abi, phone::call_media::Media, phone_accounts::PreparedAccount};
use rptadv_peer_support::sip::registration::Call as Observation;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::ThreadId,
};
use tokio::sync::{Notify, mpsc};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Key {
    account: String,
    generation: u64,
    call: u64,
}

#[derive(Clone)]
pub(crate) struct Client {
    sender: mpsc::Sender<OwnerCommand>,
    pub(super) wake: Arc<Notify>,
    pub(super) owner: Arc<OnceLock<ThreadId>>,
}
impl Client {
    pub(super) fn new(sender: mpsc::Sender<OwnerCommand>) -> Self {
        Self {
            sender,
            wake: Arc::new(Notify::new()),
            owner: Arc::new(OnceLock::new()),
        }
    }
    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, Error> {
        if self
            .owner
            .get()
            .is_some_and(|owner| *owner == std::thread::current().id())
        {
            return Err(Error::Owner);
        }
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        self.sender
            .blocking_send(OwnerCommand::Call(command(reply)))
            .map_err(|_| Error::Owner)?;
        result.recv().map_err(|_| Error::Owner)?
    }
}

/// One foreground receipt; dropping an unconsumed receipt closes that exact call.
pub(crate) struct Receipt {
    pub(crate) account: Arc<PreparedAccount>,
    key: Key,
    close: Arc<AtomicBool>,
    client: Client,
    consumed: bool,
}
impl Receipt {
    pub(crate) fn prepare(
        mut self,
        jitter: &'static abi::rptadv_jitter_descriptor_v1,
        remote: String,
    ) -> Result<Handle, Error> {
        let result = self
            .client
            .request(|reply| Command::Prepare(self.key.clone(), jitter, remote, reply));
        self.consumed = result.is_ok();
        result
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        if !self.consumed {
            self.close.store(true, Ordering::Release);
            self.client.wake.notify_one();
        }
    }
}

/// Immutable identity plus separately locked media; no borrow is held across product step.
pub(crate) struct Shared {
    key: Key,
    pub(crate) account: String,
    pub(crate) local: String,
    pub(crate) remote: String,
    pub(crate) source: std::net::SocketAddr,
    pub(crate) media: Mutex<Media>,
    pub(crate) inband: bool,
    state: AtomicU32,
    close: Arc<AtomicBool>,
    stopped: AtomicBool,
    client: Client,
}
impl Shared {
    pub(crate) fn generations(&self) -> (u64, u64) {
        (self.key.generation, self.key.call)
    }
    pub(crate) fn live(&self) -> bool {
        self.state.load(Ordering::Acquire) < 4 && !self.close.load(Ordering::Acquire)
    }
    pub(crate) fn wake(&self) {
        self.client.wake.notify_one();
    }
    pub(crate) fn answer(&self) -> Result<(), Error> {
        self.client
            .request(|reply| Command::Answer(self.key.clone(), reply))
    }
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.wake();
    }
    pub(crate) fn end(&self) {
        self.close.store(true, Ordering::Release);
        self.wake();
    }
}

/// Unique product-owned handle; its drop requests signaling retirement.
pub(crate) struct Handle {
    pub(crate) shared: Arc<Shared>,
}
impl Handle {
    pub(crate) fn attach(
        &self,
        step: unsafe extern "C" fn(*mut c_void) -> i32,
        context: *mut c_void,
    ) -> Result<Registration, Error> {
        self.shared.client.request(|reply| {
            Command::Attach(
                self.shared.key.clone(),
                Step {
                    callback: step,
                    context: context as usize,
                },
                reply,
            )
        })?;
        Ok(Registration(Arc::clone(&self.shared)))
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.end();
    }
}

pub(crate) struct Registration(pub(crate) Arc<Shared>);
impl Drop for Registration {
    fn drop(&mut self) {
        self.0.stop();
        // Closing the receiver occurs only after the reactor cleared all callbacks.
        let _ = self
            .0
            .client
            .request(|reply| Command::Detach(self.0.key.clone(), reply));
    }
}

pub(super) struct Step {
    callback: unsafe extern "C" fn(*mut c_void) -> i32,
    context: usize,
}
pub(super) enum Command {
    Prepare(
        Key,
        &'static abi::rptadv_jitter_descriptor_v1,
        String,
        Reply<Handle>,
    ),
    Answer(Key, Reply<()>),
    Attach(Key, Step, Reply<()>),
    Detach(Key, Reply<()>),
}
pub(super) struct Slot {
    observation: Observation,
    close: Arc<AtomicBool>,
    dispatched: bool,
    shared: Option<Arc<Shared>>,
    step: Option<Step>,
}
impl Slot {
    fn state(&mut self, observation: Observation) {
        if let Some(shared) = &self.shared {
            shared.state.store(observation.state, Ordering::Release);
        }
        self.observation = observation;
    }
    fn stop(&mut self) {
        self.step = None;
        if let Some(shared) = &self.shared {
            shared.stop();
        }
    }
}
impl Entry {
    pub(super) fn collect_calls(&mut self) -> Result<(), Error> {
        while let Some(call) = self.account.next_call()? {
            let token = call.call;
            let terminal = call.state == 5;
            if let Some(slot) = self.calls.get_mut(&token) {
                slot.state(call);
            } else if !terminal {
                self.calls.insert(
                    token,
                    Slot {
                        observation: call,
                        close: Arc::new(AtomicBool::new(false)),
                        dispatched: false,
                        shared: None,
                        step: None,
                    },
                );
            } else {
                self.account.release_call(token)?;
            }
            // Keep tracked terminal leases until their product callback is quiescent.
            // Native capacity therefore also bounds retained host call owners.
        }
        Ok(())
    }
    pub(super) fn retire_calls(&mut self) {
        for (token, slot) in &mut self.calls {
            slot.close.store(true, Ordering::Release);
            if let Some(shared) = &slot.shared {
                shared.state.store(4, Ordering::Release);
            }
            if slot.observation.state < 4 {
                let _ = self.account.hangup_call(*token);
            }
        }
    }
    pub(super) fn stop_callbacks(&mut self) {
        for slot in self.calls.values_mut() {
            slot.stop();
        }
    }
}
impl Reactor {
    pub(super) fn service_calls(&mut self) {
        for owner in self.active.values().chain(&self.retiring) {
            let mut callbacks: [_; 8] = std::array::from_fn(|_| None);
            {
                let mut entry = owner.borrow_mut();
                let account = Arc::clone(&entry.settings);
                let generation = account.generation();
                let id = account.settings().id.clone();
                let done = entry.done;
                let retiring = entry.retiring || done;
                let Entry {
                    account: native,
                    calls,
                    ..
                } = &mut *entry;
                for (index, (token, slot)) in calls.iter_mut().enumerate() {
                    if slot.close.load(Ordering::Acquire) && slot.observation.state < 4 {
                        let _ = native.hangup_call(*token);
                    }
                    if !slot.dispatched && !retiring && slot.observation.state == 1 {
                        slot.dispatched = true;
                        let receipt = Receipt {
                            account: Arc::clone(&account),
                            key: Key {
                                account: id.clone(),
                                generation,
                                call: *token,
                            },
                            close: Arc::clone(&slot.close),
                            client: self.calls.clone(),
                            consumed: false,
                        };
                        if self.incoming.try_send(receipt).is_err() {
                            slot.close.store(true, Ordering::Release);
                        }
                    }
                    if let (Some(shared), Some(step)) = (&slot.shared, &slot.step) {
                        if shared.stopped.load(Ordering::Acquire) {
                            slot.step = None;
                        } else if slot.observation.state >= 3 {
                            callbacks[index] =
                                Some((Arc::clone(shared), step.callback, step.context));
                        }
                    }
                }
                calls.retain(|token, slot| {
                    if slot.observation.state != 5 || slot.step.is_some() {
                        return true;
                    }
                    !done && native.release_call(*token).is_err()
                });
            }
            // No account, peer or media borrow/lock survives into the product callback.
            for (shared, callback, context) in callbacks.into_iter().flatten() {
                if !shared.stopped.load(Ordering::Acquire)
                    && unsafe { callback(context as *mut c_void) } != 0
                {
                    shared.stop();
                    shared.end();
                }
            }
        }
    }
    fn call_owner(&self, key: &Key) -> Result<super::Owner, Error> {
        self.active
            .values()
            .chain(&self.retiring)
            .find(|entry| {
                let entry = entry.borrow();
                entry.settings.settings().id == key.account
                    && entry.settings.generation() == key.generation
            })
            .cloned()
            .ok_or(Error::Call)
    }
    pub(super) async fn call_command(&mut self, command: Command) {
        match command {
            Command::Prepare(key, jitter, remote, reply) => {
                let result = async {
                    let owner = self.call_owner(&key)?;
                    let offer = {
                        let entry = owner.borrow();
                        let slot = entry.calls.get(&key.call).ok_or(Error::Call)?;
                        if entry.retiring
                            || slot.observation.state != 1
                            || slot.shared.is_some()
                            || slot.close.load(Ordering::Acquire)
                        {
                            return Err(Error::Call);
                        }
                        slot.observation.offer().to_vec()
                    };
                    let media = Media::new(&offer, key.call, jitter)
                        .await
                        .map_err(|_| Error::Call)?;
                    let mut entry = owner.borrow_mut();
                    let local = entry.settings.settings().local_node.clone();
                    let slot = entry.calls.get_mut(&key.call).ok_or(Error::Call)?;
                    let shared = Arc::new(Shared {
                        key: key.clone(),
                        account: key.account.clone(),
                        local,
                        remote,
                        source: slot.observation.source()?,
                        inband: media.inband,
                        media: Mutex::new(media),
                        state: AtomicU32::new(slot.observation.state),
                        close: Arc::clone(&slot.close),
                        stopped: AtomicBool::new(false),
                        client: self.calls.clone(),
                    });
                    slot.shared = Some(Arc::clone(&shared));
                    Ok(Handle { shared })
                }
                .await;
                let _ = reply.send(result);
            }
            Command::Answer(key, reply) => {
                let result = (|| {
                    let owner = self.call_owner(&key)?;
                    let entry = owner.borrow();
                    let slot = entry.calls.get(&key.call).ok_or(Error::Call)?;
                    let shared = slot.shared.as_ref().ok_or(Error::Call)?;
                    if entry.retiring || !shared.live() {
                        return Err(Error::Call);
                    }
                    let media = shared.media.lock().map_err(|_| Error::Call)?;
                    entry.account.respond_call(key.call, 200, media.answer())?;
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Command::Attach(key, step, reply) => {
                let result = (|| {
                    let owner = self.call_owner(&key)?;
                    let mut entry = owner.borrow_mut();
                    let slot = entry.calls.get_mut(&key.call).ok_or(Error::Call)?;
                    let shared = slot.shared.as_ref().ok_or(Error::Call)?;
                    if !shared.live()
                        || slot.step.is_some()
                        || shared.stopped.load(Ordering::Acquire)
                    {
                        return Err(Error::Call);
                    }
                    slot.step = Some(step);
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Command::Detach(key, reply) => {
                if let Ok(owner) = self.call_owner(&key) {
                    if let Some(slot) = owner.borrow_mut().calls.get_mut(&key.call) {
                        slot.stop();
                    }
                }
                let _ = reply.send(Ok(()));
            }
        }
    }
}
pub(super) fn empty() -> BTreeMap<u64, Slot> {
    BTreeMap::new()
}
