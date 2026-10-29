//! Serialized peer work, protocol-specific ingress, and native egress composition.
use super::{
    dispatcher::PeerWake,
    outbound::{EgressObservation, EgressStatistics, Outbound},
    ring::{
        FinishState, InboundObserver, InboundPolicy, InboundProducer, InboundRing, Observation,
        RingError,
    },
    source_clock::SourceClock,
};
use crate::{
    Error,
    services::{HostServices, PeerInput, PeerIo, SourceCompletion, SourceInput},
};
use rpt_advanced_core::{
    audio::LinkAudioConsumer,
    link::{
        ClassifiedText, HumanMessage, Peer, Protocol, ProtocolId, TextAudience, TextOrigin,
        classify_asl,
    },
};
use rtrb::{Consumer, Producer, RingBuffer};
mod source;
use source::{Decoded, SourceIngress};
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
    /// Literal protocol-native human text; rejection drops only this message.
    HumanText {
        /// Original represented sender, never command authority.
        sender: String,
        /// Original body, never synthesized peer PCM or control text.
        body: Arc<str>,
    },
    /// Prepared replacement generation endpoints; the old ingress becomes terminal.
    Redirect {
        /// Unique replacement inbound producer.
        inbound: InboundProducer,
        /// Unique replacement native outbound consumer.
        outbound: LinkAudioConsumer,
        /// Resolved node setting, applied before this generation admits source PCM.
        dtmf_muting: bool,
    },
}
/// Copied peer events for the serialized core control owner.
pub enum Event {
    /// Strictly parsed protocol text requiring topology/relay policy.
    Text(Vec<u8>),
    /// Validated human text, distinct from controller signaling and commands.
    HumanText(HumanMessage),
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
    /// Bounded egress observations copied without borrowing the peer owner.
    pub egress: EgressObservation,
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
    fields: [AtomicU64; 20],
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
            peer.egress_datagrams,
            peer.egress_would_block,
            peer.egress_reliable_retries,
            peer.egress_reliable_pending as u64,
            peer.egress_reply_pending as u64,
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
            egress_datagrams: values[15],
            egress_would_block: values[16],
            egress_reliable_retries: values[17],
            egress_reliable_pending: values[18] as u32,
            egress_reply_pending: values[19] as u32,
        })
    }
}
/// Single control producer/event consumer; never borrowed by audio callbacks.
pub struct PeerControl {
    can_forward_text: bool,
    text_identity: Arc<TextIdentity>,
    egress: Arc<EgressStatistics>,
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
    /// Publish a station-generation fence before attaching/publishing its reader.
    pub fn bind_text_generation(&self, generation: u64) -> TextOrigin {
        self.text_identity
            .station
            .store(generation, Ordering::Release);
        self.text_origin()
    }
    /// Snapshot this authenticated connection's current local-media generation.
    pub fn text_origin(&self) -> TextOrigin {
        self.text_identity.snapshot()
    }
    /// Copied per-connection capability; does not borrow the serial I/O owner.
    pub fn can_forward_text(&self) -> bool {
        self.can_forward_text
    }
    /// Capture this exact inbound endpoint for source-addressed command replies.
    pub fn reply_target(&self) -> rpt_advanced_core::link::PeerReply {
        self.observer.signals().reply_target()
    }
    /// Preserve the protocol's immutable ring policy when preparing a generation.
    pub(crate) fn inbound_policy(&self) -> InboundPolicy {
        self.observer.policy()
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
    /// Read terminal state without copying diagnostics; this is not quiescence.
    pub(crate) fn ended(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    /// Read one copied event for current-policy processing outside the IAX owner.
    pub fn event(&mut self) -> Option<Event> {
        // ACCEPT can reach the caller before this station's first audio
        // generation is published. Keep that first human message queued.
        let queued = self.events.peek().ok()?;
        if matches!(queued, Event::HumanText(message)
                if message.origin.station_generation == 0
                    && self.text_identity.station.load(Ordering::Acquire) == 0)
        {
            return None;
        }
        let mut event = self.events.pop().ok()?;
        if let Event::HumanText(message) = &mut event {
            if message.origin.station_generation == 0 {
                message.origin = self.text_origin();
            }
        }
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
            egress: self.egress.snapshot(),
            input_rate: self.observer.rate(),
            receiving: edge & 1 != 0,
            // A retired ring reaches EOF before control receives Redirected.
            // Only the session stop flag means the peer itself has ended.
            ended: self.ended(),
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
    text_identity: Arc<TextIdentity>,
    io: PeerIo,
    inbound: InboundProducer,
    peer_observation: Arc<PeerObservationCache>,
    pending_input: Option<PendingInput>,
    pending_pcm: [f32; MAX_PEER_INPUT_SAMPLES],
    source_finish: Option<u64>,
    source_completion: SourceCompletion,
    source_dirty: bool,
    source: Option<SourceIngress>,
    peer: Peer,
    local: String,
    remote: String,
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

static NEXT_TEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
struct TextIdentity {
    station: AtomicU64,
    connection: u64,
    peer: String,
    protocol: u32,
}
impl TextIdentity {
    fn snapshot(&self) -> TextOrigin {
        TextOrigin {
            station_generation: self.station.load(Ordering::Acquire),
            connection_generation: self.connection,
            peer: self.peer.clone(),
            protocol: self.protocol,
        }
    }
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
        session.fail("owner without a registered epoch", Error::Thread);
        return -1;
    };
    let now_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    match catch_unwind(AssertUnwindSafe(|| session.step(now_ms))) {
        Ok(Ok(())) => 0,
        Ok(Err(_)) => -1,
        Err(_) => {
            session.fail("owner panic", Error::Thread);
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
                            session.fail("owner admission allocation", Error::Allocation);
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
        dtmf_muting: bool,
    ) -> Result<(Self, PeerControl), Error> {
        let peer = Peer::new(remote).map_err(|_| Error::Reservation)?;
        if rpt_advanced_core::config::NodeId::new(local).is_err() || local.contains('\0') {
            return Err(Error::Reservation);
        }
        let outbound = Outbound::new(io.rate(), outbound)?;
        let source = io.source_media();
        if source && io.rate() != 8000 {
            return Err(Error::UnsupportedFormat);
        }
        let (inbound, _input) = InboundRing::open(
            io.rate(),
            if source {
                InboundPolicy::Echo
            } else {
                InboundPolicy::Peer
            },
        )
        .map_err(|_| Error::Allocation)?;
        if !source {
            io.send_text(c"!NEWKEY1!")?;
        }
        let (commands, incoming) = RingBuffer::new(64);
        let (events, outgoing) = RingBuffer::new(64);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = PeerWake::default();
        let peer_observation = Arc::new(PeerObservationCache::new());
        let text_identity = Arc::new(TextIdentity {
            station: AtomicU64::new(0),
            connection: NEXT_TEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
            peer: remote.into(),
            protocol: io.identity().protocol() as u32,
        });
        #[cfg(test)]
        let owner_thread = Arc::new(std::sync::Mutex::new(None));
        let control = PeerControl {
            can_forward_text: io.can_forward_text(),
            text_identity: Arc::clone(&text_identity),
            egress: outbound.statistics.clone(),
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
                text_identity,
                io,
                inbound,
                peer_observation,
                pending_input: None,
                pending_pcm: [0.0; MAX_PEER_INPUT_SAMPLES],
                source_finish: None,
                source_completion: SourceCompletion::Pending,
                source_dirty: false,
                source: if source {
                    Some(SourceIngress::new(dtmf_muting)?)
                } else {
                    None
                },
                peer,
                local: local.into(),
                remote: remote.into(),
                commands: incoming,
                events,
                stop,
                wake,
                outbound,
                edge: 0,
                query_epoch: None,
                replied_newkey: false,
                radio_control: if source {
                    RadioControlState::Disabled
                } else {
                    RadioControlState::Negotiating(None)
                },
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
    fn text(&mut self, bytes: Vec<u8>, _now_ms: u64) -> Result<(), Error> {
        let Some(protocol) = Protocol::parse(&bytes) else {
            if let ClassifiedText::Human(message) =
                classify_asl(self.text_identity.snapshot(), &bytes)
            {
                // A full local event queue rejects only the newest human message,
                // never a healthy peer connection or its unrelated audio/control.
                let _ = self.events.push(Event::HumanText(message));
            }
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
            // Addressed D messages retain their source and sequence through shared control.
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
    // Echo retains rejected prefixes. Only the exact ring token can release the
    // selected talker; provider/jitter completion alone is not audible completion.
    fn read_source(&mut self, now_ms: u64) -> Result<bool, Error> {
        for _ in 0..MAX_ASYNC_READS_PER_STEP {
            let completion = match self.source_finish {
                Some(token) => match self
                    .inbound
                    .finish_status(token)
                    .map_err(|_| Error::Write)?
                {
                    FinishState::Pending => SourceCompletion::Pending,
                    FinishState::Drained => SourceCompletion::Drained,
                    FinishState::Cancelled => SourceCompletion::Cancelled,
                    FinishState::Failed => return Err(Error::Write),
                },
                None => self.source_completion,
            };
            let mut admission_error = None;
            let mut accepted = false;
            let inbound = &mut self.inbound;
            let events = &mut self.events;
            let identity = &self.text_identity;
            let source = self.source.as_mut().ok_or(Error::Operation)?;
            let scratch = &mut self.pending_pcm;
            let peer = &mut self.peer;
            let finishing = self.source_finish.is_some();
            let result = self.io.read_source(completion, |input| {
                // Future PCM must not advance detection or its current mute gate
                // while the previous finite burst is still audible.
                if finishing && !matches!(&input, SourceInput::Text { .. }) {
                    return 0;
                }
                let credit = match input {
                    SourceInput::Audio {
                        start,
                        samples,
                        concealed,
                    } => source.audio(
                        inbound,
                        scratch,
                        Decoded {
                            start,
                            pcm: samples,
                            concealed,
                        },
                        |digit| {
                            peer.digit(digit, now_ms);
                            if events.push(Event::Digit(digit)).is_err() {
                                admission_error = Some(Error::Write);
                            }
                        },
                    ),
                    SourceInput::Gap { start, count } => {
                        source.gap(inbound, start, count, |digit| {
                            peer.digit(digit, now_ms);
                            if events.push(Event::Digit(digit)).is_err() {
                                admission_error = Some(Error::Write);
                            }
                        })
                    }
                    SourceInput::Text {
                        sender,
                        body,
                        represented_node_number,
                    } => {
                        let _ = events.push(Event::HumanText(HumanMessage {
                            origin: identity.snapshot(),
                            source: sender.to_owned(),
                            represented_source: (represented_node_number != 0).then(|| {
                                rpt_advanced_core::link::PeerIdentity::parse(
                                    ProtocolId::EchoLink,
                                    &represented_node_number.to_string(),
                                )
                                .expect("positive provider identity")
                            }),
                            audience: TextAudience::Broadcast,
                            body: Arc::from(body),
                        }));
                        return 0;
                    }
                };
                match credit {
                    Ok(count) => {
                        accepted |= count != 0;
                        count
                    }
                    Err(error) => {
                        admission_error = Some(error);
                        0
                    }
                }
            })?;
            if let Some(error) = admission_error {
                return Err(error);
            }
            if completion != SourceCompletion::Pending {
                self.source_finish = None;
                self.source_completion = SourceCompletion::Pending;
                self.source_dirty = false;
                source.reset()?;
            }
            self.source_dirty |= accepted;
            if let Some(keyed) = result.activity {
                self.inbound.signals().set_radio_keyed(keyed);
            }
            if result.closed {
                return Err(Error::Hangup);
            }
            if result.finish_pending {
                if self.source_finish.is_none() {
                    let mut overflow = false;
                    if !source.finish(&mut self.inbound, |digit| {
                        self.peer.digit(digit, now_ms);
                        overflow |= self.events.push(Event::Digit(digit)).is_err();
                    })? {
                        return Ok(true);
                    }
                    if overflow {
                        return Err(Error::Write);
                    }
                    if self.peer.deadline_ms().is_some() {
                        self.peer.digit('#', now_ms);
                        self.event(Event::Digit('#'))?;
                    }
                    self.source_finish = Some(self.inbound.finish().map_err(|_| Error::Write)?);
                }
                return Ok(true);
            }
            if result.progress == 2 {
                return Ok(true);
            }
            if result.progress == 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Execute one bounded iteration; report its first unexpected failure and publish EOF.
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
        if let Err(error) = result {
            self.fail("peer I/O", error);
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
                Command::HumanText { sender, body } => {
                    // Chat queue failure is independent of media/session health.
                    let _ = self.io.send_human_text(&sender, &body);
                }
                Command::Text { text, advisory } => {
                    if self.io.identity().protocol() == ProtocolId::EchoLink
                        && Protocol::parse(text.to_bytes()).is_some()
                    {
                        continue;
                    }
                    let result = self.io.send_text(&text);
                    if !advisory {
                        result?;
                    }
                }
                Command::Redirect {
                    inbound,
                    outbound,
                    dtmf_muting,
                } => {
                    if self.source_dirty {
                        self.source_completion = SourceCompletion::Cancelled;
                    }
                    self.source_finish = None;
                    self.source_dirty = false;
                    if let Some(source) = &mut self.source {
                        source.reset()?;
                        source.set_muting(dtmf_muting);
                    }
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
        if !self.io.source_media() {
            if let Some(epoch) = self.peer.activity(edge & 1 != 0, now_ms) {
                self.query(edge, epoch)?;
            }
        }
        let source_wait = if self.io.source_media() {
            self.read_source(now_ms)?
        } else {
            false
        };
        let read_budget = if self.io.async_owner().is_some() {
            MAX_ASYNC_READS_PER_STEP
        } else {
            1
        };
        for _ in 0..if self.io.source_media() {
            0
        } else {
            read_budget
        } {
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
            if let Some(source) = &mut self.source {
                source.timeout(&self.inbound);
            }
        }
        let deadline = self.outbound.step(&mut self.io, now_ms)?;
        let now = now_ms.saturating_mul(1_000_000);
        let mut after = deadline.map_or(u64::MAX, |deadline| deadline.saturating_sub(now));
        // Space can become available without an activity edge; retry only that
        // backpressure. The media dispatcher wakes ordinary activity changes.
        if self.pending_input.is_some() || source_wait {
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

    /// Record only the first unexpected termination; requested stops already own the flag.
    fn fail(&self, cause: &str, error: Error) {
        let first = !self.stop.swap(true, Ordering::AcqRel);
        self.inbound.signals().end();
        if first {
            let message = format!(
                "rpt_advanced {} peer {}: unexpected termination during {cause}: {error:?}",
                self.local, self.remote
            );
            #[cfg(test)]
            crate::fixture::error_log(3, &message);
            {
                // Preparation rejects NUL identities; causes and Error names are literals.
                let message = CString::new(message).expect("peer failure text contains no NUL");
                unsafe extern "C" {
                    fn syslog(priority: std::ffi::c_int, format: *const std::ffi::c_char, ...);
                }
                // SAFETY: the constant format receives one live NUL-terminated string.
                // Priority 3 is LOG_ERR, matching existing product syslog reporting.
                unsafe {
                    syslog(3, c"%s".as_ptr(), message.as_ptr());
                }
            }
        }
    }
}

impl Drop for PeerSession {
    fn drop(&mut self) {
        self.fail("owner release without a stop request", Error::Thread);
    }
}

#[cfg(test)]
pub(crate) mod tests;
