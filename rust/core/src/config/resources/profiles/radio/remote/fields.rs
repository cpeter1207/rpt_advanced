//! Public scalar policies with exact RF and power conversion.
use super::super::super::normalize::{Fields, Rule, normalize, warn};
use super::{CandidateWarning, ResourceBinding};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(super) const MODES: &[&str] = &[
    "FM", "AM", "USB", "LSB", "CW", "CWR", "RTTY", "RTTYR", "WFM", "PKTFM", "PKTUSB", "PKTLSB",
];
pub(super) fn scaled(v: &Value, places: i32) -> Option<u64> {
    u64::try_from(crate::command_language::scaled(
        v.as_number()?.as_str(),
        places,
    )?)
    .ok()
}
fn power(v: &Value) -> Option<Value> {
    u32::try_from(scaled(v, 3)?).ok().map(|_| v.clone())
}
fn required_ranges(v: &Value) -> Option<Value> {
    (!v.as_array()?.is_empty()).then_some(())?;
    ranges(v)
}
fn ranges(v: &Value) -> Option<Value> {
    v.as_array()?
        .iter()
        .all(|r| {
            let Some([low, high]) = r.as_array().map(Vec::as_slice) else {
                return false;
            };
            scaled(low, 6)
                .zip(scaled(high, 6))
                .is_some_and(|(a, b)| a > 0 && a <= b)
        })
        .then(|| v.clone())
}
fn modes(v: &Value) -> Option<Value> {
    let a = v.as_array()?;
    let mut seen = BTreeSet::new();
    (!a.is_empty()
        && a.iter().all(|s| {
            s.as_str()
                .is_some_and(|m| MODES.contains(&m) && seen.insert(m))
        }))
    .then(|| v.clone())
}
fn model(v: &Value) -> Option<Value> {
    let suffix = v.as_str()?.strip_prefix("RIG_MODEL_")?;
    (!suffix.is_empty()
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    .then(|| v.clone())
}
fn text(v: &Value) -> Option<Value> {
    v.as_str().filter(|s| !s.contains('\0')).map(Value::from)
}
fn options(v: &Value) -> Option<Value> {
    v.is_object().then(|| v.clone())
}
pub(in super::super) fn control(
    b: ResourceBinding,
    raw: &Map<String, Value>,
    w: &mut Vec<CandidateWarning>,
) -> Map<String, Value> {
    let mut out = normalize(b, raw, CONTROL, w);
    if let Some(options) = out
        .get_mut("hamlib")
        .and_then(|v| v.get_mut("options"))
        .and_then(Value::as_object_mut)
    {
        options.retain(|k, v| {
            let valid = !k.is_empty()
                && !k.contains('\0')
                && !matches!(
                    k.as_str(),
                    "timeout" | "retry" | "timeout_retry" | "rig_pathname"
                )
                && text(v).is_some();
            if !valid {
                warn(w, b, "options")
            }
            valid
        });
    }
    out
}
const CONTROL: Fields = &[
    ("enabled", Rule::Bool),
    (
        "backend",
        Rule::Choice(&["hamlib", "parallel_channel", "parallel_rtx"]),
    ),
    ("access", Rule::Choice(&["open", "locked", "disabled"])),
    ("retries", Rule::Unsigned(0, 10)),
    ("timeout_retries", Rule::Unsigned(0, 100)),
    (
        "timing",
        Rule::Object(&[
            ("timeout_ms", Rule::Unsigned(1, 10000)),
            ("poll_interval_ms", Rule::Unsigned(1, u64::MAX)),
        ]),
    ),
    (
        "hamlib",
        Rule::Object(&[
            ("model", Rule::Value(model)),
            ("device", Rule::Value(text)),
            ("options", Rule::Value(options)),
        ]),
    ),
];
pub(in super::super) const LIMITS: Fields = &[
    ("modes", Rule::Value(modes)),
    ("rx_ranges_mhz", Rule::Value(required_ranges)),
    ("tx_ranges_mhz", Rule::Value(ranges)),
    ("rx_only_ranges_mhz", Rule::Value(ranges)),
];
pub(in super::super) const POWER: Fields = &[
    ("max_w", Rule::Value(power)),
    ("low_w", Rule::Value(power)),
    ("medium_w", Rule::Value(power)),
    ("high_w", Rule::Value(power)),
];
