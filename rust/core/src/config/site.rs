//! Site definitions resolved before hardware acquisition.

use super::{
    ConfigDocument, ConfigError, ConfigWarning, NodeId, Resolution, Schema, parse, settings,
};
use std::collections::BTreeSet;

#[path = "site_policy.rs"]
mod policy;
pub use policy::{AlarmCondition, AlarmSettings, FanMode, FanSettings, SiteAction};

/// Configuration families sharing the site inheritance rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SiteKind {
    Sensor,
    Input,
    Output,
    Alarm,
    Fan,
}

impl SiteKind {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "sensor" => Some(Self::Sensor),
            "input" => Some(Self::Input),
            "output" => Some(Self::Output),
            "alarm" => Some(Self::Alarm),
            "fan" => Some(Self::Fan),
            _ => None,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Sensor => "sensor",
            Self::Input => "input",
            Self::Output => "output",
            Self::Alarm => "alarm",
            Self::Fan => "fan",
        }
    }
}

/// Selected hardware owner; there is no automatic backend fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SiteBackend {
    /// The radio's existing CM119 HID owner.
    Cm119,
    /// The radio's existing parallel-port owner.
    Parallel,
    /// A Linux GPIO character device, through libgpiod.
    Gpiod,
    /// A Linux hwmon device, through libsensors.
    Hwmon,
    /// A kernel IIO channel.
    Iio,
}

impl SiteBackend {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "cm119" => Some(Self::Cm119),
            "parallel" => Some(Self::Parallel),
            "gpiod" => Some(Self::Gpiod),
            "hwmon" => Some(Self::Hwmon),
            "iio" => Some(Self::Iio),
            _ => None,
        }
    }
}

/// Stable configured endpoint. Runtime capability resolution must also reject
/// aliases of an already-owned physical output before acquiring it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteEndpoint {
    /// Owner/backend selected by the instance.
    pub backend: SiteBackend,
    /// Stable device identity, never an enumeration position.
    pub device: String,
    /// Channel or digital-line identity within the device.
    pub channel: String,
}

/// Engineering-unit conversion after the adapter has normalized raw units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Calibration {
    /// Preserve adapter source units.
    None,
    /// Linear mapping between two distinct, finite source points.
    TwoPoint {
        /// First source-unit reference.
        input_low: f64,
        /// Engineering value at the first reference.
        value_low: f64,
        /// Second source-unit reference.
        input_high: f64,
        /// Engineering value at the second reference.
        value_high: f64,
    },
}

impl Calibration {
    /// Apply calibration once; nonfinite results are acquisition faults.
    pub fn apply(self, value: f64) -> Option<f64> {
        let result = match self {
            Self::None => value,
            Self::TwoPoint {
                input_low,
                value_low,
                input_high,
                value_high,
            } => {
                value_low
                    + (value - input_low) / (input_high - input_low) * (value_high - value_low)
            }
        };
        result.is_finite().then_some(result)
    }
}

/// Resolved analog source and display policy; precision never rounds stored PCM-independent readings.
#[derive(Clone, Debug, PartialEq)]
pub struct SensorSettings {
    /// Node-local stable name, shared with digital inputs.
    pub name: String,
    /// Whether acquisition may run after capability validation.
    pub enabled: bool,
    /// Instance-only source identity.
    pub endpoint: Option<SiteEndpoint>,
    /// Optional localized label; an empty value uses the stable name.
    pub label_message_id: String,
    /// Engineering unit used in reports.
    pub unit: String,
    /// Display decimal places, not measurement precision.
    pub precision: u8,
    /// Positive acquisition interval.
    pub poll_interval_ms: u64,
    /// Age at which a successful observation stops being current.
    pub stale_after_ms: u64,
    /// Exponential smoothing time constant; zero disables smoothing.
    pub smoothing_ms: u64,
    /// Conversion of already-normalized source units.
    pub calibration: Calibration,
    /// Optional canonical positive decimal query selector.
    pub dtmf_id: Option<String>,
}

/// Resolved site digital input, independent of radio COR/CTCSS debounce.
#[derive(Clone, Debug, PartialEq)]
pub struct InputSettings {
    /// Node-local stable name, shared with sensors.
    pub name: String,
    /// Whether this input may be acquired.
    pub enabled: bool,
    /// Instance-only source identity.
    pub endpoint: Option<SiteEndpoint>,
    /// Electrical high means logical active when true.
    pub active_high: bool,
    /// Time the changed state must remain stable.
    pub debounce_ms: u64,
    /// Read-only query selector.
    pub dtmf_id: Option<String>,
    /// Ordered action references on a debounced active edge.
    pub on_active: Vec<SiteAction>,
    /// Ordered action references on a debounced inactive edge.
    pub on_inactive: Vec<SiteAction>,
}

/// Resolved logical output; hardware readback is not implied by a requested state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSettings {
    /// Node-local stable name.
    pub name: String,
    /// Whether this output may be acquired.
    pub enabled: bool,
    /// Instance-only owned endpoint.
    pub endpoint: Option<SiteEndpoint>,
    /// Electrical polarity of logical on.
    pub active_high: bool,
    /// Initial logical demand.
    pub startup_on: bool,
    /// Logical demand on failure and shutdown.
    pub safe_on: bool,
    /// Default retriggerable pulse duration.
    pub pulse_ms: u64,
}

/// Ordered named site instances; defaults never create an instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedSiteSettings {
    /// Analog measurements in configuration order.
    pub sensors: Vec<SensorSettings>,
    /// Site digital inputs in configuration order.
    pub inputs: Vec<InputSettings>,
    /// Site digital outputs in configuration order.
    pub outputs: Vec<OutputSettings>,
    /// Threshold/health policies in configuration order.
    pub alarms: Vec<AlarmSettings>,
    /// Cooling policies in configuration order.
    pub fans: Vec<FanSettings>,
}

impl ResolvedSiteSettings {
    /// Validate and resolve all fields before opening hardware.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: resolve_definitions(document, node)?.value,
            warnings,
        })
    }
}

pub(super) fn key_known(kind: SiteKind, named: bool, key: &str) -> bool {
    let identity = matches!(key, "backend" | "device" | "channel" | "line" | "dtmf_id");
    (!identity || named)
        && match kind {
            SiteKind::Sensor => matches!(
                key,
                "enabled"
                    | "backend"
                    | "device"
                    | "channel"
                    | "label_message_id"
                    | "unit"
                    | "precision"
                    | "poll_interval_ms"
                    | "stale_after_ms"
                    | "smoothing_ms"
                    | "calibration"
                    | "input_low"
                    | "value_low"
                    | "input_high"
                    | "value_high"
                    | "dtmf_id"
            ),
            SiteKind::Input => matches!(
                key,
                "enabled"
                    | "backend"
                    | "device"
                    | "line"
                    | "active_level"
                    | "debounce_ms"
                    | "dtmf_id"
                    | "on_active"
                    | "on_inactive"
            ),
            SiteKind::Output => matches!(
                key,
                "enabled"
                    | "backend"
                    | "device"
                    | "line"
                    | "active_level"
                    | "startup_state"
                    | "safe_state"
                    | "pulse_ms"
            ),
            SiteKind::Alarm => matches!(
                key,
                "enabled"
                    | "source"
                    | "condition"
                    | "trip_value"
                    | "clear_value"
                    | "reading"
                    | "trip_delay_ms"
                    | "clear_delay_ms"
                    | "latched"
                    | "repeat_interval_ms"
                    | "on_trip"
                    | "on_clear"
                    | "trip_message"
                    | "clear_message"
                    | "repeat_message"
            ),
            SiteKind::Fan => matches!(
                key,
                "enabled"
                    | "output"
                    | "mode"
                    | "ptt_nodes"
                    | "run_on_ms"
                    | "temperature_sensor"
                    | "on_temperature"
                    | "off_temperature"
                    | "temperature_curve"
                    | "rpm_sensor"
                    | "minimum_rpm"
                    | "spinup_ms"
            ),
        }
}

fn finite(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|value| value.is_finite())
}

pub(super) fn value_valid(kind: SiteKind, key: &str, value: &str) -> bool {
    if matches!(kind, SiteKind::Alarm | SiteKind::Fan) {
        return policy::value_valid(kind, key, value);
    }
    match key {
        "enabled" => parse::boolean(value).is_some(),
        "on_active" | "on_inactive" => policy::actions(value).is_some(),
        "backend" => SiteBackend::parse(value).is_some_and(|backend| {
            matches!(
                (kind, backend),
                (SiteKind::Sensor, SiteBackend::Hwmon | SiteBackend::Iio)
                    | (
                        SiteKind::Input,
                        SiteBackend::Cm119 | SiteBackend::Parallel | SiteBackend::Gpiod
                    )
                    | (
                        SiteKind::Output,
                        SiteBackend::Cm119
                            | SiteBackend::Parallel
                            | SiteBackend::Gpiod
                            | SiteBackend::Hwmon
                    )
            )
        }),
        "device" | "channel" | "line" => {
            !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
        }
        "precision" => parse::unsigned(value, 0, 15).is_some(),
        "poll_interval_ms" => parse::unsigned(value, 1, u64::MAX - 1).is_some(),
        "stale_after_ms" | "pulse_ms" => parse::unsigned(value, 1, u64::MAX).is_some(),
        "smoothing_ms" | "debounce_ms" => parse::unsigned(value, 0, u64::MAX).is_some(),
        "calibration" => matches!(value, "none" | "two_point"),
        "input_low" | "input_high" | "value_low" | "value_high" => finite(value).is_some(),
        "dtmf_id" => super::messages::selector_valid(value),
        "active_level" => matches!(value, "high" | "low"),
        "startup_state" | "safe_state" => matches!(value, "on" | "off"),
        _ => !value.chars().any(char::is_control),
    }
}

pub(super) fn default_value(key: &str) -> &'static str {
    match key {
        "enabled" => "no",
        "precision" => "1",
        "poll_interval_ms" => "1000",
        "stale_after_ms" => "5000",
        "smoothing_ms" => "0",
        "calibration" => "none",
        "active_level" => "high",
        "debounce_ms" => "50",
        "startup_state" | "safe_state" => "off",
        "pulse_ms" => "500",
        _ => policy::default_value(key),
    }
}

/// Validated field-wise lookup shared by site families, with instance-only identities.
struct Fields<'a> {
    document: &'a ConfigDocument,
    kind: SiteKind,
    scopes: Vec<String>,
}

impl<'a> Fields<'a> {
    fn new(document: &'a ConfigDocument, kind: SiteKind, node: &NodeId, name: &str) -> Self {
        let family = kind.name();
        Self {
            document,
            kind,
            scopes: vec![
                format!("{family} {} {name}", node.as_str()),
                format!("{family} {}", node.as_str()),
                family.to_owned(),
            ],
        }
    }

    fn value(&self, key: &str) -> Option<String> {
        let scopes = if matches!(key, "backend" | "device" | "channel" | "line" | "dtmf_id") {
            &self.scopes[..1]
        } else {
            &self.scopes
        };
        settings::lookup_valid_scopes(self.document, key, scopes, |value| {
            value_valid(self.kind, key, value).then(|| value.to_owned())
        })
    }

    fn text(&self, key: &str) -> String {
        self.value(key)
            .unwrap_or_else(|| default_value(key).to_owned())
    }
    fn number(&self, key: &str) -> u64 {
        self.text(key)
            .parse()
            .expect("validated numeric site field")
    }
    fn enabled(&self) -> bool {
        parse::boolean(&self.text("enabled")).expect("validated boolean")
    }
    fn endpoint(&self) -> Option<SiteEndpoint> {
        Some(SiteEndpoint {
            backend: SiteBackend::parse(&self.value("backend")?)?,
            device: self.value("device")?,
            channel: self.value(if self.kind == SiteKind::Sensor {
                "channel"
            } else {
                "line"
            })?,
        })
    }
    fn warn(&self, warnings: &mut Vec<ConfigWarning>, key: &str, reason: &str, fallback: &str) {
        let entry = self
            .document
            .section_entries(&self.scopes[0])
            .find(|entry| entry.key == key);
        warnings.push(ConfigWarning::new(
            entry.map_or(0, |entry| entry.line),
            &self.scopes[0],
            key,
            entry.map_or("", |entry| entry.value.as_str()),
            reason,
            fallback,
        ));
    }
    fn acquisition(&self, warnings: &mut Vec<ConfigWarning>) -> (bool, Option<SiteEndpoint>) {
        let endpoint = self.endpoint();
        let mut enabled = self.enabled();
        if enabled && endpoint.is_none() {
            self.warn(
                warnings,
                "enabled",
                "enabled site instance requires a complete endpoint",
                "no",
            );
            enabled = false;
        }
        (enabled, endpoint)
    }
    fn calibration(&self) -> Option<Calibration> {
        if self.text("calibration") == "none" {
            return Some(Calibration::None);
        }
        let input_low = finite(&self.value("input_low")?)?;
        let input_high = finite(&self.value("input_high")?)?;
        let value_low = finite(&self.value("value_low")?)?;
        let value_high = finite(&self.value("value_high")?)?;
        let input_span = input_high - input_low;
        let value_span = value_high - value_low;
        (input_span != 0.0
            && input_span.is_finite()
            && value_span.is_finite()
            && (value_span / input_span).is_finite())
        .then_some(Calibration::TwoPoint {
            input_low,
            input_high,
            value_low,
            value_high,
        })
    }
    fn actions(&self, key: &str) -> Vec<SiteAction> {
        policy::actions(&self.text(key)).expect("validated action list")
    }
}

pub(super) fn resolve_definitions(
    document: &ConfigDocument,
    node: &NodeId,
) -> Result<Resolution<ResolvedSiteSettings>, ConfigError> {
    let mut result = ResolvedSiteSettings::default();
    let mut warnings = Vec::new();
    let mut names = BTreeSet::new();
    let mut selectors = BTreeSet::new();
    for kind in [SiteKind::Sensor, SiteKind::Input, SiteKind::Output] {
        for section in document.named_sections(kind.name(), Some(node.as_str())) {
            let name = section
                .rsplit(' ')
                .next()
                .expect("named site scope")
                .to_owned();
            let fields = Fields::new(document, kind, node, &name);
            let (mut enabled, endpoint) = fields.acquisition(&mut warnings);
            let dtmf_id = fields.value("dtmf_id").filter(|value| !value.is_empty());
            if kind != SiteKind::Output
                && (!names.insert(name.clone())
                    || dtmf_id
                        .as_ref()
                        .is_some_and(|selector| !selectors.insert(selector.clone())))
            {
                return Err(ConfigError::structure(
                    &fields.scopes[0],
                    "ambiguous site name or DTMF selector",
                ));
            }
            if kind == SiteKind::Sensor {
                let calibration = fields.calibration();
                if calibration.is_none() {
                    fields.warn(&mut warnings, "calibration", "two-point calibration requires distinct finite source points and finite slope", "sensor disabled");
                    enabled = false;
                }
                let poll_interval_ms = fields.number("poll_interval_ms");
                let stale_after_ms = settings::lookup_valid_scopes(
                    document,
                    "stale_after_ms",
                    &fields.scopes,
                    |raw| parse::unsigned(raw, poll_interval_ms + 1, u64::MAX),
                )
                .unwrap_or(5000.max(poll_interval_ms + 1));
                if fields.number("stale_after_ms") != stale_after_ms {
                    fields.warn(
                        &mut warnings,
                        "stale_after_ms",
                        "stale time must exceed polling interval",
                        &stale_after_ms.to_string(),
                    );
                }
                result.sensors.push(SensorSettings {
                    name,
                    enabled,
                    endpoint,
                    label_message_id: fields.text("label_message_id"),
                    unit: fields.text("unit"),
                    precision: fields.number("precision") as u8,
                    poll_interval_ms,
                    stale_after_ms,
                    smoothing_ms: fields.number("smoothing_ms"),
                    calibration: calibration.unwrap_or(Calibration::None),
                    dtmf_id,
                });
            } else if kind == SiteKind::Input {
                result.inputs.push(InputSettings {
                    name,
                    enabled,
                    endpoint,
                    active_high: fields.text("active_level") == "high",
                    debounce_ms: fields.number("debounce_ms"),
                    dtmf_id,
                    on_active: fields.actions("on_active"),
                    on_inactive: fields.actions("on_inactive"),
                });
            } else {
                result.outputs.push(OutputSettings {
                    name,
                    enabled,
                    endpoint,
                    active_high: fields.text("active_level") == "high",
                    startup_on: fields.text("startup_state") == "on",
                    safe_on: fields.text("safe_state") == "on",
                    pulse_ms: fields.number("pulse_ms"),
                });
            }
        }
    }
    policy::resolve(document, node, &mut result)?;
    Ok(Resolution {
        value: result,
        warnings,
    })
}

#[cfg(test)]
#[path = "site_tests.rs"]
mod tests;
