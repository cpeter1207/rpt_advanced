//! Alarm and fan configuration resolved against named site instances.
use super::finite;
use crate::config::resources::PortKey;
use crate::site::OutputRequest;

/// Bounded site action syntax. References are resolved at reload, never interpreted as scripts.
#[derive(Clone, Debug, PartialEq)]
pub enum SiteAction {
    /// Invoke a validated existing controller macro once.
    Macro(String),
    /// Change a named logical output.
    Output {
        /// Configured output name.
        name: String,
        /// Logical operation after authorization.
        request: OutputRequest,
    },
    /// Set additional fan demand, or automatic operation with None.
    Fan {
        /// Configured fan name.
        name: String,
        /// Additional normalized cooling demand, or automatic operation with None.
        duty: Option<f64>,
    },
    /// Assert/release only this action source's protective transmit inhibit.
    Inhibit(bool),
}

fn name_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value
            .chars()
            .any(|character| character.is_whitespace() || "[]:".contains(character))
}

/// Parse one site action element without conflating punctuation inside a JSON array member.
pub(crate) fn parse_action(value: &str) -> Option<SiteAction> {
    let parts: Vec<_> = value.split(':').collect();
    match parts.as_slice() {
        ["macro", name] if name_valid(name) => Some(SiteAction::Macro((*name).into())),
        ["output", name, command] if name_valid(name) => Some(SiteAction::Output {
            name: (*name).into(),
            request: match *command {
                "on" => OutputRequest::On,
                "off" => OutputRequest::Off,
                "pulse" => OutputRequest::Pulse,
                _ => return None,
            },
        }),
        ["fan", name, value] if name_valid(name) => Some(SiteAction::Fan {
            name: (*name).into(),
            duty: if *value == "auto" {
                None
            } else {
                Some(finite(value).filter(|value| (0.0..=1.0).contains(value))?)
            },
        }),
        ["inhibit", "on"] => Some(SiteAction::Inhibit(true)),
        ["inhibit", "off"] => Some(SiteAction::Inhibit(false)),
        _ => None,
    }
}

/// Evaluated condition; analog thresholds have explicit recovery hysteresis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmCondition {
    /// Trip at/above the high threshold; recover at/below the clear threshold.
    Above,
    /// Trip at/below the low threshold; recover at/above the clear threshold.
    Below,
    /// A logical digital input is active.
    Active,
    /// A logical digital input is inactive.
    Inactive,
    /// A source is not currently valid.
    Unavailable,
}

/// One control-plane alarm policy. Actions are validated before generation publication.
#[derive(Clone, Debug, PartialEq)]
pub struct AlarmSettings {
    /// Stable policy name.
    pub name: String,
    /// Whether transitions are evaluated.
    pub enabled: bool,
    /// Named sensor or input, never an endpoint opened by the alarm.
    pub source: String,
    /// Trip/recovery comparison.
    pub condition: AlarmCondition,
    /// Analog trip threshold; absent for digital/health conditions.
    pub trip_value: Option<f64>,
    /// Analog recovery threshold.
    pub clear_value: Option<f64>,
    /// Compare the smoothed sensor value when true; never a rounded display value.
    pub smoothed: bool,
    /// Continuous unsafe duration needed to trip.
    pub trip_delay_ms: u64,
    /// Continuous healthy duration needed to clear/be reset.
    pub clear_delay_ms: u64,
    /// Recovery also requires explicit reset.
    pub latched: bool,
    /// Repeat notification period, zero disables repeats.
    pub repeat_interval_ms: u64,
    /// Ordered equipment/controller action references on trip.
    pub on_trip: Vec<SiteAction>,
    /// Ordered equipment/controller action references on recovery.
    pub on_clear: Vec<SiteAction>,
    /// Optional named message recipe on trip.
    pub trip_message: String,
    /// Optional named message recipe on recovery.
    pub clear_message: String,
    /// Optional named message recipe for repeats only.
    pub repeat_message: String,
}

/// Sources contributing to cooling demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanMode {
    /// Transmitter activity and run-on.
    Ptt,
    /// Temperature policy.
    Temperature,
    /// Maximum of transmitter and temperature demand.
    Combined,
}

/// Cooling policy referencing an output owned by the site service.
#[derive(Clone, Debug, PartialEq)]
pub struct FanSettings {
    /// Stable policy name.
    pub name: String,
    /// Whether this policy claims the named output.
    pub enabled: bool,
    /// Named output; one fan may claim it.
    pub output: String,
    /// Active demand sources.
    pub mode: FanMode,
    /// Local transmitter identities contributing to PTT demand.
    pub ptt_nodes: Vec<PortKey>,
    /// Run-on after the last selected PTT releases.
    pub run_on_ms: u64,
    /// Named temperature sensor, when selected.
    pub temperature_sensor: String,
    /// On threshold for switched cooling.
    pub on_temperature: Option<f64>,
    /// Off threshold for switched cooling.
    pub off_temperature: Option<f64>,
    /// Ordered temperature/duty points, requiring a real PWM-capable endpoint.
    pub temperature_curve: Vec<(f64, f64)>,
    /// Optional named tachometer sensor.
    pub rpm_sensor: String,
    /// Minimum healthy running RPM when tachometry is selected.
    pub minimum_rpm: Option<f64>,
    /// Grace period after cooling starts before RPM health is evaluated.
    pub spinup_ms: u64,
}

/// Accept a finite monotonic temperature/duty response with at least two distinct points.
pub(crate) fn curve_valid(points: &[(f64, f64)]) -> bool {
    points.len() >= 2
        && points
            .iter()
            .all(|(t, d)| t.is_finite() && (0.0..=1.0).contains(d))
        && points
            .windows(2)
            .all(|p| p[0].0 < p[1].0 && p[0].1 <= p[1].1 && (p[1].0 - p[0].0).is_finite())
}
