//! Calibrated readings preserve physical observation time and source health.

#[cfg(test)]
#[path = "measurement_tests.rs"]
mod tests;
use super::{Observation, ReadingQuality};
use crate::config::SensorSettings;

/// One valid, unrounded engineering value and its actual acquisition time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedValue {
    /// Calibrated engineering-unit value.
    pub value: f64,
    /// Monotonic acquisition timestamp.
    pub observed_at_ms: u64,
}

/// Read-only measurement state; stale/faulted last-good values are diagnostic only.
#[derive(Clone, Debug, PartialEq)]
pub struct SensorSnapshot {
    /// Last-good value/time with current source health.
    pub current: Observation<f64>,
    /// Elapsed-time filtered value, with the same health as current.
    pub smoothed: Option<f64>,
    /// Minimum valid unsmoothed value since reset.
    pub minimum: Option<TimedValue>,
    /// Maximum valid unsmoothed value since reset.
    pub maximum: Option<TimedValue>,
}

/// Control-owned calibration, freshness, smoothing and extrema; performs no I/O.
pub struct SensorState {
    settings: SensorSettings,
    state: SensorSnapshot,
}

impl SensorState {
    /// Begin unavailable, without inventing a numeric value or timestamp.
    pub fn new(settings: SensorSettings) -> Self {
        Self {
            settings,
            state: SensorSnapshot {
                current: Observation {
                    value: None,
                    observed_at_ms: None,
                    quality: ReadingQuality::Unavailable,
                },
                smoothed: None,
                minimum: None,
                maximum: None,
            },
        }
    }

    /// Retain measurement history only when source identity and calibration agree.
    /// Display precision and polling changes do not rewrite measured values.
    pub fn reconfigure(&mut self, settings: SensorSettings) {
        if self.settings.endpoint != settings.endpoint
            || self.settings.calibration != settings.calibration
        {
            *self = Self::new(settings);
        } else {
            self.settings = settings;
        }
    }

    /// Clear extrema without fabricating a new acquisition or changing current data.
    pub fn reset_extrema(&mut self) {
        self.state.minimum = None;
        self.state.maximum = None;
    }

    /// Accept normalized adapter units once. Failed, nonfinite, future or older
    /// observations cannot replace or refresh last-good data. Repeated cached
    /// observations leave smoothing and extrema untouched.
    pub fn observe(&mut self, reading: Observation<f64>, now_ms: u64) {
        if reading.quality != ReadingQuality::Valid {
            self.state.current.quality = reading.quality;
            return;
        }
        let valid = reading
            .value
            .filter(|value| value.is_finite())
            .and_then(|value| self.settings.calibration.apply(value))
            .zip(reading.observed_at_ms)
            .filter(|(_, time)| {
                *time <= now_ms
                    && self
                        .state
                        .current
                        .observed_at_ms
                        .is_none_or(|last| *time >= last)
            });
        let Some((value, time)) = valid else {
            self.state.current.quality = ReadingQuality::Fault;
            return;
        };
        if self.state.current.observed_at_ms == Some(time) {
            return;
        }
        self.state.smoothed = Some(
            match (self.state.smoothed, self.state.current.observed_at_ms) {
                (Some(previous), Some(last)) if self.settings.smoothing_ms > 0 => {
                    let weight =
                        (-((time - last) as f64) / self.settings.smoothing_ms as f64).exp();
                    weight * previous + (1.0 - weight) * value
                }
                _ => value,
            },
        );
        let observed = TimedValue {
            value,
            observed_at_ms: time,
        };
        if self
            .state
            .minimum
            .is_none_or(|minimum| value < minimum.value)
        {
            self.state.minimum = Some(observed);
        }
        if self
            .state
            .maximum
            .is_none_or(|maximum| value > maximum.value)
        {
            self.state.maximum = Some(observed);
        }
        self.state.current = Observation {
            value: Some(value),
            observed_at_ms: Some(time),
            quality: ReadingQuality::Valid,
        };
    }

    /// Snapshot health using acquisition age, never the last query/read attempt.
    pub fn snapshot(&self, now_ms: u64) -> SensorSnapshot {
        let mut snapshot = self.state.clone();
        if snapshot.current.quality == ReadingQuality::Valid
            && snapshot
                .current
                .observed_at_ms
                .is_some_and(|time| now_ms.saturating_sub(time) >= self.settings.stale_after_ms)
        {
            snapshot.current.quality = ReadingQuality::Stale;
        }
        snapshot
    }
}
