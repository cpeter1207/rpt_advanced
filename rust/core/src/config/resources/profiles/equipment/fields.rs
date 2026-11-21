//! Checked authoring field types for each public station equipment family.
use super::*;

fn finite(v: &Value) -> Option<Value> {
    v.as_f64().filter(|n| n.is_finite()).map(|_| v.clone())
}
fn string(v: &Value) -> Option<Value> {
    v.as_str().filter(|s| !s.contains('\0')).map(Value::from)
}
fn device(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 255 && !s.contains('\0'))
        .map(Value::from)
}
fn selector(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| !s.is_empty() && !s.starts_with('0') && s.bytes().all(|c| c.is_ascii_digit()))
        .map(Value::from)
}
fn property(v: &Value) -> Option<Value> {
    let path = v.as_str()?;
    let pointer = jsonptr::Pointer::parse(path).ok()?;
    let mut components = pointer.tokens();
    let root = components.next()?;
    (matches!(root.decoded().as_ref(), "gpio" | "parallel" | "audio")
        && components.next().is_some())
    .then(|| v.clone())
}
fn actions(v: &Value) -> Option<Value> {
    let rows = v.as_array()?;
    rows.iter()
        .all(|row| {
            row.as_str()
                .and_then(crate::config::site::parse_action)
                .is_some()
        })
        .then(|| v.clone())
}
fn ports(v: &Value) -> Option<Value> {
    let mut names = BTreeSet::new();
    v.as_array()?
        .iter()
        .all(|v| v.as_str().is_some_and(|s| names.insert(s)))
        .then(|| v.clone())
}
fn temperature_curve(v: &Value) -> Option<Value> {
    let points = v
        .as_array()?
        .iter()
        .map(|row| {
            Some((
                row.get("temperature")?.as_f64()?,
                row.get("duty")?.as_f64()?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    if !crate::config::site::curve_valid(&points) {
        return None;
    }
    Some(Value::Array(
        points
            .into_iter()
            .map(|(temperature, duty)| serde_json::json!({"temperature":temperature,"duty":duty}))
            .collect(),
    ))
}

const SENSOR_SOURCE: Fields = &[
    ("type", Rule::Choice(&["hwmon", "iio", "interface"])),
    ("device", Rule::Value(device)),
    ("channel", Rule::Value(device)),
    ("node", Rule::Value(|v| v.as_str().map(Value::from))),
    ("property", Rule::Value(property)),
];
const INPUT_SOURCE: Fields = &[
    ("type", Rule::Choice(&["gpiod", "interface"])),
    ("device", Rule::Value(device)),
    ("line", Rule::Value(device)),
    ("node", Rule::Value(|v| v.as_str().map(Value::from))),
    ("property", Rule::Value(property)),
];
const OUTPUT_SOURCE: Fields = &[
    ("type", Rule::Choice(&["hwmon", "gpiod", "interface"])),
    ("device", Rule::Value(device)),
    ("line", Rule::Value(device)),
    ("channel", Rule::Value(device)),
    ("node", Rule::Value(|v| v.as_str().map(Value::from))),
    ("property", Rule::Value(property)),
];
const POINT: Fields = &[("low", Rule::Value(finite)), ("high", Rule::Value(finite))];
pub(super) const SENSOR: Fields = &[
    ("enabled", Rule::Bool),
    ("unit", Rule::Value(string)),
    ("precision", Rule::Unsigned(0, 15)),
    ("dtmf_id", Rule::Value(selector)),
    ("label", Rule::Value(string)),
    (
        "timing",
        Rule::Object(&[
            ("poll_interval_ms", Rule::Unsigned(1, u64::MAX - 1)),
            ("stale_after_ms", Rule::Unsigned(1, u64::MAX)),
            ("smoothing_ms", Rule::Unsigned(0, u64::MAX)),
        ]),
    ),
    (
        "calibration",
        Rule::Object(&[
            ("mode", Rule::Choice(&["none", "two_point"])),
            ("input", Rule::Object(POINT)),
            ("value", Rule::Object(POINT)),
        ]),
    ),
    ("source", Rule::Object(SENSOR_SOURCE)),
];
pub(super) const INPUT: Fields = &[
    ("enabled", Rule::Bool),
    ("active_level", Rule::Choice(&["high", "low"])),
    ("dtmf_id", Rule::Value(selector)),
    (
        "timing",
        Rule::Object(&[("debounce_ms", Rule::Unsigned(0, u64::MAX))]),
    ),
    ("on_active", Rule::Value(actions)),
    ("on_inactive", Rule::Value(actions)),
    ("source", Rule::Object(INPUT_SOURCE)),
];
pub(super) const OUTPUT: Fields = &[
    ("enabled", Rule::Bool),
    ("active_level", Rule::Choice(&["high", "low"])),
    ("startup_state", Rule::Choice(&["on", "off"])),
    ("safe_state", Rule::Choice(&["on", "off"])),
    (
        "timing",
        Rule::Object(&[("pulse_ms", Rule::Unsigned(1, u64::MAX))]),
    ),
    ("source", Rule::Object(OUTPUT_SOURCE)),
];
pub(super) const ALARM: Fields = &[
    ("enabled", Rule::Bool),
    ("source", Rule::Value(string)),
    (
        "condition",
        Rule::Choice(&["above", "below", "active", "inactive", "unavailable"]),
    ),
    ("trip_value", Rule::Value(finite)),
    ("clear_value", Rule::Value(finite)),
    ("reading", Rule::Choice(&["current", "smoothed"])),
    (
        "timing",
        Rule::Object(&[
            ("trip_delay_ms", Rule::Unsigned(0, u64::MAX)),
            ("clear_delay_ms", Rule::Unsigned(0, u64::MAX)),
            ("repeat_interval_ms", Rule::Unsigned(0, u64::MAX)),
        ]),
    ),
    ("latched", Rule::Bool),
    ("trip_message", Rule::Value(string)),
    ("clear_message", Rule::Value(string)),
    ("repeat_message", Rule::Value(string)),
    ("on_trip", Rule::Value(actions)),
    ("on_clear", Rule::Value(actions)),
];
pub(super) const FAN: Fields = &[
    ("enabled", Rule::Bool),
    ("output", Rule::Value(string)),
    ("mode", Rule::Choice(&["ptt", "temperature", "combined"])),
    ("ptt_nodes", Rule::Value(ports)),
    (
        "timing",
        Rule::Object(&[
            ("run_on_ms", Rule::Unsigned(0, u64::MAX)),
            ("spinup_ms", Rule::Unsigned(0, u64::MAX)),
        ]),
    ),
    ("temperature_sensor", Rule::Value(string)),
    ("on_temperature", Rule::Value(finite)),
    ("off_temperature", Rule::Value(finite)),
    ("temperature_curve", Rule::Value(temperature_curve)),
    ("rpm_sensor", Rule::Value(string)),
    (
        "minimum_rpm",
        Rule::Value(|v| {
            v.as_f64()
                .filter(|n| n.is_finite() && *n > 0.0)
                .map(|_| v.clone())
        }),
    ),
];
