//! Serialized IAX peer work, queued control delivery, and native egress composition.
use super::{
    dispatcher::PeerWake,
    outbound::Outbound,
    ring::{InboundObserver, InboundPolicy, InboundProducer, InboundRing, Observation, RingError},
    source_clock::SourceClock,
};
use crate::{
    Error,
    services::{HostServices, PeerInput, PeerIo},
};
use rpt_advanced_core::{
    audio::LinkAudioConsumer,
    link::{Peer, Protocol},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    ffi::{CString, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Work copied by control and executed only by the serialized peer owner.
pub enum Command {
    /// Wire text, with explicit best-effort advisory semantics for K messages.
    Text {
        /// NUL-terminated payload.
        text: CString,
        /// Failure must not disconnect an advisory K exchange.
        advisory: bool,
    },
    /// Completed remote-control digit.
    Digit(char),
    /// Prepared replacement generation endpoints; the old ingress becomes terminal.
    Redirect {
        /// Unique replacement inbound producer.
        inbound: InboundProducer,
        /// Unique replacement native outbound consumer.
        outbound: LinkAudioConsumer,
    },
}
/// Copied peer events for the serialized core control owner.
pub enum Event {
    /// Strictly parsed protocol text requiring topology/relay policy.
    Text(Vec<u8>),
    /// Completed digit, including the existing three-second synthesized terminator.
    Digit(char),
    /// Replacement ingress is installed; control may acknowledge old-generation detachment.
    Redirected(InboundObserver),
}

/// Whether the remote `app_rpt` negotiated explicit radio key controls.
#[derive(Clone, Copy)]
enum RadioControlState {
    /// Wait for negotiation, then allow legacy PCM keying after the deadline.
    Negotiating(Option<u64>),
    /// A `!NEWKEY!` negotiation permits key and unkey control events.
    Allowed,
    /// The compatibility timeout permits radio-key events when text is missing.
    LegacyAllowed,
    /// The peer explicitly selected PCM-based keying with `!NEWKEY1!`.
    Disabled,
}

/// app_rpt's compatibility window before falling back to audio-frame keying.
const RADIO_CONTROL_NEGOTIATION_TIMEOUT_MS: u64 = 2_000;

/// Current direct-peer media snapshot; combine with core LinkStatus for routing/retry intent.
pub struct MediaSnapshot {
    /// Negotiated decoded input rate; divide input-unit ring fields by this for seconds.
    pub input_rate: u32,
    /// Native receiver qualification currently active.
    pub receiving: bool,
    /// Terminal connection state, independent of audio-generation replacement.
    pub ended: bool,
    /// First valid downstream responder from the current/last completed receive burst.
    pub selected_source: Option<String>,
    /// Released ring counters, including occupancy/reserve/target, drift and missing PCM.
    pub ring: Observation,
    /// Copied host jitter/ingress counters when this peer exposes them.
    pub peer: Option<PeerObservation>,
}
/// Source-rate peer metrics copied from the optional host observation tail.
pub type PeerObservation = crate::abi::rptadv_peer_observation_v1;

struct PeerObservationCache {
    valid: AtomicBool,
    fields: [AtomicU64; 15],
}
impl PeerObservationCache {
    fn new() -> Self {
        Self {
            valid: AtomicBool::new(false),
            fields: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
    fn store(&self, peer: PeerObservation) {
        let fields = [
            peer.jitter_queued_packets as u64,
            peer.jitter_delay_samples as u64,
            peer.jitter_buffered_span_samples,
            peer.jitter_duplicate_drops,
            peer.jitter_late_drops,
            peer.jitter_full_drops,
            peer.jitter_missing_samples,
            peer.jitter_insertion_samples,
            peer.jitter_skipped_samples,
            peer.jitter_limit_resets,
            peer.jitter_overload_drops,
            peer.jitter_overload_span_samples,
            peer.ingress_overflow_drops,
            peer.ingress_queued_packets as u64,
            peer.ingress_capacity_packets as u64,
        ];
        for (slot, value) in self.fields.iter().zip(fields) {
            slot.store(value, Ordering::Relaxed);
        }
        self.valid.store(true, Ordering::Release);
    }
    fn load(&self) -> Option<PeerObservation> {
        if !self.valid.load(Ordering::Acquire) {
            return None;
        }
        let values = self
            .fields
            .each_ref()
            .map(|field| field.load(Ordering::Relaxed));
        Some(PeerObservation {
            struct_size: std::mem::size_of::<PeerObservation>() as u32,
            abi_version: crate::abi::RPTADV_PEER_OBSERVATION_ABI_VERSION,
            jitter_queued_packets: values[0] as u32,
            jitter_delay_samples: values[1] as u32,
            jitter_buffered_span_samples: values[2],
            jitter_duplicate_drops: values[3],
            jitter_late_drops: values[4],
            jitter_full_drops: values[5],
            jitter_missing_samples: values[6],
            jitter_insertion_samples: values[7],
            jitter_skipped_samples: values[8],
            jitter_limit_resets: values[9],
            jitter_overload_drops: values[10],
            jitter_overload_span_samples: values[11],
            ingress_overflow_drops: values[12],
            ingress_queued_packets: values[13] as u32,
            ingress_capacity_packets: values[14] as u32,
        })
    }
}
/// Single control producer/event consumer; never borrowed by audio callbacks.
pub struct PeerControl {
    commands: Producer<Command>,
    events: Consumer<Event>,
    stop: Arc<AtomicBool>,
    wake: PeerWake,
    observer: InboundObserver,
    peer_observation: Arc<PeerObservationCache>,
    #[cfg(test)]
    owner_thread: Arc<std::sync::Mutex<Option<thread::ThreadId>>>,
}
impl PeerControl {
    /// Capture this exact inbound endpoint for source-addressed command replies.
    pub fn reply_target(&self) -> rpt_advanced_core::link::PeerReply {
        self.observer.signals().reply_target()
    }
    /// Queue bounded work, returning ownership to the caller when full.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands
            .push(command)
            .map_err(|rtrb::PushError::Full(command)| command)?;
        self.wake.notify();
        Ok(())
    }
    /// Signal-only handle for the off-audio media distributor.
    pub(crate) fn wake(&self) -> PeerWake {
        self.wake.clone()
    }
    /// Read one copied event for current-policy processing outside the IAX owner.
    pub fn event(&mut self) -> Option<Event> {
        let event = self.events.pop().ok()?;
        if let Event::Redirected(observer) = &event {
            self.observer = observer.clone();
        }
        Some(event)
    }
    /// Copy diagnostics on control without formatting or allocating on audio workers.
    pub fn snapshot(&self) -> Result<MediaSnapshot, RingError> {
        let signal = self.observer.signals();
        let edge = signal.activity_edge();
        let mut source = [0; 64];
        let selected_source = signal
            .selected_source(
                if edge & 1 == 0 {
                    edge.saturating_sub(1)
                } else {
                    edge
                },
                &mut source,
            )
            .map(str::to_owned);
        Ok(MediaSnapshot {
            input_rate: self.observer.rate(),
            receiving: edge & 1 != 0,
            // A retired ring reaches EOF before control receives Redirected.
            // Only the session stop flag means the peer itself has ended.
            ended: self.stop.load(Ordering::Acquire),
            selected_source,
            ring: self.observer.observe()?,
            peer: self.peer_observation.load(),
        })
    }
    /// Stop is out-of-band so a full work queue cannot prevent teardown.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.observer.signals().end();
        self.wake.notify();
    }

    #[cfg(test)]
    pub(crate) fn owner_thread(&self) -> Option<thread::ThreadId> {
        *self.owner_thread.lock().unwrap()
    }
}
impl Drop for PeerControl {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One prepared peer session, stepped by exactly one synchronous or retained owner.
pub struct PeerSession {
    io: PeerIo,
    inbound: InboundProducer,
    peer_observation: Arc<PeerObservationCache>,
    pending_input: Option<PendingInput>,
    pending_pcm: [f32; MAX_PEER_INPUT_SAMPLES],
    peer: Peer,
    local: String,
    remote: String,
    dtmf_sequence: i32,
    commands: Consumer<Command>,
    events: Producer<Event>,
    stop: Arc<AtomicBool>,
    wake: PeerWake,
    outbound: Outbound,
    edge: u64,
    query_epoch: Option<u64>,
    replied_newkey: bool,
    radio_control: RadioControlState,
    async_epoch: Option<Instant>,
    #[cfg(test)]
    owner_thread: Arc<std::sync::Mutex<Option<thread::ThreadId>>>,
}

const MAX_PEER_INPUT_SAMPLES: usize = 4096;
const MAX_ASYNC_READS_PER_STEP: usize = 64;

/// The sole partly admitted event; later host events must wait behind it.
enum PendingInput {
    Audio { next: usize, len: usize },
    Gap(u64),
}

/// Pending operations are bounded independently of established peer count.
const PENDING_ATTACH_CAPACITY: usize = 1024;

/// Bounded peer ownership: one round-robin thread for synchronous hosts, with
/// individually retained callbacks for hosts that provide async peer steps.
pub struct PeerIoWorker {
    pending: Producer<Box<PeerSession>>,
    incoming: Option<Consumer<Box<PeerSession>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    async_peers: Vec<AsyncPeer>,
    epoch: Instant,
    #[cfg(test)]
    fail_next_spawn: bool,
}
struct AsyncPeer {
    services: HostServices,
    registration: NonNull<c_void>,
    stop: Arc<AtomicBool>,
    observer: InboundObserver,
    session: NonNull<PeerSession>,
    wake: PeerWake,
}
// SAFETY: the stable heap allocation moves only as a raw owner token. The host
// serializes callbacks; detach quiesces them before the Box is reconstructed.
unsafe impl Send for AsyncPeer {}
impl AsyncPeer {
    fn signal_stop(&self) {
        self.wake.close();
        self.stop.store(true, Ordering::Release);
        self.observer.signals().end();
        self.services.peer_step_stop(self.registration);
    }
    fn detach(self) {
        self.wake.close();
        while !self.wake.is_quiescent() {
            thread::yield_now();
        }
        self.services.peer_step_detach(self.registration);
        // SAFETY: detach guarantees no callback still borrows this peer.
        drop(unsafe { Box::from_raw(self.session.as_ptr()) });
    }
}
unsafe extern "C" fn retained_step(context: *mut c_void) -> i32 {
    let Some(session) = (unsafe { context.cast::<PeerSession>().as_mut() }) else {
        return -1;
    };
    let Some(epoch) = session.async_epoch else {
        session.stop();
        return -1;
    };
    let now_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    match catch_unwind(AssertUnwindSafe(|| session.step(now_ms))) {
        Ok(Ok(())) => 0,
        _ => {
            session.stop();
            -1
        }
    }
}
impl PeerIoWorker {
    /// Prepare the owner; a poll thread starts only for a synchronous host.
    pub fn start(epoch: Instant) -> std::io::Result<Self> {
        let (pending, incoming) = RingBuffer::<Box<PeerSession>>::new(PENDING_ATTACH_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        Ok(Self {
            pending,
            incoming: Some(incoming),
            stop,
            thread: None,
            async_peers: Vec::new(),
            epoch,
            #[cfg(test)]
            fail_next_spawn: false,
        })
    }

    fn spawn_thread(
        &mut self,
        mut incoming: Consumer<Box<PeerSession>>,
    ) -> std::io::Result<JoinHandle<()>> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_spawn) {
            return Err(std::io::Error::other("injected peer owner spawn failure"));
        }
        let worker_stop = Arc::clone(&self.stop);
        let epoch = self.epoch;
        thread::Builder::new()
            .name("rpt-peer-io".into())
            .spawn(move || {
                let mut sessions: Vec<Box<PeerSession>> = Vec::new();
                loop {
                    while let Ok(session) = incoming.pop() {
                        if sessions.try_reserve(1).is_ok() {
                            sessions.push(session);
                        } else {
                            // Only this new admission ends; existing owners keep running.
                            session.stop();
                        }
                    }
                    if worker_stop.load(Ordering::Acquire) {
                        for session in &sessions {
                            session.stop();
                        }
                        break;
                    }
                    let now_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                    let mut index = 0;
                    while index < sessions.len() {
                        if sessions[index].step(now_ms).is_err() {
                            sessions.swap_remove(index);
                        } else {
                            index += 1;
                        }
                    }
                    thread::park_timeout(if sessions.is_empty() {
                        Duration::from_millis(10)
                    } else {
                        Duration::from_millis(1)
                    });
                }
            })
    }

    /// Transfer a prepared peer without blocking or growing the bounded queue.
    pub fn attach(&mut self, session: PeerSession) -> Result<(), Box<PeerSession>> {
        self.reap();
        let mut session = Box::new(session);
        if self.stop.load(Ordering::Acquire) {
            return Err(session);
        }
        if let Some((services, peer)) = session.io.async_owner() {
            if self.async_peers.try_reserve(1).is_err() {
                return Err(session);
            }
            let stop = Arc::clone(&session.stop);
            let observer = session.inbound.observer();
            let wake = session.wake.clone();
            session.async_epoch = Some(self.epoch);
            // The host may invoke the callback before attach returns. Keep the
            // peer allocation raw and stable until detach has quiesced it.
            let context = Box::into_raw(session);
            let registration = match unsafe {
                services.peer_step_attach(peer, Some(retained_step), context.cast())
            } {
                Ok(registration) => registration,
                Err(_) => return Err(unsafe { Box::from_raw(context) }),
            };
            services.attach_wake(&wake, registration);
            self.async_peers.push(AsyncPeer {
                services,
                registration,
                stop,
                observer,
                session: NonNull::new(context).unwrap(),
                wake,
            });
            return Ok(());
        }
        if self.thread.is_none() {
            let Some(incoming) = self.incoming.take() else {
                return Err(session);
            };
            let thread = self.spawn_thread(incoming);
            self.thread = match thread {
                Ok(thread) => Some(thread),
                Err(_) => {
                    self.stop.store(true, Ordering::Release);
                    return Err(session);
                }
            };
        }
        let Some(thread) = self.thread.as_ref().filter(|thread| !thread.is_finished()) else {
            return Err(session);
        };
        self.pending
            .push(session)
            .map_err(|rtrb::PushError::Full(session)| session)?;
        thread.thread().unpark();
        Ok(())
    }

    /// Reclaim disconnected async generations off the executor callback.
    pub fn reap(&mut self) {
        let mut index = 0;
        while index < self.async_peers.len() {
            if self.async_peers[index].stop.load(Ordering::Acquire) {
                self.async_peers[index].wake.close();
                if !self.async_peers[index].wake.is_quiescent() {
                    index += 1;
                    continue;
                }
                let peer = self.async_peers.swap_remove(index);
                peer.signal_stop();
                peer.detach();
            } else {
                index += 1;
            }
        }
    }

    /// Signal all owners, then wait off-callback before releasing peer handles.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for peer in &self.async_peers {
            peer.signal_stop();
        }
        for peer in self.async_peers.drain(..) {
            peer.detach();
        }
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl Drop for PeerIoWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

impl PeerSession {
    /// Complete peer preparation before the core's final attach gate.
    pub fn prepare(
        mut io: PeerIo,
        outbound: LinkAudioConsumer,
        local: &str,
        remote: &str,
    ) -> Result<(Self, PeerControl), Error> {
        let peer = Peer::new(remote).map_err(|_| Error::Reservation)?;
        if rpt_advanced_core::config::NodeId::new(local).is_err() || local.contains('\0') {
            return Err(Error::Reservation);
        }
        let outbound = Outbound::new(io.rate(), outbound)?;
        let (inbound, _input) =
            InboundRing::open(io.rate(), InboundPolicy::Peer).map_err(|_| Error::Allocation)?;
        io.send_text(c"!NEWKEY1!")?;
        let (commands, incoming) = RingBuffer::new(64);
        let (events, outgoing) = RingBuffer::new(64);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = PeerWake::default();
        let peer_observation = Arc::new(PeerObservationCache::new());
        #[cfg(test)]
        let owner_thread = Arc::new(std::sync::Mutex::new(None));
        let control = PeerControl {
            commands,
            events: outgoing,
            stop: stop.clone(),
            wake: wake.clone(),
            observer: inbound.observer(),
            peer_observation: Arc::clone(&peer_observation),
            #[cfg(test)]
            owner_thread: Arc::clone(&owner_thread),
        };
        Ok((
            Self {
                io,
                inbound,
                peer_observation,
                pending_input: None,
                pending_pcm: [0.0; MAX_PEER_INPUT_SAMPLES],
                peer,
                local: local.into(),
                remote: remote.into(),
                dtmf_sequence: 0,
                commands: incoming,
                events,
                stop,
                wake,
                outbound,
                edge: 0,
                query_epoch: None,
                replied_newkey: false,
                radio_control: RadioControlState::Negotiating(None),
                async_epoch: None,
                #[cfg(test)]
                owner_thread,
            },
            control,
        ))
    }
    /// Bind the physical stream before publishing this session to its peer owner.
    pub(crate) fn bind_clock(&mut self, clock: Arc<SourceClock>) {
        self.outbound.bind_clock(clock);
    }
    fn event(&mut self, event: Event) -> Result<(), Error> {
        self.events.push(event).map_err(|_| Error::Write)
    }
    fn text(&mut self, bytes: Vec<u8>, now_ms: u64) -> Result<(), Error> {
        let Some(protocol) = Protocol::parse(&bytes) else {
            return Ok(());
        };
        match protocol {
            Protocol::NewKey => {
                if matches!(self.radio_control, RadioControlState::Negotiating(_)) {
                    self.radio_control = RadioControlState::Allowed;
                }
                if matches!(self.radio_control, RadioControlState::Allowed) && !self.replied_newkey
                {
                    self.replied_newkey = true;
                    // app_rpt treats this legacy exchange as best-effort and only echoes once.
                    let _ = self.io.send_text(c"!NEWKEY!");
                }
            }
            // NEWKEY1 disables separate radio-key controls; PCM remains the fallback.
            Protocol::NewKey1 => {
                self.radio_control = RadioControlState::Disabled;
                self.inbound.signals().set_radio_keyed(false);
            }
            Protocol::IaxKey => {
                self.io.send_text(c"!IAXKEY! 1 1 0 0")?;
            }
            Protocol::Disconnect => return Err(Error::Hangup),
            Protocol::Key {
                ref destination,
                ref source,
                keyed,
                ..
            } => {
                let edge = self.inbound.signals().activity_edge();
                if edge == self.edge && edge & 1 != 0 {
                    if let Some(epoch) = self.query_epoch {
                        if self.peer.accept_key(epoch, destination, source, keyed) {
                            self.inbound
                                .signals()
                                .select_source(edge, self.peer.selected_source());
                        }
                    }
                }
                self.event(Event::Text(bytes))?;
            }
            Protocol::RemoteDigit {
                ref destination,
                ref source,
                digit,
                ..
            } if destination == &self.local && source == &self.remote => {
                // Protocol::parse admits only conventional DTMF digits.
                let _ = self.peer.digit(digit, now_ms);
                self.event(Event::Digit(digit))?;
            }
            Protocol::RemoteDigit { .. } => (),
            _ => self.event(Event::Text(bytes))?,
        }
        Ok(())
    }
    // An audio owner may unkey after the peer owner captures its activity edge.
    // Recheck that exact edge before sending the query prepared for this epoch.
    fn query(&mut self, edge: u64, epoch: u64) -> Result<(), Error> {
        if let Some(query) = self.peer.query(epoch, &self.local) {
            let query = CString::new(query).map_err(|_| Error::InvalidFrame)?;
            if self.inbound.signals().activity_edge() == edge && self.io.send_text(&query).is_ok() {
                self.query_epoch = Some(epoch);
            }
        }
        Ok(())
    }
    fn flush_pending_input(&mut self) -> Result<(), Error> {
        let finished = match self.pending_input.as_mut() {
            Some(PendingInput::Audio { next, len }) => {
                let accepted = self
                    .inbound
                    .write(&self.pending_pcm[*next..*len])
                    .map_err(|_| Error::Write)?;
                *next += accepted;
                *next == *len
            }
            Some(PendingInput::Gap(remaining)) => {
                let chunk = (*remaining).min(MAX_PEER_INPUT_SAMPLES as u64);
                let accepted = self.inbound.gap(chunk).map_err(|_| Error::Write)?;
                if accepted > chunk {
                    return Err(Error::Write);
                }
                *remaining -= accepted;
                *remaining == 0
            }
            None => return Ok(()),
        };
        if finished {
            self.pending_input = None;
        }
        Ok(())
    }
    /// Execute one bounded peer iteration; any fatal error publishes EOF immediately.
    pub fn step(&mut self, now_ms: u64) -> Result<(), Error> {
        #[cfg(test)]
        {
            let mut owner = self.owner_thread.lock().unwrap();
            if owner.is_none() {
                *owner = Some(thread::current().id());
            }
        }
        let result = self.step_inner(now_ms);
        if result.is_ok() {
            if let Some(observation) = self.io.observe() {
                self.peer_observation.store(observation);
            }
        }
        if result.is_err() {
            self.inbound.signals().end();
            self.stop.store(true, Ordering::Release);
        }
        result
    }
    fn step_inner(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.stop.load(Ordering::Acquire) {
            return Err(Error::Hangup);
        }
        if let RadioControlState::Negotiating(deadline) = self.radio_control {
            let deadline = deadline
                .unwrap_or_else(|| now_ms.saturating_add(RADIO_CONTROL_NEGOTIATION_TIMEOUT_MS));
            self.radio_control = if now_ms >= deadline {
                RadioControlState::LegacyAllowed
            } else {
                RadioControlState::Negotiating(Some(deadline))
            };
        }
        for _ in 0..64 {
            let Ok(command) = self.commands.pop() else {
                break;
            };
            match command {
                Command::Text { text, advisory } => {
                    let result = self.io.send_text(&text);
                    if !advisory {
                        result?;
                    }
                }
                Command::Digit(digit) => {
                    self.dtmf_sequence = self.dtmf_sequence.wrapping_add(1);
                    let text = CString::new(format!(
                        "D {} {} {} {digit}",
                        self.remote, self.local, self.dtmf_sequence
                    ))
                    .map_err(|_| Error::InvalidFrame)?;
                    self.io.send_text(&text)?;
                }
                Command::Redirect { inbound, outbound } => {
                    self.inbound = inbound;
                    self.pending_input = None;
                    self.outbound.redirect(outbound)?;
                    self.peer.activity(false, now_ms);
                    self.edge = 0;
                    self.query_epoch = None;
                    self.event(Event::Redirected(self.inbound.observer()))?;
                }
            }
        }
        let edge = self.inbound.signals().activity_edge();
        if edge != self.edge {
            self.peer.activity(false, now_ms);
            self.query_epoch = None;
            self.edge = edge;
        }
        if let Some(epoch) = self.peer.activity(edge & 1 != 0, now_ms) {
            self.query(edge, epoch)?;
        }
        let read_budget = if self.io.async_owner().is_some() {
            MAX_ASYNC_READS_PER_STEP
        } else {
            1
        };
        for _ in 0..read_budget {
            self.flush_pending_input()?;
            if self.pending_input.is_some() || !self.io.ready()? {
                break;
            }
            let mut event = None;
            let mut radio_keyed = None;
            let mut admission_error = None;
            let radio_control_allowed = matches!(
                self.radio_control,
                RadioControlState::Allowed | RadioControlState::LegacyAllowed
            );
            let inbound = &mut self.inbound;
            let pending_input = &mut self.pending_input;
            let pending_pcm = &mut self.pending_pcm;
            let seen = self.io.read(|input| match input {
                PeerInput::Text(bytes) => event = Some(Event::Text(bytes.into())),
                PeerInput::Digit(digit) => event = Some(Event::Digit(digit)),
                PeerInput::Audio(samples) => {
                    if samples.len() > MAX_PEER_INPUT_SAMPLES {
                        admission_error = Some(Error::InvalidFrame);
                    } else {
                        match inbound.write(samples) {
                            Ok(accepted) if accepted <= samples.len() => {
                                if accepted < samples.len() {
                                    pending_pcm[..samples.len()].copy_from_slice(samples);
                                    *pending_input = Some(PendingInput::Audio {
                                        next: accepted,
                                        len: samples.len(),
                                    });
                                }
                            }
                            _ => admission_error = Some(Error::Write),
                        }
                    }
                }
                PeerInput::Gap(samples) => {
                    let chunk = samples.min(MAX_PEER_INPUT_SAMPLES) as u64;
                    match inbound.gap(chunk) {
                        Ok(accepted) if accepted <= chunk => {
                            let remaining = samples as u64 - accepted;
                            if remaining != 0 {
                                *pending_input = Some(PendingInput::Gap(remaining));
                            }
                        }
                        _ => admission_error = Some(Error::Write),
                    }
                }
                PeerInput::RadioKey => radio_keyed = Some(true),
                PeerInput::RadioUnkey => radio_keyed = Some(false),
            })?;
            if let Some(error) = admission_error {
                return Err(error);
            }
            if let Some(keyed) = radio_keyed {
                if !keyed || radio_control_allowed {
                    self.inbound.signals().set_radio_keyed(keyed);
                }
            }
            match event {
                Some(Event::Text(bytes)) => self.text(bytes, now_ms)?,
                Some(Event::Digit(digit)) => {
                    self.peer.digit(digit, now_ms);
                    self.event(Event::Digit(digit))?;
                }
                _ => (),
            }
            if !seen {
                break;
            }
        }
        if self.peer.expire_digit(now_ms) {
            self.event(Event::Digit('#'))?;
        }
        let deadline = self.outbound.step(&mut self.io, now_ms)?;
        let now = now_ms.saturating_mul(1_000_000);
        let mut after = deadline.map_or(u64::MAX, |deadline| deadline.saturating_sub(now));
        // Space can become available without an activity edge; retry only that
        // backpressure. The media dispatcher wakes ordinary activity changes.
        if self.pending_input.is_some() {
            after = after.min(10_000_000);
        }
        if self.commands.slots() != 0 {
            after = 0;
        }
        if let Some(deadline) = self.peer.deadline_ms() {
            after = after.min(deadline.saturating_sub(now_ms).saturating_mul(1_000_000));
        }
        if let RadioControlState::Negotiating(Some(deadline)) = self.radio_control {
            after = after.min(deadline.saturating_sub(now_ms).saturating_mul(1_000_000));
        }
        // Conversion/control work may have consumed part of the wait already.
        if let Some(epoch) = self.async_epoch {
            let elapsed = epoch.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            if after != u64::MAX {
                after = after.saturating_sub(elapsed.saturating_sub(now));
            }
        }
        self.io.set_deadline(after)?;
        Ok(())
    }
    /// Request this owner to stop before the next serialized step.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.inbound.signals().end();
    }
}

#[cfg(test)]
pub(crate) mod tests;
