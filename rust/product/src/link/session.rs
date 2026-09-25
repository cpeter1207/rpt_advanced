//! Serial IAX reader, queued control delivery and native egress composition.
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
    thread::JoinHandle,
};

/// Work copied by control and executed only by the owning IAX reader.
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
/// Copied reader events for the serialized core control owner.
pub enum Event {
    /// Strictly parsed protocol text requiring topology/relay policy.
    Text(Vec<u8>),
    /// Completed digit, including the existing three-second synthesized terminator.
    Digit(char),
    /// Replacement ingress is installed; control may acknowledge old-generation detachment.
    Redirected(InboundObserver),
}
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
    }
}
impl Drop for PeerControl {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One prepared reader owner. It may be stepped by a host worker or started once.
pub struct PeerSession {
    io: PeerIo,
    inbound: InboundProducer,
    peer: Peer,
    local: String,
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
}
impl PeerSession {
    /// Complete reader preparation before the core's final attach gate.
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
        let control = PeerControl {
            commands,
            events: outgoing,
            stop: stop.clone(),
            observer: inbound.observer(),
        };
        Ok((
            Self {
                io,
                inbound,
                peer,
                local: local.into(),
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
            },
            control,
        ))
    }
    fn event(&mut self, event: Event) -> Result<(), Error> {
        self.events.push(event).map_err(|_| Error::Write)
    }
    fn text(&mut self, bytes: Vec<u8>) -> Result<(), Error> {
        let Some(protocol) = Protocol::parse(&bytes) else {
            return Ok(());
        };
        match protocol {
            Protocol::NewKey => (),
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
            _ => self.event(Event::Text(bytes))?,
        }
        Ok(())
    }
    // An audio owner may unkey after the reader captures its activity edge.
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
    /// Execute one bounded reader iteration; any fatal error publishes EOF immediately.
    pub fn step(&mut self, now_ms: u64) -> Result<(), Error> {
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
                Command::Digit(digit) => self.io.send_digit(digit)?,
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
            let inbound = &mut self.inbound;
            self.io.read(|input| match input {
                PeerInput::Text(bytes) => event = Some(Event::Text(bytes.into())),
                PeerInput::Digit(digit) => event = Some(Event::Digit(digit)),
                PeerInput::Audio(samples) => {
                    let _ = inbound.write(samples);
                }
            })?;
            match event {
                Some(Event::Text(bytes)) => self.text(bytes)?,
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
    /// Move this exclusive owner onto one reader thread; no channel handle is shared.
    pub fn start(mut self) -> std::io::Result<PeerReader> {
        let stop = self.stop.clone();
        let thread = std::thread::Builder::new()
            .name("rpt-iax-reader".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                while self
                    .step(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
                    .is_ok()
                {}
            })?;
        Ok(PeerReader {
            stop,
            thread: Some(thread),
        })
    }
}
/// Reader lifetime guard: stop and join before releasing channel/ring ownership.
pub struct PeerReader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl PeerReader {
    /// Observe completion on control; finished readers must still be joined.
    pub fn ended(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    /// Stop and join all IAX operations before core reclaims the direct identity.
    pub fn join(mut self) {
        self.stop_and_join();
    }
    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for PeerReader {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[cfg(test)]
pub(crate) mod tests;
