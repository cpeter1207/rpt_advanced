//! Alarm and fan configuration resolved against named site instances.

use super::{
    ConfigDocument, ConfigError, Fields, NodeId, ResolvedSiteSettings, SiteKind, finite, parse,
};
use crate::site::OutputRequest;
use std::collections::BTreeSet;

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

/// Parse a comma-separated ordered list; malformed entries invalidate the whole
/// list, allowing ordinary inherited fallback rather than partial protection.
pub(super) fn actions(value: &str) -> Option<Vec<SiteAction>> {
    if value.is_empty() {
        return Some(Vec::new());
    }
    value
        .split(',')
        .map(|item| {
            let parts: Vec<_> = item.trim().split(':').collect();
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
        })
        .collect()
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
impl AlarmCondition {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "above" => Some(Self::Above),
            "below" => Some(Self::Below),
            "active" => Some(Self::Active),
            "inactive" => Some(Self::Inactive),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
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
impl FanMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "ptt" => Some(Self::Ptt),
            "temperature" => Some(Self::Temperature),
            "combined" => Some(Self::Combined),
            _ => None,
        }
    }
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
    pub ptt_nodes: Vec<NodeId>,
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

fn curve(value: &str) -> Option<Vec<(f64, f64)>> {
    let points: Vec<_> = value
        .split(',')
        .map(|point| {
            let (temperature, duty) = point.trim().split_once(':')?;
            Some((
                finite(temperature.trim())?,
                finite(duty.trim()).filter(|value| (0.0..=1.0).contains(value))?,
            ))
        })
        .collect::<Option<_>>()?;
    (points.len() >= 2
        && points.windows(2).all(|pair| {
            pair[0].0 < pair[1].0 && pair[0].1 <= pair[1].1 && (pair[1].0 - pair[0].0).is_finite()
        }))
    .then_some(points)
}

pub(super) fn value_valid(_kind: SiteKind, key: &str, value: &str) -> bool {
    match key {
        "on_trip" | "on_clear" => actions(value).is_some(),
        "enabled" | "latched" => parse::boolean(value).is_some(),
        "condition" => AlarmCondition::parse(value).is_some(),
        "mode" => FanMode::parse(value).is_some(),
        "reading" => matches!(value, "current" | "smoothed"),
        "trip_delay_ms" | "clear_delay_ms" | "repeat_interval_ms" | "run_on_ms" | "spinup_ms" => {
            parse::unsigned(value, 0, u64::MAX).is_some()
        }
        "trip_value" | "clear_value" | "on_temperature" | "off_temperature" => {
            finite(value).is_some()
        }
        "minimum_rpm" => finite(value).is_some_and(|value| value > 0.0),
        "temperature_curve" => curve(value).is_some(),
        "ptt_nodes" => {
            !value.is_empty()
                && value
                    .split(',')
                    .all(|node| NodeId::new(node.trim()).is_ok())
        }
        _ => !value.chars().any(char::is_control),
    }
}

pub(super) fn default_value(key: &str) -> &'static str {
    match key {
        "condition" => "unavailable",
        "reading" => "current",
        "latched" => "no",
        "trip_delay_ms" | "clear_delay_ms" | "repeat_interval_ms" => "0",
        "mode" => "ptt",
        "run_on_ms" => "120000",
        "spinup_ms" => "1000",
        _ => "",
    }
}

pub(super) fn resolve(
    document: &ConfigDocument,
    node: &NodeId,
    result: &mut ResolvedSiteSettings,
) -> Result<(), ConfigError> {
    let sensor = |name: &str| result.sensors.iter().any(|sensor| sensor.name == name);
    let input = |name: &str| result.inputs.iter().any(|input| input.name == name);
    for section in document.named_sections("alarm", Some(node.as_str())) {
        let name = section.rsplit(' ').next().expect("named alarm");
        let fields = Fields::new(document, SiteKind::Alarm, node, name);
        let condition =
            AlarmCondition::parse(&fields.text("condition")).expect("validated condition");
        let source = fields.text("source");
        let trip_value = fields.value("trip_value").as_deref().and_then(finite);
        let clear_value = fields.value("clear_value").as_deref().and_then(finite);
        let thresholds = trip_value
            .zip(clear_value)
            .is_some_and(|(trip, clear)| match condition {
                AlarmCondition::Above => trip >= clear,
                AlarmCondition::Below => trip <= clear,
                _ => true,
            });
        let valid = match condition {
            AlarmCondition::Above | AlarmCondition::Below => sensor(&source) && thresholds,
            AlarmCondition::Active | AlarmCondition::Inactive => input(&source),
            AlarmCondition::Unavailable => sensor(&source) || input(&source),
        };
        if fields.enabled() && !valid {
            return Err(ConfigError::structure(
                section,
                "enabled alarm requires a compatible source and ordered thresholds",
            ));
        }
        result.alarms.push(AlarmSettings {
            name: name.into(),
            enabled: fields.enabled(),
            source,
            condition,
            trip_value,
            clear_value,
            smoothed: fields.text("reading") == "smoothed",
            trip_delay_ms: fields.number("trip_delay_ms"),
            clear_delay_ms: fields.number("clear_delay_ms"),
            latched: parse::boolean(&fields.text("latched")).unwrap(),
            repeat_interval_ms: fields.number("repeat_interval_ms"),
            on_trip: fields.actions("on_trip"),
            on_clear: fields.actions("on_clear"),
            trip_message: fields.text("trip_message"),
            clear_message: fields.text("clear_message"),
            repeat_message: fields.text("repeat_message"),
        });
    }
    let mut owned = BTreeSet::new();
    for section in document.named_sections("fan", Some(node.as_str())) {
        let name = section.rsplit(' ').next().expect("named fan");
        let fields = Fields::new(document, SiteKind::Fan, node, name);
        let output = fields.text("output");
        let mode = FanMode::parse(&fields.text("mode")).expect("validated mode");
        let temperature_sensor = fields.text("temperature_sensor");
        let on_temperature = fields.value("on_temperature").as_deref().and_then(finite);
        let off_temperature = fields.value("off_temperature").as_deref().and_then(finite);
        let temperature_curve = fields
            .value("temperature_curve")
            .as_deref()
            .and_then(curve)
            .unwrap_or_default();
        let rpm_sensor = fields.text("rpm_sensor");
        let minimum_rpm = fields.value("minimum_rpm").as_deref().and_then(finite);
        let ptt_nodes: Vec<_> = fields
            .value("ptt_nodes")
            .map(|value| {
                value
                    .split(',')
                    .map(|node| NodeId::new(node.trim()).unwrap())
                    .collect()
            })
            .unwrap_or_else(|| vec![node.clone()]);
        let valid = result
            .outputs
            .iter()
            .any(|candidate| candidate.name == output)
            && ptt_nodes
                .iter()
                .all(|node| document.nodes().contains(&node.as_str()))
            && (mode == FanMode::Ptt
                || (sensor(&temperature_sensor)
                    && (!temperature_curve.is_empty()
                        || on_temperature
                            .zip(off_temperature)
                            .is_some_and(|(on, off)| on >= off))))
            && (rpm_sensor.is_empty() || (sensor(&rpm_sensor) && minimum_rpm.is_some()));
        if fields.enabled() && (!valid || !owned.insert(output.clone())) {
            return Err(ConfigError::structure(
                section,
                "enabled fan requires one unclaimed output and valid demand/feedback sources",
            ));
        }
        result.fans.push(FanSettings {
            name: name.into(),
            enabled: fields.enabled(),
            output,
            mode,
            ptt_nodes,
            run_on_ms: fields.number("run_on_ms"),
            temperature_sensor,
            on_temperature,
            off_temperature,
            temperature_curve,
            rpm_sensor,
            minimum_rpm,
            spinup_ms: fields.number("spinup_ms"),
        });
    }
    validate_references(document, node, result)
}

fn validate_references(
    document: &ConfigDocument,
    node: &NodeId,
    settings: &ResolvedSiteSettings,
) -> Result<(), ConfigError> {
    let named = |family, name: &str| {
        document
            .named_sections(family, None)
            .into_iter()
            .chain(document.named_sections(family, Some(node.as_str())))
            .any(|section| section.rsplit(' ').next() == Some(name))
    };
    let valid_action = |action: &SiteAction| match action {
        SiteAction::Macro(name) => named("macro", name),
        SiteAction::Output { name, .. } => {
            settings.outputs.iter().any(|output| &output.name == name)
                && !settings
                    .fans
                    .iter()
                    .any(|fan| fan.enabled && &fan.output == name)
        }
        SiteAction::Fan { name, .. } => settings.fans.iter().any(|fan| &fan.name == name),
        SiteAction::Inhibit(_) => true,
    };
    for input in settings.inputs.iter().filter(|input| input.enabled) {
        if !input
            .on_active
            .iter()
            .chain(&input.on_inactive)
            .all(valid_action)
        {
            return Err(ConfigError::structure(
                &input.name,
                "input action references an unknown or fan-owned target",
            ));
        }
    }
    for alarm in settings.alarms.iter().filter(|alarm| alarm.enabled) {
        if !alarm
            .on_trip
            .iter()
            .chain(&alarm.on_clear)
            .all(valid_action)
        {
            return Err(ConfigError::structure(
                &alarm.name,
                "alarm action references an unknown or fan-owned target",
            ));
        }
        for message in [
            &alarm.trip_message,
            &alarm.clear_message,
            &alarm.repeat_message,
        ] {
            if !message.is_empty() && !named("message", message) {
                return Err(ConfigError::structure(
                    &alarm.name,
                    "alarm message references an unknown recipe",
                ));
            }
        }
    }
    Ok(())
}
