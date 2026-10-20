//! Control-plane site observations shared by message bindings and sensor policy.

mod alarm;
mod controller;
mod fan;
mod measurement;
mod output;
pub use alarm::{AlarmEvent, AlarmSnapshot, AlarmState, AlarmValue};
pub use controller::{SiteCommand, SiteController, SiteEffect, SiteObservations, SiteSnapshot};
pub use fan::{FanOutput, FanSnapshot, FanState};
pub use measurement::{SensorSnapshot, SensorState, TimedValue};
pub use output::{OutputRequest, OutputSnapshot, OutputState};

/// A policy command rejected before any device work is submitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteError {
    /// No configured instance has this name.
    UnknownName,
    /// The configured target cannot currently be controlled.
    Unavailable,
    /// Recovery or a command would override an unsafe condition.
    Unsafe,
    /// The requested value is outside the operation's valid domain.
    InvalidValue,
}

/// Health of one observed value, independent of its numeric or digital representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingQuality {
    /// A current, successfully acquired value.
    Valid,
    /// A previously acquired value exceeded its configured freshness interval.
    Stale,
    /// No observation is available.
    Unavailable,
    /// The source reported a fault or invalid value.
    Fault,
}

/// One source observation; absent values/times are never invented as zero.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation<T> {
    /// Acquired value, when present.
    pub value: Option<T>,
    /// Monotonic observation time, not the time a status was requested.
    pub observed_at_ms: Option<u64>,
    /// Acquisition/freshness status at snapshot time.
    pub quality: ReadingQuality,
}
