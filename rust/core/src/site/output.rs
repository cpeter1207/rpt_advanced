//! Logical output pulses restore prior demand, never a hardcoded baseline.

use super::SiteError;
use crate::config::OutputSettings;

/// Authorized logical output command, without direct device access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputRequest {
    /// Request logical on, canceling a pulse.
    On,
    /// Request logical off, canceling a pulse.
    Off,
    /// Request on until the configured deadline, then restore prior demand.
    Pulse,
}

/// Requested policy state, not hardware submission or readback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSnapshot {
    /// Current logical demand.
    pub requested_on: bool,
    /// No usable output control is currently available.
    pub fault: bool,
    /// Original/extended pulse deadline, when a pulse is pending.
    pub pulse_deadline_ms: Option<u64>,
}

/// Prior-state pulse timer owned by station control.
#[derive(Clone)]
pub struct OutputState {
    settings: OutputSettings,
    state: OutputSnapshot,
    prior_on: bool,
}
impl OutputState {
    /// Start with configured demand, but reject commands until availability is observed.
    pub fn new(settings: OutputSettings) -> Self {
        Self {
            state: OutputSnapshot {
                requested_on: settings.startup_on,
                fault: true,
                pulse_deadline_ms: None,
            },
            settings,
            prior_on: false,
        }
    }
    /// Preserve a live pulse's prior state and deadline on the same endpoint.
    /// Changed endpoints must already have completed safe ownership handoff.
    pub fn reconfigure(&mut self, settings: OutputSettings) {
        if self.settings.endpoint != settings.endpoint
            || self.settings.active_high != settings.active_high
        {
            *self = Self::new(settings);
        } else {
            self.settings = settings;
        }
    }
    /// Apply a logical command; repeated pulses extend from this command's time.
    pub fn command(&mut self, request: OutputRequest, now_ms: u64) -> Result<(), SiteError> {
        if !self.settings.enabled || self.state.fault {
            return Err(SiteError::Unavailable);
        }
        match request {
            OutputRequest::Pulse => {
                if self.state.pulse_deadline_ms.is_none() {
                    self.prior_on = self.state.requested_on;
                }
                self.state.requested_on = true;
                self.state.pulse_deadline_ms = Some(now_ms.saturating_add(self.settings.pulse_ms));
            }
            OutputRequest::On | OutputRequest::Off => {
                self.state.pulse_deadline_ms = None;
                self.state.requested_on = request == OutputRequest::On;
            }
        }
        Ok(())
    }
    /// Fault/safe demand wins immediately and permanently cancels the old pulse.
    pub fn advance(&mut self, now_ms: u64, available: bool) {
        self.state.fault = !available || !self.settings.enabled;
        if self.state.fault {
            self.state.requested_on = self.settings.safe_on;
            self.state.pulse_deadline_ms = None;
        } else if self
            .state
            .pulse_deadline_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.state.requested_on = self.prior_on;
            self.state.pulse_deadline_ms = None;
        }
    }
    /// Inspect desired state; no hardware read is performed or implied.
    pub const fn snapshot(&self) -> OutputSnapshot {
        self.state
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
