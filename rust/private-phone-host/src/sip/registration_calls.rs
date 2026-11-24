//! Call admission and retained product steps on the existing SIP reactor.
use super::{Command as OwnerCommand, Entry, Error, Reactor, Reply};
use crate::{
    sip::call_media::{Media, Offer},
    accounts::Route as PreparedAccount,
};
use crate::sip_provider::Call as Observation;
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
    /// Start one exact outgoing call; local is the initiating controller binding.
    pub(crate) fn originate(
        &self,
        account: &PreparedAccount,
        local: &str,
        destination: String,
        jitter: Arc<crate::JitterProvider>,
    ) -> Result<Dial, Error> {
        if self
            .owner
            .get()
            .is_some_and(|owner| *owner == std::thread::current().id())
        {
            return Err(Error::Owner);
        }
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        let close = Arc::new(AtomicBool::new(false));
        let mut dial = Dial {
            result,
            pending: Some(OwnerCommand::Call(Command::Originate(
                account.account.account_id().to_string(),
                account.generation(),
                local.to_owned(),
                destination,
                jitter,
                Arc::clone(&close),
                reply,
            ))),
            close,
            client: self.clone(),
            finished: false,
        };
        dial.submit()?;
        Ok(dial)
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

/// A unique pending originator claim; dropping it cancels only that exact call.
pub(crate) struct Dial {
    result: std::sync::mpsc::Receiver<Result<Handle, Error>>,
    pending: Option<OwnerCommand>,
    close: Arc<AtomicBool>,
    client: Client,
    finished: bool,
}
impl Dial {
    /// Retain a refused command locally; never block cancellation on queue space.
    fn submit(&mut self) -> Result<(), Error> {
        if let Some(command) = self.pending.take() {
            match self.client.sender.try_send(command) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(command)) => self.pending = Some(command),
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(Error::Owner),
            }
        }
        Ok(())
    }
    pub(crate) fn try_complete(&mut self) -> Result<Option<Handle>, Error> {
        if self.finished {
            return Err(Error::Call);
        }
        self.submit()?;
        match self.result.try_recv() {
            Ok(result) => {
                self.finished = true;
                result.map(Some)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.finished = true;
                Err(Error::Owner)
            }
        }
    }
}
impl Drop for Dial {
    fn drop(&mut self) {
        if !self.finished {
            self.close.store(true, Ordering::Release);
            self.client.wake.notify_one();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Incoming,
    Outgoing,
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
        jitter: Arc<crate::JitterProvider>,
    ) -> Result<Handle, Error> {
        let remote = format!("{:016x}{:016x}", self.key.generation, self.key.call);
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
    pub(crate) digit_capabilities: u32,
    pub(crate) pcm_rate: u32,
    pub(crate) direction: Direction,
    state: AtomicU32,
    deadline: Mutex<Option<std::time::Instant>>,
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
    pub(crate) fn established(&self) -> bool {
        self.state.load(Ordering::Acquire) == 3 && self.live()
    }
    pub(crate) fn wake(&self) {
        self.client.wake.notify_one();
    }
    pub(crate) fn set_deadline(&self, after:u64) -> Result<(),Error> {
        let next=if after==u64::MAX {None} else {
            Some(std::time::Instant::now().checked_add(std::time::Duration::from_nanos(after)).ok_or(Error::Call)?)
        };
        *self.deadline.lock().map_err(|_|Error::Owner)?=next;
        if self.client.owner.get()!=Some(&std::thread::current().id()){self.wake();}
        Ok(())
    }
    pub(crate) fn answer(&self) -> Result<(), Error> {
        if self.direction != Direction::Incoming {
            return Err(Error::Call);
        }
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
    Originate(
        String,
        u64,
        String,
        String,
        Arc<crate::JitterProvider>,
        Arc<AtomicBool>,
        Reply<Handle>,
    ),
    Prepare(
        Key,
        Arc<crate::JitterProvider>,
        String,
        Reply<Handle>,
    ),
    Answer(Key, Reply<()>),
    Attach(Key, Step, Reply<()>),
    Detach(Key, Reply<()>),
}
struct Outgoing {
    local: String,
    offer: Offer,
    jitter: Arc<crate::JitterProvider>,
    reply: Reply<Handle>,
}
pub(super) struct Slot {
    outgoing: Option<Outgoing>,
    observation: Observation,
    close: Arc<AtomicBool>,
    dispatched: bool,
    shared: Option<Arc<Shared>>,
    step: Option<Step>,
}
impl Slot {
    pub(super) fn set_delays(&self, delays_ms: [u32; 3]) -> Result<(), Error> {
        if let Some(shared) = &self.shared {
            shared.media.lock().map_err(|_| Error::Owner)?
                .set_delays(delays_ms).map_err(|_| Error::Call)?;
        }
        Ok(())
    }
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
                        outgoing: None,
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
    pub(super) fn earliest_call_deadline(&self)->Option<std::time::Instant>{
        self.active.values().chain(&self.retiring).filter_map(|owner| {
            let entry=owner.borrow();
            entry.calls.values().filter_map(|slot| {
                slot.step.as_ref()?;
                let shared=slot.shared.as_ref()?;
                *shared.deadline.lock().ok()?
            }).min()
        }).min()
    }
    pub(super) fn service_calls(&mut self) {
        for owner in self.active.values().chain(&self.retiring) {
            let mut callbacks: [_; 8] = std::array::from_fn(|_| None);
            {
                let mut entry = owner.borrow_mut();
                let account = Arc::clone(&entry.settings);
                let generation = account.generation();
                let id = account.account.account_id().to_string();
                let done = entry.done;
                let registered = entry.last.state == 2 && entry.last.generation == generation;
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
                    if !slot.dispatched
                        && !retiring
                        && slot.observation.state == 1
                        && slot.observation.direction == 0
                    {
                        if !registered || slot.observation.source().ok() != Some(account.remote()) {
                            slot.dispatched = true;
                            slot.close.store(true, Ordering::Release);
                            let _ = native.respond_call(*token, 403, &[]);
                            continue;
                        }
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
                        if let Ok(mut deadline)=shared.deadline.lock()
                            && deadline.is_some_and(|t|t<=std::time::Instant::now()) { *deadline=None; }
                        if shared.stopped.load(Ordering::Acquire) {
                            slot.step = None;
                        } else if slot.observation.state >= 3
                            || shared.close.load(Ordering::Acquire)
                        {
                            // A host close must retire the product even before ACK
                            // or when native signaling cannot emit its terminal event.
                            callbacks[index] =
                                Some((Arc::clone(shared), step.callback, step.context));
                        }
                    }
                }
                calls.retain(|token, slot| {
                    if slot.observation.state != 5 || slot.step.is_some() {
                        return true;
                    }
                    if !done && native.release_call(*token).is_err() {
                        return true;
                    }
                    if let Some(shared) = &slot.shared {
                        match shared.media.lock() {
                            Ok(media) => match media.observe() {
                                Ok(observation) => eprintln!(
                                    "phone SIP playout: jitter={:?} decode_recovery_frames={} native_plc_frames={} fallback_frames={}",
                                    observation.jitter, observation.decode_recovery_frames,
                                    observation.native_plc_frames, observation.fallback_frames
                                ),
                                Err(_) => eprintln!("phone SIP: playout observation unavailable"),
                            },
                            Err(_) => eprintln!("phone SIP: media owner unavailable for observation"),
                        }
                    }
                    false
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
    /// Complete at most the existing native slot count, on the same serial reactor.
    /// No RefCell or media borrow is retained across endpoint preparation.
    pub(super) async fn service_outgoing(&mut self) {
        for owner in self.active.values().chain(&self.retiring) {
            let tokens: [Option<u64>; 8] = {
                let entry = owner.borrow();
                let mut tokens = [None; 8];
                for (index, (token, slot)) in entry.calls.iter().enumerate() {
                    if slot.outgoing.is_some()
                        && (slot.observation.state != 1
                            || slot.close.load(Ordering::Acquire)
                            || entry.retiring
                            || entry.done)
                    {
                        tokens[index] = Some(*token);
                    }
                }
                tokens
            };
            for token in tokens.into_iter().flatten() {
                let (outgoing, observation, closed, retiring, account, delays_ms) = {
                    let mut entry = owner.borrow_mut();
                    let retiring = entry.retiring || entry.done;
                    let account = Arc::clone(&entry.settings);
                    let delays_ms = entry.delays_ms;
                    let slot = entry.calls.get_mut(&token).expect("retained outgoing slot");
                    (
                        slot.outgoing.take().expect("pending outgoing owner"),
                        slot.observation.clone(),
                        Arc::clone(&slot.close),
                        retiring,
                        account,
                        delays_ms,
                    )
                };
                let Outgoing {
                    local,
                    offer,
                    jitter,
                    reply,
                } = outgoing;
                let result = async {
                    if retiring
                        || closed.load(Ordering::Acquire)
                        || observation.state != 2
                        || observation.direction != 1
                    {
                        return Err(Error::Call);
                    }
                    let media = offer
                        .accept(observation.offer(), jitter, delays_ms)
                        .await
                        .map_err(|_| Error::Call)?;
                    if closed.load(Ordering::Acquire) {
                        return Err(Error::Call);
                    }
                    let source = observation.source()?;
                    let mut entry = owner.borrow_mut();
                    if entry.retiring || entry.done || entry.last.state != 2
                        || entry.last.generation != account.generation()
                        || source != account.remote() {
                        return Err(Error::Call);
                    }
                    entry.account.acknowledge_call(token)?;
                    let id = account.account.account_id().to_string();
                    let shared = Arc::new(Shared {
                        key: Key {
                            account: id.clone(),
                            generation: account.generation(),
                            call: token,
                        },
                        account: id,
                        local,
                        remote: format!("{:016x}{token:016x}", account.generation()),
                        source,
                        digit_capabilities: media.digit_capabilities,
                        pcm_rate: media.pcm_rate(),
                        direction: Direction::Outgoing,
                        media: Mutex::new(media),
                        // ACK was submitted. The next native observation alone confirms ready.
                        state: AtomicU32::new(observation.state), deadline: Mutex::new(None),
                        close: Arc::clone(&closed),
                        stopped: AtomicBool::new(false),
                        client: self.calls.clone(),
                    });
                    entry
                        .calls
                        .get_mut(&token)
                        .expect("retained outgoing slot")
                        .shared = Some(Arc::clone(&shared));
                    Ok(Handle { shared })
                }
                .await;
                if result.is_err() {
                    closed.store(true, Ordering::Release);
                    if observation.state < 4 {
                        let _ = owner.borrow().account.hangup_call(token);
                    }
                }
                let _ = reply.send(result);
            }
        }
    }
    fn call_owner(&self, key: &Key) -> Result<super::Owner, Error> {
        self.active
            .values()
            .chain(&self.retiring)
            .find(|entry| {
                let entry = entry.borrow();
                entry.settings.account.account_id().to_string() == key.account
                    && entry.settings.generation() == key.generation
            })
            .cloned()
            .ok_or(Error::Call)
    }
    pub(super) async fn call_command(&mut self, command: Command) {
        match command {
            Command::Originate(
                id,
                generation,
                local,
                destination,
                jitter,
                close,
                reply,
            ) => {
                let result = async {
                    if close.load(Ordering::Acquire) {
                        return Err(Error::Call);
                    }
                    let owner = self.active.get(&id).cloned().ok_or(Error::Call)?;
                    let route = {
                        let entry = owner.borrow();
                        if entry.retiring
                            || entry.done
                            || entry.settings.generation() != generation
                            || entry.last.state != 2 || entry.last.generation != generation
                        {
                            return Err(Error::Call);
                        }
                        entry.settings.remote()
                    };
                    let offer = Offer::new(route, generation, Arc::clone(&self.catalog))
                        .await
                        .map_err(|_| Error::Call)?;
                    if close.load(Ordering::Acquire) {
                        return Err(Error::Call);
                    }
                    let token = owner
                        .borrow_mut()
                        .account
                        .originate_call(&destination, offer.description())?;
                    Ok((owner, offer, token))
                }
                .await;
                match result {
                    Ok((owner, offer, token)) => {
                        let mut observation = Observation::default();
                        observation.generation = generation;
                        observation.call = token;
                        observation.state = 1;
                        observation.direction = 1;
                        owner.borrow_mut().calls.insert(
                            token,
                            Slot {
                                outgoing: Some(Outgoing {
                                    local,
                                    offer,
                                    jitter,
                                    reply,
                                }),
                                observation,
                                close,
                                dispatched: true,
                                shared: None,
                                step: None,
                            },
                        );
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Prepare(key, jitter, remote, reply) => {
                let result = async {
                    let owner = self.call_owner(&key)?;
                    let (offer, delays_ms) = {
                        let entry = owner.borrow();
                        let slot = entry.calls.get(&key.call).ok_or(Error::Call)?;
                        if entry.retiring
                            || entry.last.state != 2 || entry.last.generation != key.generation
                            || slot.observation.source().ok() != Some(entry.settings.remote())
                            || slot.observation.direction != 0
                            || slot.observation.state != 1
                            || slot.shared.is_some()
                            || slot.close.load(Ordering::Acquire)
                        {
                            return Err(Error::Call);
                        }
                        (slot.observation.offer().to_vec(), entry.delays_ms)
                    };
                    let media = Media::new(&offer, key.call, jitter, delays_ms, &self.catalog)
                        .await
                        .map_err(|_| Error::Call)?;
                    let mut entry = owner.borrow_mut();
                    let local = entry.settings.account.port_id().to_string();
                    let slot = entry.calls.get_mut(&key.call).ok_or(Error::Call)?;
                    let shared = Arc::new(Shared {
                        key: key.clone(),
                        account: key.account.clone(),
                        local,
                        remote,
                        source: slot.observation.source()?,
                        digit_capabilities: media.digit_capabilities,
                        pcm_rate: media.pcm_rate(),
                        direction: Direction::Incoming,
                        media: Mutex::new(media),
                        state: AtomicU32::new(slot.observation.state), deadline: Mutex::new(None),
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
