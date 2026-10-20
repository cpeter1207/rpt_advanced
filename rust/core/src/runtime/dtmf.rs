//! Bounded radio-to-control digit handoff and existing linking command policy.

use crate::{
    audio::{DtmfDetector, DtmfError},
    command::{Command, CommandCollector, DtmfCommandMap, LinkAction},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// One copied radio event; no borrowed callback memory crosses to control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigitEvent {
    /// Hardware-timestamped actual digit or NUL timeout tick.
    Digit {
        /// Decoded symbol or timeout marker.
        digit: char,
        /// Captured monotonic timestamp, never dispatcher execution time.
        now_ms: u64,
    },
    /// Actual receive falling edge, distinct from a literal hash digit.
    Unkey {
        /// Captured monotonic timestamp.
        now_ms: u64,
    },
    /// A queue/submission loss invalidates the partial command before further digits.
    Lost,
}

struct DtmfPublisher {
    writer: Producer<DigitEvent>,
    drops: Arc<AtomicU64>,
    last_ms: u64,
    timeout: bool,
    receiving: bool,
}
impl DtmfPublisher {
    fn new(generation: u64) -> (Self, DtmfDispatcher) {
        let (writer, reader) = RingBuffer::new(256);
        let drops = Arc::new(AtomicU64::new(0));
        (
            Self {
                writer,
                drops: drops.clone(),
                last_ms: 0,
                timeout: false,
                receiving: false,
            },
            DtmfDispatcher {
                reader,
                drops,
                reported: 0,
                generation,
            },
        )
    }
    fn queue(&mut self, digit: char, now_ms: u64) {
        self.publish(DigitEvent::Digit { digit, now_ms });
    }
    fn publish(&mut self, event: DigitEvent) {
        if self.writer.push(event).is_err() {
            self.drops.fetch_add(1, Ordering::Release);
        }
    }
    fn decoded(&mut self, digit: char, now_ms: u64) {
        self.queue(digit, now_ms);
        self.last_ms = now_ms;
        self.timeout = true;
    }
    fn finish_frame(&mut self, receiving: bool, now_ms: u64, emitted: bool) {
        if !emitted && self.timeout && now_ms.saturating_sub(self.last_ms) >= 3000 {
            self.queue('\0', now_ms);
            self.timeout = false;
        }
        if self.receiving && !receiving {
            self.publish(DigitEvent::Unkey { now_ms });
            self.timeout = false;
        }
        self.receiving = receiving;
    }
}

/// Receive-private detector and bounded SPSC producer. Construct before audio starts.
pub struct DtmfWorker {
    detector: DtmfDetector,
    publisher: DtmfPublisher,
}
impl DtmfWorker {
    /// Allocate the fixed 256-event queue and return its separate control consumer.
    pub fn new(generation: u64, muting: bool) -> Result<(Self, DtmfDispatcher), DtmfError> {
        let (publisher, dispatcher) = DtmfPublisher::new(generation);
        Ok((
            Self {
                detector: DtmfDetector::new(muting)?,
                publisher,
            },
            dispatcher,
        ))
    }
    /// Decode every digit completion and publish without allocation, submission, or locks.
    pub fn process(&mut self, receiving: bool, audio: &mut [f32], now_ms: u64) {
        let mut emitted = false;
        self.detector.process(receiving, audio, |digit| {
            self.publisher.decoded(digit, now_ms);
            emitted = true;
        });
        self.publisher.finish_frame(receiving, now_ms, emitted);
    }

    /// Current receive-side muting state for the delayed PCM consumer.
    #[must_use]
    pub fn suppressing(&self) -> bool {
        self.detector.suppressing()
    }
}

/// Non-audio consumer of one generation's fixed SPSC endpoints. It invalidates queued
/// prefixes after overflow; no other generation can publish into this queue.
pub struct DtmfDispatcher {
    reader: Consumer<DigitEvent>,
    drops: Arc<AtomicU64>,
    reported: u64,
    generation: u64,
}
impl DtmfDispatcher {
    /// Discard the observed prefix after control admission was suppressed or lost.
    pub fn discard(&mut self) {
        let queued = self.reader.slots();
        for _ in 0..queued {
            let _ = self.reader.pop();
        }
        self.reported = self.drops.load(Ordering::Acquire);
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    /// Take one current-generation event; overflow discards the observed queued prefix.
    pub fn next(&mut self, current_generation: u64) -> Option<DigitEvent> {
        let dropped = self.drops.load(Ordering::Acquire);
        if self.generation != current_generation {
            let queued = self.reader.slots();
            for _ in 0..queued {
                let _ = self.reader.pop();
            }
            self.reported = dropped;
            return None;
        }
        if dropped != self.reported {
            let queued = self.reader.slots();
            for _ in 0..queued {
                let _ = self.reader.pop();
            }
            self.reported = dropped;
            return Some(DigitEvent::Lost);
        }
        self.reader.pop().ok()
    }
    /// Cumulative radio-event drops; read outside audio for diagnostics.
    pub fn dropped(&self) -> u64 {
        self.drops.load(Ordering::Acquire)
    }
    /// Current bounded queue occupancy.
    pub fn queued(&self) -> usize {
        self.reader.slots()
    }
}

/// Existing linking operation, optionally forwarding one digit to a selected direct peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigitOperation {
    /// Typed linking command and resolved destination (`0` means the last named peer).
    pub command: Command,
    /// Remote-command symbol, absent for ordinary link operations.
    pub digit: Option<char>,
    /// Exact originating peer; absent for the existing local/CLI path.
    pub reply: Option<crate::link::PeerReply>,
    /// Claimed source label for relayed commands; never an authorization identity.
    pub origin: Option<String>,
    pub(crate) admin_authorized: bool,
}

/// Bounded local keypad capture; only actual receiver unkey completes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadTestResult {
    /// First 127 ASCII keypad symbols.
    pub digits: String,
    /// Additional symbols were discarded.
    pub overflowed: bool,
}

/// Control-side interpretation; media and directory work stay outside collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DigitOutcome {
    /// Complete operation or one digit for a selected remote.
    Operation(DigitOperation),
    /// Incomplete destination to select only if the directory recognizes it exactly.
    RemoteCandidate(DigitOperation),
    /// Local keypad report after actual unkey.
    PadTest(PadTestResult),
    /// Idle addressed peer digit eligible for configured local-TX regeneration.
    Regenerate(char),
}

impl DigitOperation {
    /// Construct a non-administrative typed operation for trusted control interfaces.
    pub fn new(command: Command, digit: Option<char>) -> Self {
        Self {
            command,
            digit,
            reply: None,
            origin: None,
            admin_authorized: false,
        }
    }
}

/// Serialized command collection; authorization/peer availability stays with link control.
pub struct DtmfCommands {
    map: DtmfCommandMap,
    collector: CommandCollector,
    last_node: String,
    remote: String,
    peer_source: bool,
    pad: Option<PadTestResult>,
    digit_deadline: Option<u64>,
    admin_unlock_hash: String,
    admin_lock_hash: String,
    admin_timeout_ms: u64,
    admin_until_ms: u64,
}
impl DtmfCommands {
    /// Create independent collection and authorization state with the same resolved policy.
    pub(super) fn for_source(&self) -> Self {
        let mut commands = Self::new(self.map.clone());
        commands.peer_source = true;
        commands.configure_admin(
            &self.admin_unlock_hash,
            &self.admin_lock_hash,
            self.admin_timeout_ms,
        );
        commands
    }
    /// Start with the resolved current command map.
    pub fn new(map: DtmfCommandMap) -> Self {
        let collector = map.collector();
        Self {
            map,
            collector,
            last_node: String::new(),
            remote: String::new(),
            peer_source: false,
            pad: None,
            digit_deadline: None,
            admin_unlock_hash: String::new(),
            admin_lock_hash: String::new(),
            admin_timeout_ms: 300_000,
            admin_until_ms: 0,
        }
    }
    /// Install hashed DTMF credentials on control; administration stays unavailable unless both
    /// credential digests are valid Argon2id PHC strings.
    pub fn configure_admin(&mut self, unlock_hash: &str, lock_hash: &str, timeout_ms: u64) {
        self.admin_unlock_hash.clear();
        self.admin_lock_hash.clear();
        if valid_hash(unlock_hash) && valid_hash(lock_hash) && timeout_ms > 0 {
            self.admin_unlock_hash.push_str(unlock_hash);
            self.admin_lock_hash.push_str(lock_hash);
            self.admin_timeout_ms = timeout_ms;
        }
        self.admin_until_ms = 0;
    }
    /// Select an independently authorized direct peer after the link owner validates it.
    pub fn select_remote(&mut self, remote: &str) {
        self.remote = remote.into();
        self.collector = self.map.collector();
    }
    /// Clear selection when its peer disconnects or a scheduled route is withdrawn.
    pub fn disconnect(&mut self, remote: &str) {
        if self.remote == remote {
            self.remote.clear();
        }
    }
    /// Interpret copied events on control. Call `Lost` also after control submission rejection.
    pub fn feed(&mut self, event: DigitEvent) -> Option<DigitOutcome> {
        self.digit_deadline = match event {
            DigitEvent::Digit { digit, now_ms } if "0123456789ABCD*".contains(digit) => {
                Some(now_ms.saturating_add(3000))
            }
            _ => None,
        };
        if event == DigitEvent::Lost {
            self.collector = self.map.collector();
            self.pad = None;
            return None;
        }
        if let Some(pad) = self.pad.as_mut() {
            match event {
                DigitEvent::Unkey { .. } => return self.pad.take().map(DigitOutcome::PadTest),
                DigitEvent::Digit { digit, .. } if "0123456789ABCD*#".contains(digit) => {
                    if pad.digits.len() < 127 {
                        pad.digits.push(digit);
                    } else {
                        pad.overflowed = true;
                    }
                }
                _ => {}
            }
            return None;
        }
        // Ordinary commands still terminate on unkey; selected remote mode does not.
        let (digit, now_ms) = match event {
            DigitEvent::Digit { digit, now_ms } => (digit, now_ms),
            DigitEvent::Unkey { now_ms } if self.remote.is_empty() => ('#', now_ms),
            _ => return None,
        };
        if !self.remote.is_empty() {
            if digit == '#' {
                self.remote.clear();
                return None;
            }
            return "0123456789ABCD*".contains(digit).then(|| {
                DigitOutcome::Operation(DigitOperation {
                    command: Command {
                        action: LinkAction::Command,
                        node: self.remote.clone(),
                    },
                    digit: Some(digit),
                    reply: None,
                    origin: None,
                    admin_authorized: false,
                })
            });
        }
        if now_ms >= self.admin_until_ms {
            self.admin_until_ms = 0;
        }
        if self.peer_source
            && !self.collector.active()
            && digit != '*'
            && "0123456789ABCD#".contains(digit)
            && matches!(event, DigitEvent::Digit { .. })
        {
            return Some(DigitOutcome::Regenerate(digit));
        }
        let Some(text) = self.collector.feed(digit, now_ms) else {
            return self
                .collector
                .remote_candidate()
                .filter(|node| node.len() < 64)
                .map(|node| {
                    DigitOutcome::RemoteCandidate(DigitOperation::new(
                        Command {
                            action: LinkAction::Command,
                            node,
                        },
                        None,
                    ))
                });
        };
        let mut command = self.map.parse(&text)?;
        if matches!(command.action, LinkAction::Message | LinkAction::SiteStatus) {
            return (!command.node.is_empty()
                && crate::config::message_selector_valid(&command.node))
            .then(|| DigitOutcome::Operation(DigitOperation::new(command, None)));
        }
        let mut admin_authorized = false;
        if command.node.len() >= 64 {
            return None;
        }
        match command.action {
            LinkAction::PadTest => {
                if !self.peer_source {
                    self.pad = Some(PadTestResult {
                        digits: String::with_capacity(127),
                        overflowed: false,
                    });
                }
                return None;
            }
            LinkAction::AdminUnlock => {
                self.admin_until_ms = if self.credentials_configured()
                    && verify_hash(&self.admin_unlock_hash, &command.node)
                {
                    now_ms.saturating_add(self.admin_timeout_ms)
                } else {
                    0
                };
                return None;
            }
            LinkAction::AdminLock => {
                if self.credentials_configured()
                    && verify_hash(&self.admin_lock_hash, &command.node)
                {
                    self.admin_until_ms = 0;
                }
                return None;
            }
            LinkAction::ParrotEnable | LinkAction::ParrotDisable => {
                if self.admin_until_ms == 0 {
                    return None;
                }
                admin_authorized = true;
            }
            _ => {}
        }
        if command.node == "0" {
            if self.last_node.is_empty() {
                return None;
            }
            command.node = self.last_node.clone();
        }
        if !command.node.is_empty() {
            self.last_node.clone_from(&command.node);
        }
        Some(DigitOutcome::Operation(DigitOperation {
            command,
            digit: None,
            reply: None,
            origin: None,
            admin_authorized,
        }))
    }

    /// Preserve commands, selected/remembered destinations, and live administrative grants.
    pub(super) fn retains_state(&self, now_ms: u64) -> bool {
        self.collector.active()
            || !self.remote.is_empty()
            || !self.last_node.is_empty()
            || self.pad.is_some()
            || now_ms < self.admin_until_ms
    }
    /// Expire only this origin's pending command, without ending remote selection.
    pub(super) fn expire(&mut self, now_ms: u64) -> Option<DigitOutcome> {
        if self
            .digit_deadline
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.feed(DigitEvent::Digit {
                digit: '\0',
                now_ms,
            })
        } else {
            None
        }
    }

    /// Consume the one DTMF authorization issued for a parrot action before link control applies it.
    pub(crate) fn consume_parrot_authorization(&mut self, authorized: bool, now_ms: u64) -> bool {
        let authorized = authorized && self.admin_until_ms != 0 && now_ms < self.admin_until_ms;
        if !authorized && now_ms >= self.admin_until_ms {
            self.admin_until_ms = 0;
        }
        authorized
    }

    /// Reset the inactivity deadline only after link and parrot-control owners accept the action.
    pub(crate) fn confirm_parrot_action(&mut self, now_ms: u64) {
        if self.admin_until_ms != 0 && now_ms < self.admin_until_ms {
            self.admin_until_ms = now_ms.saturating_add(self.admin_timeout_ms);
        }
    }

    fn credentials_configured(&self) -> bool {
        !self.admin_unlock_hash.is_empty() && !self.admin_lock_hash.is_empty()
    }
}

fn valid_hash(value: &str) -> bool {
    use argon2::password_hash::PasswordHash;
    PasswordHash::new(value).is_ok_and(|hash| hash.algorithm.as_str() == "argon2id")
}

fn verify_hash(hash: &str, code: &str) -> bool {
    use argon2::{Argon2, PasswordVerifier, password_hash::PasswordHash};
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(code.as_bytes(), &parsed)
            .is_ok()
    })
}

#[cfg(test)]
#[path = "dtmf_tests.rs"]
mod tests;
