//! Authored processing fields and their public limits for each independent source role.
use super::super::super::normalize::{Fields, Rule};
use super::Stage;
use serde_json::Value;
fn order(v: &Value) -> Option<Value> {
    let a = v.as_array()?;
    let mut seen = Vec::new();
    for item in a {
        let stage = Stage::from_public(item.as_str()?)?;
        if seen.contains(&stage) {
            return None;
        }
        seen.push(stage);
    }
    (!seen.is_empty()).then(|| v.clone())
}
fn oversample(v: &Value) -> Option<Value> {
    matches!(v.as_u64(), Some(1 | 2 | 4)).then(|| v.clone())
}
fn corner(v: &Value) -> Option<Value> {
    v.as_f64()
        .filter(|n| *n > 0.0 && *n <= 500.0)
        .map(|_| v.clone())
}
const COMMON_EQUALIZER_LOW: Fields = &[
    ("gain_db", Rule::Number(-12.0, 12.0)),
    ("frequency_hz", Rule::Number(20.0, 1000.0)),
    ("slope", Rule::Number(0.1, 1.0)),
];

const COMMON_EQUALIZER_MID: Fields = &[
    ("gain_db", Rule::Number(-12.0, 12.0)),
    ("frequency_hz", Rule::Number(100.0, 4000.0)),
    ("width_octaves", Rule::Number(0.1, 4.0)),
];

const COMMON_EQUALIZER_HIGH: Fields = &[
    ("gain_db", Rule::Number(-12.0, 12.0)),
    ("frequency_hz", Rule::Number(1000.0, 5000.0)),
    ("slope", Rule::Number(0.1, 1.0)),
];

const COMMON_EQUALIZER: Fields = &[
    ("low", Rule::Object(COMMON_EQUALIZER_LOW)),
    ("mid", Rule::Object(COMMON_EQUALIZER_MID)),
    ("high", Rule::Object(COMMON_EQUALIZER_HIGH)),
    ("enabled", Rule::Bool),
];

const COMMON_AGC_TIMING: Fields = &[
    ("loudness_window_ms", Rule::Number(400.0, 3000.0)),
    ("gain_increase_db_per_second", Rule::Number(0.1, 100.0)),
    ("gain_decrease_db_per_second", Rule::Number(0.1, 100.0)),
    ("hold_ms", Rule::Number(0.0, 10000.0)),
];

const COMMON_AGC: Fields = &[
    ("target_lufs", Rule::Number(-40.0, -3.0)),
    ("max_gain_db", Rule::Number(0.0, 30.0)),
    ("max_attenuation_db", Rule::Number(0.0, 60.0)),
    ("timing", Rule::Object(COMMON_AGC_TIMING)),
    ("activity_threshold_dbfs", Rule::Number(-100.0, -3.0)),
    ("activity_hysteresis_db", Rule::Number(0.0, 12.0)),
    ("deadband_db", Rule::Number(0.0, 6.0)),
    ("enabled", Rule::Bool),
];

const COMMON_EXPANDER_TIMING: Fields = &[
    ("attack_ms", Rule::Number(1.0, 1000.0)),
    ("release_ms", Rule::Number(1.0, 10000.0)),
];

const COMMON_EXPANDER_SIDECHAIN: Fields = &[
    ("highpass_hz", Rule::Number(50.0, 2000.0)),
    ("lowpass_hz", Rule::Number(50.0, 3500.0)),
];

const COMMON_EXPANDER: Fields = &[
    ("threshold_dbfs", Rule::Number(-100.0, -10.0)),
    ("ratio", Rule::Number(1.0, 10.0)),
    ("max_attenuation_db", Rule::Number(0.0, 40.0)),
    ("timing", Rule::Object(COMMON_EXPANDER_TIMING)),
    ("sidechain", Rule::Object(COMMON_EXPANDER_SIDECHAIN)),
    ("enabled", Rule::Bool),
];

const COMMON_DEESSER_TIMING: Fields = &[
    ("attack_ms", Rule::Number(0.1, 100.0)),
    ("release_ms", Rule::Number(1.0, 2000.0)),
];

const COMMON_DEESSER: Fields = &[
    ("frequency_hz", Rule::Number(2000.0, 8000.0)),
    ("width_octaves", Rule::Number(0.1, 4.0)),
    ("threshold_dbfs", Rule::Number(-60.0, -1.0)),
    ("ratio", Rule::Number(1.0, 20.0)),
    ("max_reduction_db", Rule::Number(0.1, 20.0)),
    ("timing", Rule::Object(COMMON_DEESSER_TIMING)),
    ("enabled", Rule::Bool),
];

const COMMON_COMPRESSOR_CROSSOVER: Fields = &[
    ("low_hz", Rule::Number(100.0, 2000.0)),
    ("high_hz", Rule::Number(100.0, 5000.0)),
];

const COMMON_COMPRESSOR_DEFAULTS_TIMING: Fields = &[
    ("attack_ms", Rule::Number(1.0, 1000.0)),
    ("release_ms", Rule::Number(1.0, 9000.0)),
];

const COMMON_COMPRESSOR_DEFAULTS: Fields = &[
    ("threshold_dbfs", Rule::Number(-60.0, 0.0)),
    ("ratio", Rule::Number(1.0, 20.0)),
    ("makeup_gain_db", Rule::Number(-30.0, 30.0)),
    ("timing", Rule::Object(COMMON_COMPRESSOR_DEFAULTS_TIMING)),
];

const COMMON_COMPRESSOR_LOW: Fields = &[
    ("threshold_dbfs", Rule::Number(-60.0, 0.0)),
    ("ratio", Rule::Number(1.0, 20.0)),
    ("makeup_gain_db", Rule::Number(-30.0, 30.0)),
    ("timing", Rule::Object(COMMON_COMPRESSOR_DEFAULTS_TIMING)),
    ("knee_db", Rule::Number(0.0, 18.0)),
];

const COMMON_COMPRESSOR: Fields = &[
    ("crossover", Rule::Object(COMMON_COMPRESSOR_CROSSOVER)),
    ("sidechain", Rule::Object(COMMON_EXPANDER_SIDECHAIN)),
    ("enabled", Rule::Bool),
    ("defaults", Rule::Object(COMMON_COMPRESSOR_DEFAULTS)),
    ("low", Rule::Object(COMMON_COMPRESSOR_LOW)),
    ("mid", Rule::Object(COMMON_COMPRESSOR_LOW)),
    ("high", Rule::Object(COMMON_COMPRESSOR_LOW)),
];

const COMMON_MULTIBAND_LIMITER_DEFAULTS_TIMING: Fields = &[
    ("attack_ms", Rule::Number(0.1, 1000.0)),
    ("release_ms", Rule::Number(1.0, 9000.0)),
];

const COMMON_MULTIBAND_LIMITER_DEFAULTS: Fields = &[
    ("threshold_dbfs", Rule::Number(-40.0, -1.0)),
    ("ratio", Rule::Number(1.0, 20.0)),
    ("knee_db", Rule::Number(0.0, 18.0)),
    (
        "timing",
        Rule::Object(COMMON_MULTIBAND_LIMITER_DEFAULTS_TIMING),
    ),
];

const COMMON_MULTIBAND_LIMITER_HIGH_TIMING: Fields = &[
    ("attack_ms", Rule::Number(0.1, 100.0)),
    ("release_ms", Rule::Number(1.0, 1000.0)),
];

const COMMON_MULTIBAND_LIMITER_HIGH: Fields = &[
    ("threshold_dbfs", Rule::Number(-30.0, -1.0)),
    ("ratio", Rule::Number(1.0, 20.0)),
    ("knee_db", Rule::Number(0.0, 18.0)),
    ("timing", Rule::Object(COMMON_MULTIBAND_LIMITER_HIGH_TIMING)),
];

const COMMON_MULTIBAND_LIMITER: Fields = &[
    ("crossover", Rule::Object(COMMON_COMPRESSOR_CROSSOVER)),
    ("enabled", Rule::Bool),
    ("defaults", Rule::Object(COMMON_MULTIBAND_LIMITER_DEFAULTS)),
    ("low", Rule::Object(COMMON_MULTIBAND_LIMITER_DEFAULTS)),
    ("mid", Rule::Object(COMMON_MULTIBAND_LIMITER_DEFAULTS)),
    ("high", Rule::Object(COMMON_MULTIBAND_LIMITER_HIGH)),
];

const COMMON_PEAK_LIMITER_TIMING: Fields = &[
    ("lookahead_ms", Rule::Number(0.1, 20.0)),
    ("attack_ms", Rule::Number(0.1, 20.0)),
    ("release_ms", Rule::Number(1.0, 5000.0)),
];

const COMMON_PEAK_LIMITER: Fields = &[
    ("ceiling_dbfs", Rule::Number(-30.0, -0.1)),
    ("timing", Rule::Object(COMMON_PEAK_LIMITER_TIMING)),
    ("enabled", Rule::Bool),
    ("oversample", Rule::Value(oversample)),
];

macro_rules! role_fields { ($($extra:expr),* $(,)?) => { &[
    ("enabled", Rule::Bool),
    ("input_gain_db", Rule::Number(-30.0, 30.0)),
    ("output_gain_db", Rule::Number(-30.0, 30.0)),
    ("stage_order", Rule::Value(order)),
    ("equalizer", Rule::Object(COMMON_EQUALIZER)),
    ("agc", Rule::Object(COMMON_AGC)),
    ("expander", Rule::Object(COMMON_EXPANDER)),
    ("deesser", Rule::Object(COMMON_DEESSER)),
    ("compressor", Rule::Object(COMMON_COMPRESSOR)),
    ("multiband_limiter", Rule::Object(COMMON_MULTIBAND_LIMITER)),
    ("peak_limiter", Rule::Object(COMMON_PEAK_LIMITER)),
    $($extra,)*
] }; }

pub(in super::super) const LINK: Fields = role_fields!();

const LOCAL_RNNOISE: Fields = &[("enabled", Rule::Bool)];

const LOCAL_BANDPASS: Fields = &[
    ("enabled", Rule::Bool),
    ("highpass_hz", Rule::Number(20.0, 2000.0)),
    ("lowpass_hz", Rule::Number(20.0, 6000.0)),
];

const LOCAL_CTCSS_FILTER: Fields = &[
    ("mode", Rule::Choice(&["disabled", "notch", "highpass"])),
    ("notch_width_hz", Rule::Number(10.0, 10.0)),
    ("highpass_hz", Rule::Number(50.0, 500.0)),
];

const LOCAL: Fields = role_fields!(
    ("rnnoise", Rule::Object(LOCAL_RNNOISE)),
    ("bandpass", Rule::Object(LOCAL_BANDPASS)),
    ("ctcss_filter", Rule::Object(LOCAL_CTCSS_FILTER))
);

const VOICE_BANDPASS: Fields = &[
    ("enabled", Rule::Bool),
    ("highpass_hz", Rule::Number(0.0, 300.0)),
    ("lowpass_hz", Rule::Number(2500.0, 20000.0)),
];

const VOICE: Fields = role_fields!(("bandpass", Rule::Object(VOICE_BANDPASS)));

const RECEIVEAUDIO_DEEMPHASIS: Fields =
    &[("corner_hz", Rule::Value(corner)), ("enabled", Rule::Bool)];

fn receive_gain(v: &Value) -> Option<Value> {
    v.as_i64()
        .filter(|n| (-30..=30).contains(n))
        .map(Value::from)
}
pub(in super::super) const RX_AUDIO: Fields = &[
    ("input_gain_db", Rule::Number(-30.0, 30.0)),
    ("output_gain_db", Rule::Value(receive_gain)),
    ("deemphasis", Rule::Object(RECEIVEAUDIO_DEEMPHASIS)),
    ("processing", Rule::Object(LOCAL)),
    ("enabled", Rule::Bool),
];

const TRANSMITAUDIO_PREEMPHASIS: Fields =
    &[("enabled", Rule::Bool), ("corner_hz", Rule::Value(corner))];

pub(in super::super) const TX_AUDIO: Fields = &[
    ("preemphasis", Rule::Object(TRANSMITAUDIO_PREEMPHASIS)),
    (
        "output_a_assignment",
        Rule::Choice(&["disabled", "voice", "tone", "composite", "auxiliary_voice"]),
    ),
    (
        "output_b_assignment",
        Rule::Choice(&["disabled", "voice", "tone", "composite", "auxiliary_voice"]),
    ),
    ("processing", Rule::Object(VOICE)),
    ("enabled", Rule::Bool),
];
