//! Serialized IAX peer work, queued control delivery, and native egress composition.
use super::{
    egress::Egress,
    ring::{InboundObserver, InboundPolicy, InboundProducer, InboundRing, Observation, RingError},
};
use crate::{
    Error,
    services::{PeerInput, PeerIo},
};
use rpt_advanced_core::{
    audio::LinkAudioConsumer,
    link::{Peer, Protocol},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    ffi::CString,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
    /// Terminal EOF for this exact endpoint generation.
    pub ended: bool,
    /// First valid downstream responder from the current/last completed receive burst.
    pub selected_source: Option<String>,
    /// Released ring counters, including occupancy/reserve/target, drift and missing PCM.
    pub ring: Observation,
}
/// Single control producer/event consumer; never borrowed by audio callbacks.
pub struct PeerControl {
    commands: Producer<Command>,
    events: Consumer<Event>,
    stop: Arc<AtomicBool>,
    observer: InboundObserver,
    #[cfg(test)]
    owner_thread: Arc<std::sync::Mutex<Option<thread::ThreadId>>>,
}
impl PeerControl {
    /// Queue bounded work, returning ownership to the caller when full.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands
            .push(command)
            .map_err(|rtrb::PushError::Full(command)| command)
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
            ended: signal.ended(),
            selected_source,
            ring: self.observer.observe()?,
        })
    }
    /// Stop is out-of-band so a full work queue cannot prevent teardown.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.observer.signals().end();
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

/// One prepared peer session, stepped only by the shared peer-I/O owner.
pub struct PeerSession {
    io: PeerIo,
    inbound: InboundProducer,
    peer: Peer,
    local: String,
    remote: String,
    dtmf_sequence: i32,
    commands: Consumer<Command>,
    events: Producer<Event>,
    stop: Arc<AtomicBool>,
    outbound: LinkAudioConsumer,
    egress: Egress,
    native: [f32; 960],
    next_audio: u64,
    sent_audio: bool,
    edge: u64,
    query_epoch: Option<u64>,
    replied_newkey: bool,
    radio_control: RadioControlState,
    #[cfg(test)]
    owner_thread: Arc<std::sync::Mutex<Option<thread::ThreadId>>>,
}

/// Fixed maximum for the single shared peer-I/O owner and its attach queue.
pub const MAX_PEER_SESSIONS: usize = 1024;

/// One bounded, round-robin I/O owner for all connected peers.
pub struct PeerIoWorker {
    pending: Producer<Box<PeerSession>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl PeerIoWorker {
    /// Start the single owner before admitting any sessions.
    pub fn start(epoch: Instant) -> std::io::Result<Self> {
        let (pending, mut incoming) = RingBuffer::<Box<PeerSession>>::new(MAX_PEER_SESSIONS);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("rpt-peer-io".into())
            .spawn(move || {
                let mut sessions: Vec<Box<PeerSession>> = Vec::with_capacity(MAX_PEER_SESSIONS);
                loop {
                    while let Ok(session) = incoming.pop() {
                        sessions.push(session);
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
            })?;
        Ok(Self {
            pending,
            stop,
            thread: Some(thread),
        })
    }

    /// Transfer a prepared peer without blocking or growing the bounded queue.
    pub fn attach(&mut self, session: PeerSession) -> Result<(), Box<PeerSession>> {
        let session = Box::new(session);
        if self.thread.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(session);
        }
        self.pending
            .push(session)
            .map_err(|rtrb::PushError::Full(session)| session)?;
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
        Ok(())
    }

    /// Stop the shared owner and release all peer handles on that owner thread.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
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
        let egress = Egress::new(io.rate(), 960).map_err(|_| Error::Allocation)?;
        let (inbound, _input) =
            InboundRing::open(io.rate(), InboundPolicy::Peer).map_err(|_| Error::Allocation)?;
        io.send_text(c"!NEWKEY1!")?;
        let (commands, incoming) = RingBuffer::new(64);
        let (events, outgoing) = RingBuffer::new(64);
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let owner_thread = Arc::new(std::sync::Mutex::new(None));
        let control = PeerControl {
            commands,
            events: outgoing,
            stop: stop.clone(),
            observer: inbound.observer(),
            #[cfg(test)]
            owner_thread: Arc::clone(&owner_thread),
        };
        Ok((
            Self {
                io,
                inbound,
                peer,
                local: local.into(),
                remote: remote.into(),
                dtmf_sequence: 0,
                commands: incoming,
                events,
                stop,
                outbound,
                egress,
                native: [0.0; 960],
                next_audio: 0,
                sent_audio: false,
                edge: 0,
                query_epoch: None,
                replied_newkey: false,
                radio_control: RadioControlState::Negotiating(None),
                #[cfg(test)]
                owner_thread,
            },
            control,
        ))
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
                if self.peer.digit(digit, now_ms) {
                    self.event(Event::Digit(digit))?;
                }
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
                    self.outbound = outbound;
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
        if self.io.ready()? {
            let mut event = None;
            let mut radio_keyed = None;
            let radio_control_allowed = matches!(
                self.radio_control,
                RadioControlState::Allowed | RadioControlState::LegacyAllowed
            );
            let inbound = &mut self.inbound;
            self.io.read(|input| match input {
                PeerInput::Text(bytes) => event = Some(Event::Text(bytes.into())),
                PeerInput::Digit(digit) => event = Some(Event::Digit(digit)),
                PeerInput::Audio(samples) => {
                    let _ = inbound.write(samples);
                }
                PeerInput::RadioKey => radio_keyed = Some(true),
                PeerInput::RadioUnkey => radio_keyed = Some(false),
            })?;
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
        }
        if self.peer.expire_digit(now_ms) {
            self.event(Event::Digit('#'))?;
        }
        if now_ms >= self.next_audio {
            self.next_audio = now_ms.saturating_add(20);
            let shortfall = self.outbound.read(&mut self.native);
            let real = self.native.len() - shortfall;
            if real != 0 {
                let audio = self
                    .egress
                    .process(&self.native[..real])
                    .map_err(|_| Error::Translation)?;
                if !audio.is_empty() {
                    self.io.write(audio)?;
                    self.sent_audio = true;
                }
            } else if self.sent_audio {
                self.io.write(&[])?;
                self.sent_audio = false;
            }
        }
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
