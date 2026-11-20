//! Public sparse fields checked before signal binding inheritance.
use super::super::super::normalize::{Fields, Rule};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn tenths(v: &Value) -> Option<u16> {
    u16::try_from(crate::command_language::scaled(v.as_number()?.as_str(), 1)?).ok()
}
fn tone(v: &Value) -> Option<Value> {
    tenths(v)
        .filter(|f| (500..=3000).contains(f))
        .map(|_| v.clone())
}
pub(super) const DSP_TONES: &[u16] = &[
    670, 719, 744, 770, 797, 825, 854, 885, 915, 948, 974, 1000, 1035, 1072, 1109, 1148, 1188,
    1230, 1273, 1318, 1365, 1413, 1462, 1514, 1567, 1622, 1679, 1738, 1799, 1862, 1928, 2035, 2107,
    2181, 2257, 2336, 2418, 2503,
];
fn tones(v: &Value, distinct_keys: bool) -> Option<Value> {
    let list = v.as_array()?;
    let mut seen = BTreeSet::new();
    (!list.is_empty()
        && list.len() <= DSP_TONES.len()
        && list.iter().all(|v| {
            tenths(v).is_some_and(|n| DSP_TONES.contains(&n) && (!distinct_keys || seen.insert(n)))
        }))
    .then(|| v.clone())
}
fn positive(v: &Value) -> Option<Value> {
    v.as_f64()
        .filter(|n| *n > 0.0 && (*n as f32).is_finite())
        .map(|_| v.clone())
}
fn pins(v: &Value) -> Option<Value> {
    let list = v.as_array()?;
    let mut seen = BTreeSet::new();
    (!list.is_empty()
        && list.len() <= 8
        && list.iter().all(|x| {
            x.as_str()
                .is_some_and(|s| !s.is_empty() && seen.insert(s.to_owned()))
                || x.as_u64().is_some_and(|n| seen.insert(n.to_string()))
        }))
    .then(|| v.clone())
}
fn dcs(v: &Value) -> Option<Value> {
    let [a, b, c, polarity] = *v.as_str()?.as_bytes() else {
        return None;
    };
    ([a, b, c].into_iter().all(|b| (b'0'..=b'7').contains(&b)) && matches!(polarity, b'N' | b'I'))
        .then(|| v.clone())
}
const RX_SQUELCH: Fields = &[
    (
        "source",
        Rule::Choice(&["dsp", "sound_chip_gpio", "parallel", "serial", "key_down"]),
    ),
    ("level", Rule::Unsigned(0, 999)),
    ("hysteresis", Rule::Unsigned(0, 32767)),
    ("noise_filter", Rule::Choice(&["standard", "alternate"])),
    ("inverted", Rule::Bool),
    ("scancode", Rule::Unsigned(0, 4294967295)),
    ("pin", Rule::Value(pins)),
    ("enabled", Rule::Bool),
];
const RX_VAD_TIMING: Fields = &[("hang_ms", Rule::Unsigned(0, 32767))];
const RX_VAD: Fields = &[
    ("enabled", Rule::Bool),
    ("mode", Rule::Choice(&["kerchunk", "continuous"])),
    ("timing", Rule::Object(RX_VAD_TIMING)),
];
const RX_CTCSS: Fields = &[
    (
        "source",
        Rule::Choice(&[
            "dsp",
            "sound_chip_gpio",
            "parallel",
            "serial",
            "external",
            "hamlib",
            "auto",
        ]),
    ),
    ("tones_hz", Rule::Value(|v| tones(v, true))),
    ("gain_db", Rule::Number(-60.0, 60.0)),
    ("relaxed", Rule::Bool),
    ("override", Rule::Bool),
    ("inverted", Rule::Bool),
    ("pin", Rule::Value(pins)),
    ("enabled", Rule::Bool),
    ("frequency_hz", Rule::Value(tone)),
];
const RX_DCS: Fields = &[
    ("enabled", Rule::Bool),
    (
        "source",
        Rule::Choice(&["dsp", "external", "hamlib", "auto"]),
    ),
    ("code", Rule::Value(dcs)),
];
const RX_TIMING: Fields = &[
    ("on_delay_ms", Rule::Unsigned(0, 60000)),
    ("squelch_delay_ms", Rule::Unsigned(0, 511)),
    ("kerchunk_max_ms", Rule::Unsigned(0, 4294967295)),
];
pub(in super::super) const RX: Fields = &[
    ("squelch", Rule::Object(RX_SQUELCH)),
    ("vad", Rule::Object(RX_VAD)),
    ("ctcss", Rule::Object(RX_CTCSS)),
    ("dcs", Rule::Object(RX_DCS)),
    ("timing", Rule::Object(RX_TIMING)),
];
const TX_PTT_TIMING: Fields = &[
    ("settle_ms", Rule::Unsigned(0, 2147483647)),
    ("off_delay_ms", Rule::Unsigned(0, 60000)),
    ("hang_ms", Rule::Unsigned(0, 4294967295)),
    ("timeout_ms", Rule::Unsigned(0, 4294967295)),
    ("rx_blanking_ms", Rule::Unsigned(0, 32767)),
];
const TX_PTT: Fields = &[
    ("timing", Rule::Object(TX_PTT_TIMING)),
    (
        "source",
        Rule::Choice(&[
            "sound_chip_gpio",
            "parallel",
            "serial",
            "key_down",
            "hamlib",
        ]),
    ),
    ("inverted", Rule::Bool),
    ("scancode", Rule::Unsigned(0, 4294967295)),
    ("pin", Rule::Value(pins)),
];
const TX_CTCSS_TURNOFF_TIMING: Fields = &[("duration_ms", Rule::Unsigned(40, 32767))];
const TX_CTCSS_TURNOFF: Fields = &[
    (
        "mode",
        Rule::Choice(&["none", "phase_shift", "tone_remove", "tail_tone"]),
    ),
    ("phase_shift_degrees", Rule::Value(positive)),
    ("timing", Rule::Object(TX_CTCSS_TURNOFF_TIMING)),
    ("tail_tone_hz", Rule::Value(positive)),
];
const TX_CTCSS_TIMING: Fields = &[("hang_ms", Rule::Unsigned(0, 4294967295))];
const TX_CTCSS_OUTPUT: Fields = &[
    (
        "source",
        Rule::Choice(&["sound_chip_gpio", "parallel", "serial"]),
    ),
    ("inverted", Rule::Bool),
    ("pin", Rule::Value(pins)),
];
const TX_CTCSS: Fields = &[
    ("tones_hz", Rule::Value(|v| tones(v, false))),
    ("frequency_hz", Rule::Value(tone)),
    ("level_dbfs", Rule::Number(-90.0, 0.0)),
    ("turnoff", Rule::Object(TX_CTCSS_TURNOFF)),
    ("on_input", Rule::Bool),
    ("timing", Rule::Object(TX_CTCSS_TIMING)),
    ("enabled", Rule::Bool),
    (
        "source",
        Rule::Choice(&["dsp", "external", "hamlib", "auto"]),
    ),
    ("output", Rule::Object(TX_CTCSS_OUTPUT)),
];
const TX_DCS_TURNOFF_TIMING: Fields = &[("duration_ms", Rule::Unsigned(150, 200))];
const TX_DCS_TURNOFF: Fields = &[
    ("enabled", Rule::Bool),
    ("timing", Rule::Object(TX_DCS_TURNOFF_TIMING)),
];
const TX_DCS_OUTPUT: Fields = &[
    (
        "source",
        Rule::Choice(&["sound_chip_gpio", "parallel", "serial"]),
    ),
    ("inverted", Rule::Bool),
    ("pin", Rule::Value(pins)),
];
const TX_DCS: Fields = &[
    ("code", Rule::Value(dcs)),
    ("level_dbfs", Rule::Number(-90.0, 0.0)),
    ("turnoff", Rule::Object(TX_DCS_TURNOFF)),
    ("enabled", Rule::Bool),
    (
        "source",
        Rule::Choice(&["dsp", "external", "hamlib", "auto"]),
    ),
    ("output", Rule::Object(TX_DCS_OUTPUT)),
];
pub(in super::super) const TX: Fields = &[
    ("ptt", Rule::Object(TX_PTT)),
    ("ctcss", Rule::Object(TX_CTCSS)),
    ("dcs", Rule::Object(TX_DCS)),
];
