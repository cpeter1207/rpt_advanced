//! Monotonic alarm transitions, independent of telemetry admission and hardware I/O.

use super::{Observation, ReadingQuality, SiteError};
use crate::config::{AlarmCondition, AlarmSettings};

/// Selected, unrounded value supplied to one alarm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlarmValue {
    /// Engineering units after source normalization/calibration.
    Number(f64),
    /// Debounced logical input state.
    Digital(bool),
}

/// Policy event; only transition events may cause equipment actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmEvent {
    /// Newly unsafe condition.
    Trip,
    /// Healthy recovery, including authorized latch reset.
    Clear,
    /// Notification only; never replay trip equipment actions.
    Repeat,
}

/// Current alarm admission and recovery state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlarmSnapshot {
    /// Tripped or latched alarm state.
    pub active: bool,
    /// Repeat notifications have been silenced for this activation.
    pub acknowledged: bool,
    /// Latest valid condition is unsafe, healthy, or unknown.
    pub unsafe_condition: Option<bool>,
    /// Healthy clear persistence has completed, so reset is permitted.
    pub reset_ready: bool,
}

/// Monotonic trip/clear persistence and notification timing.
#[derive(Clone)]
pub struct AlarmState {
    settings: AlarmSettings,
    state: AlarmSnapshot,
    pending: Option<(bool, u64)>,
    repeat_at: Option<u64>,
}
impl AlarmState {
    /// Begin inactive; no source observation is fabricated.
    pub fn new(settings: AlarmSettings) -> Self {
        Self {
            settings,
            state: AlarmSnapshot {
                active: false,
                acknowledged: false,
                unsafe_condition: None,
                reset_ready: false,
            },
            pending: None,
            repeat_at: None,
        }
    }
    fn condition(&self, reading: &Observation<AlarmValue>) -> Option<bool> {
        let value = (reading.quality == ReadingQuality::Valid)
            .then_some(reading.value)
            .flatten();
        if self.settings.condition == AlarmCondition::Unavailable {
            return Some(value.is_none());
        }
        match (self.settings.condition, value?) {
            (AlarmCondition::Above, AlarmValue::Number(value)) if value.is_finite() => {
                Some(if self.state.active {
                    value > self.settings.clear_value?
                } else {
                    value >= self.settings.trip_value?
                })
            }
            (AlarmCondition::Below, AlarmValue::Number(value)) if value.is_finite() => {
                Some(if self.state.active {
                    value < self.settings.clear_value?
                } else {
                    value <= self.settings.trip_value?
                })
            }
            (AlarmCondition::Active, AlarmValue::Digital(value)) => Some(value),
            (AlarmCondition::Inactive, AlarmValue::Digital(value)) => Some(!value),
            _ => None,
        }
    }
    /// Evaluate one acquired state. Unknown breaks persistence but cannot clear an alarm.
    pub fn advance(&mut self, now_ms: u64, reading: &Observation<AlarmValue>) -> Vec<AlarmEvent> {
        let mut events = Vec::new();
        if !self.settings.enabled {
            return events;
        }
        self.state.unsafe_condition = self.condition(reading);
        self.state.reset_ready = false;
        match self.state.unsafe_condition {
            Some(unsafe_now) if unsafe_now != self.state.active => {
                let since = match self.pending {
                    Some((value, since)) if value == unsafe_now => since,
                    _ => now_ms,
                };
                self.pending = Some((unsafe_now, since));
                let delay = if unsafe_now {
                    self.settings.trip_delay_ms
                } else {
                    self.settings.clear_delay_ms
                };
                if now_ms.saturating_sub(since) >= delay {
                    if unsafe_now {
                        self.state.active = true;
                        self.state.acknowledged = false;
                        self.pending = None;
                        self.repeat_at = (self.settings.repeat_interval_ms > 0)
                            .then(|| now_ms.saturating_add(self.settings.repeat_interval_ms));
                        events.push(AlarmEvent::Trip);
                    } else if self.settings.latched {
                        self.state.reset_ready = true;
                    } else {
                        self.clear();
                        events.push(AlarmEvent::Clear);
                    }
                }
            }
            _ => self.pending = None,
        }
        if self.state.active
            && !self.state.acknowledged
            && self.repeat_at.is_some_and(|deadline| now_ms >= deadline)
        {
            self.repeat_at = Some(now_ms.saturating_add(self.settings.repeat_interval_ms));
            events.push(AlarmEvent::Repeat);
        }
        events
    }
    /// Silence only repeats; equipment actions and unsafe state remain intact.
    pub fn acknowledge(&mut self) {
        self.state.acknowledged = true;
    }
    /// Clear a healthy latched alarm after its recovery delay. Physical interlocks are untouched.
    pub fn reset(&mut self) -> Result<Option<AlarmEvent>, SiteError> {
        if !self.state.active {
            return Ok(None);
        }
        if !self.state.reset_ready {
            return Err(SiteError::Unsafe);
        }
        self.clear();
        Ok(Some(AlarmEvent::Clear))
    }
    fn clear(&mut self) {
        self.state.active = false;
        self.state.acknowledged = false;
        self.pending = None;
        self.repeat_at = None;
        self.state.reset_ready = false;
    }
    /// Inspect state without advancing timers or reading hardware.
    pub const fn snapshot(&self) -> AlarmSnapshot {
        self.state
    }
}

#[cfg(test)]
#[path = "alarm_tests.rs"]
mod tests;
