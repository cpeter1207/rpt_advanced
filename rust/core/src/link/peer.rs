//! Serialized peer control state; audio edges arrive through the owner boundary.
use super::{AdmissionError, decimal_identity, identity, valid_digit};

/// Per-peer keyed-source epochs and completed-digit deadlines.
pub struct Peer {
    direct: String,
    source: String,
    active: bool,
    epoch: u64,
    sent: Option<u64>,
    requester: String,
    responded: bool,
    next_query: u64,
    digit_deadline: Option<u64>,
}

impl Peer {
    /// Construct state for a direct decimal ASL identity.
    pub fn new(direct: &str) -> Result<Self, AdmissionError> {
        if !decimal_identity(direct) {
            return Err(AdmissionError::Identity);
        }
        Ok(Self {
            direct: direct.into(),
            source: direct.into(),
            active: false,
            epoch: 0,
            sent: None,
            requester: String::new(),
            responded: false,
            next_query: 0,
            digit_deadline: None,
        })
    }
    /// Advance receive activity; return a new query epoch at an edge or each second.
    pub fn activity(&mut self, active: bool, now_ms: u64) -> Option<u64> {
        if !active {
            self.active = false;
            self.sent = None;
            return None;
        }
        if self.active && now_ms < self.next_query {
            return None;
        }
        if !self.active {
            self.source.clone_from(&self.direct);
        }
        self.active = true;
        self.epoch = self.epoch.wrapping_add(1);
        self.sent = None;
        self.responded = false;
        self.next_query = now_ms.saturating_add(1000);
        Some(self.epoch)
    }
    /// Mark a still-active query sent by its transport owner.
    pub fn query(&mut self, epoch: u64, requester: &str) -> Option<String> {
        if !self.active || epoch != self.epoch || !identity(requester) {
            return None;
        }
        self.sent = Some(epoch);
        self.requester = requester.into();
        Some(format!("K? * {requester} 0 0"))
    }
    /// Accept the first valid downstream response for the current sent epoch.
    pub fn accept_key(&mut self, epoch: u64, destination: &str, source: &str, keyed: bool) -> bool {
        // Query records only the current epoch; every epoch change clears `sent`.
        if !self.active
            || self.sent != Some(epoch)
            || self.responded
            || !keyed
            || destination != self.requester
            || source == self.direct
            || !identity(source)
        {
            return false;
        }
        self.responded = true;
        self.source = source.into();
        true
    }
    /// Advisory source retained across the completed receive edge.
    pub fn selected_source(&self) -> &str {
        &self.source
    }
    /// Record a conventional completed digit; hash cancels automatic termination.
    pub fn digit(&mut self, digit: char, now_ms: u64) -> bool {
        if !valid_digit(digit) {
            return false;
        }
        self.digit_deadline = (digit != '#').then(|| now_ms.saturating_add(3000));
        true
    }
    /// Consume one due three-second terminator in the non-audio owner.
    pub fn expire_digit(&mut self, now_ms: u64) -> bool {
        if self
            .digit_deadline
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.digit_deadline = None;
            true
        } else {
            false
        }
    }
}

/// Consumer-owned receive qualification; the shared ring alone supplies concealment.
pub struct ReceiveState {
    rate: u64,
    epoch: u64,
    age_native_samples: u64,
    primed: bool,
}
impl ReceiveState {
    /// Establish source units; activity timing always advances at native 48 kHz.
    pub fn new(source_rate: u32) -> Result<Self, AdmissionError> {
        if source_rate == 0 || source_rate > 48000 {
            return Err(AdmissionError::Invalid);
        }
        Ok(Self {
            rate: u64::from(source_rate),
            epoch: 0,
            age_native_samples: 0,
            primed: false,
        })
    }
    /// Decide whether to consume this native block; an ended transport wins immediately.
    /// `epoch` is published only after PCM, and `available` counts input-rate samples.
    pub fn should_render(
        &mut self,
        epoch: u64,
        available: u64,
        ended: bool,
        native_frames: usize,
    ) -> bool {
        if epoch != self.epoch {
            self.epoch = epoch;
            self.age_native_samples = 0;
        } else {
            self.age_native_samples = self.age_native_samples.saturating_add(native_frames as u64);
        }
        let capacity = (self.rate * 300 / 1000).max(512);
        let source_frames = (native_frames as u64).saturating_mul(self.rate) / 48000;
        let remaining = capacity.saturating_sub(source_frames);
        let reserve = (self.rate * 60 / 1000).min(remaining);
        let target = (self.rate * 260 / 1000).min(remaining);
        self.primed |= epoch != 0 && available >= target;
        let fresh = self.age_native_samples.saturating_mul(self.rate) < reserve * 48000;
        let protected = if fresh { reserve } else { 0 };
        self.primed && !ended && (available > protected || fresh)
    }
}
