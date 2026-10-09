//! Bounded radio-to-control digit handoff and existing linking command policy.

use crate::{
    audio::{DtmfDetector, DtmfDigit},
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
    /// Hardware-timestamped digit, `#` unkey terminator, or NUL timeout tick.
    Digit {
        /// Decoded symbol or timeout marker.
        digit: char,
        /// Captured monotonic timestamp, never dispatcher execution time.
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
        if self
            .writer
            .push(DigitEvent::Digit { digit, now_ms })
            .is_err()
        {
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
        if self.receiving && !receiving && self.timeout {
            self.queue('#', now_ms);
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
    pub fn new(generation: u64, muting: bool) -> (Self, DtmfDispatcher) {
        let (publisher, dispatcher) = DtmfPublisher::new(generation);
        (
            Self {
                detector: DtmfDetector::new(muting),
                publisher,
            },
            dispatcher,
        )
    }
    /// Decode every digit completion and publish without allocation, submission, or locks.
    pub fn process(&mut self, receiving: bool, audio: &mut [f32], now_ms: u64) {
        let mut emitted = false;
        self.detector.process(receiving, audio, |digit| {
            self.publisher.decoded(character(digit), now_ms);
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

fn character(digit: DtmfDigit) -> char {
    match digit {
        DtmfDigit::One => '1',
        DtmfDigit::Two => '2',
        DtmfDigit::Three => '3',
        DtmfDigit::Four => '4',
        DtmfDigit::Five => '5',
        DtmfDigit::Six => '6',
        DtmfDigit::Seven => '7',
        DtmfDigit::Eight => '8',
        DtmfDigit::Nine => '9',
        DtmfDigit::Zero => '0',
        DtmfDigit::Star => '*',
        DtmfDigit::Hash => '#',
        DtmfDigit::A => 'A',
        DtmfDigit::B => 'B',
        DtmfDigit::C => 'C',
        DtmfDigit::D => 'D',
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
    pub(crate) admin_authorized: bool,
}

impl DigitOperation {
    /// Construct a non-administrative typed operation for trusted control interfaces.
    pub fn new(command: Command, digit: Option<char>) -> Self {
        Self {
            command,
            digit,
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
    admin_unlock_hash: String,
    admin_lock_hash: String,
    admin_timeout_ms: u64,
    admin_until_ms: u64,
}
impl DtmfCommands {
    /// Start with the resolved current command map.
    pub fn new(map: DtmfCommandMap) -> Self {
        let collector = map.collector();
        Self {
            map,
            collector,
            last_node: String::new(),
            remote: String::new(),
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
    }
    /// Clear selection when its peer disconnects or a scheduled route is withdrawn.
    pub fn disconnect(&mut self, remote: &str) {
        if self.remote == remote {
            self.remote.clear();
        }
    }
    /// Interpret copied events on control. Call `Lost` also after control submission rejection.
    pub fn feed(&mut self, event: DigitEvent) -> Option<DigitOperation> {
        let DigitEvent::Digit { digit, now_ms } = event else {
            self.collector = self.map.collector();
            return None;
        };
        if !self.remote.is_empty() {
            if digit == '#' {
                self.remote.clear();
                return None;
            }
            return "0123456789ABCD*".contains(digit).then(|| DigitOperation {
                command: Command {
                    action: LinkAction::Command,
                    node: self.remote.clone(),
                },
                digit: Some(digit),
                admin_authorized: false,
            });
        }
        if now_ms >= self.admin_until_ms {
            self.admin_until_ms = 0;
        }
        let text = self.collector.feed(digit, now_ms)?;
        let mut command = self.map.parse(&text)?;
        let mut admin_authorized = false;
        if command.node.len() >= 64 {
            return None;
        }
        match command.action {
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
        Some(DigitOperation {
            command,
            digit: None,
            admin_authorized,
        })
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
