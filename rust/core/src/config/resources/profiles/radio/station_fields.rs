//! Sparse physical interface and radio-general public fields.
use super::super::super::normalize::{Fields, Rule};
use serde_json::Value;
fn text(v: &Value) -> Option<Value> {
    v.as_str().filter(|s| !s.contains('\0')).map(Value::from)
}
fn name(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| s.chars().count() <= 255)
        .and_then(|_| text(v))
}
fn device(v: &Value) -> Option<Value> {
    v.as_str().filter(|s| !s.is_empty()).and_then(|_| name(v))
}
fn clip(v: &Value) -> Option<Value> {
    if v.is_null() {
        return Some(Value::Null);
    }
    let n = v.as_u64().or_else(|| v.as_str()?.parse().ok())?;
    (1..=8).contains(&n).then(|| Value::from(n))
}
fn stop(v: &Value) -> Option<Value> {
    v.as_f64()
        .filter(|n| [1.0, 1.5, 2.0].contains(n))
        .map(|_| v.clone())
}
fn flow_off(v: &Value) -> Option<Value> {
    (v.as_bool() == Some(false)).then(|| v.clone())
}
fn mhz(v: &Value) -> Option<Value> {
    u64::try_from(crate::command_language::scaled(v.as_number()?.as_str(), 6)?)
        .ok()
        .map(|_| v.clone())
}
const INTERFACE_AUDIO_TIMING: Fields = &[
    ("input_extra_buffer_ms", Rule::Unsigned(0, 500)),
    ("output_extra_buffer_ms", Rule::Unsigned(0, 500)),
];
const INTERFACE_AUDIO: Fields = &[
    (
        "device_selection",
        Rule::Choice(&["exact", "automatic_lowest_alsa_card"]),
    ),
    ("device_identifier", Rule::Value(name)),
    ("usb_serial", Rule::Value(name)),
    ("input_device_channels", Rule::Unsigned(1, 2)),
    ("output_device_channels", Rule::Unsigned(1, 2)),
    ("timing", Rule::Object(INTERFACE_AUDIO_TIMING)),
    ("input_gain_db", Rule::Number(-30.0, 30.0)),
    ("output_a_gain_db", Rule::Number(-30.0, 30.0)),
    ("output_b_gain_db", Rule::Number(-30.0, 30.0)),
    ("eeprom_enabled", Rule::Bool),
    ("backend", Rule::Choice(&["sound_chip", "alsa"])),
];
const INTERFACE_TX: Fields = &[];
const INTERFACE_RX: Fields = &[];
const PARALLEL_OUTPUTS: Fields = &[
    ("2", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("3", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("4", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("5", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("6", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("7", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("8", Rule::Choice(&["", "out0", "out1", "ptt"])),
    ("9", Rule::Choice(&["", "out0", "out1", "ptt"])),
];
const PARALLEL_INPUTS: Fields = &[
    ("10", Rule::Choice(&["", "in", "cor", "ctcss"])),
    ("12", Rule::Choice(&["", "in", "cor", "ctcss"])),
    ("13", Rule::Choice(&["", "in", "cor", "ctcss"])),
    ("15", Rule::Choice(&["", "in", "cor", "ctcss"])),
];
const INTERFACE_PARALLEL: Fields = &[
    ("device", Rule::Value(text)),
    ("base_address", Rule::Unsigned(1, 4294967295)),
    ("outputs", Rule::Object(PARALLEL_OUTPUTS)),
    ("inputs", Rule::Object(PARALLEL_INPUTS)),
];
const INTERFACE_SERIAL: Fields = &[
    ("device", Rule::Value(device)),
    ("baud_rate", Rule::Unsigned(1, 4294967295)),
    ("data_bits", Rule::Unsigned(5, 8)),
    (
        "parity",
        Rule::Choice(&["none", "even", "odd", "mark", "space"]),
    ),
    ("stop_bits", Rule::Value(stop)),
    ("rtscts", Rule::Value(flow_off)),
    ("dsrdtr", Rule::Value(flow_off)),
    ("xonxoff", Rule::Value(flow_off)),
];
const GPIO_PINS: Fields = &[
    ("1", Rule::Choice(&["in", "out0", "out1"])),
    ("2", Rule::Choice(&["in", "out0", "out1"])),
    ("3", Rule::Choice(&["in", "out0", "out1"])),
    ("4", Rule::Choice(&["in", "out0", "out1"])),
    ("5", Rule::Choice(&["in", "out0", "out1"])),
    ("6", Rule::Choice(&["in", "out0", "out1"])),
    ("7", Rule::Choice(&["in", "out0", "out1"])),
    ("8", Rule::Choice(&["in", "out0", "out1"])),
];
const INTERFACE_GPIO: Fields = &[
    ("pins", Rule::Object(GPIO_PINS)),
    (
        "wiring_profile",
        Rule::Choice(&["dudeusb", "sphusb", "nhrc", "custom"]),
    ),
    ("clip_led_pin", Rule::Value(clip)),
];
pub(in super::super) const INTERFACE: Fields = &[
    ("audio", Rule::Object(INTERFACE_AUDIO)),
    ("tx", Rule::Object(INTERFACE_TX)),
    ("rx", Rule::Object(INTERFACE_RX)),
    ("parallel", Rule::Object(INTERFACE_PARALLEL)),
    ("serial", Rule::Object(INTERFACE_SERIAL)),
    ("keyboard", Rule::Object(&[("device", Rule::Value(device))])),
    ("gpio", Rule::Object(INTERFACE_GPIO)),
];
const GENERAL_PARROT: Fields = &[("enabled", Rule::Bool)];
const GENERAL_TIMING: Fields = &[
    ("status_snapshot_interval_ms", Rule::Unsigned(1, 4294967295)),
    ("scan_slow_ms", Rule::Unsigned(1, 4294967295)),
    ("scan_medium_ms", Rule::Unsigned(1, 4294967295)),
    ("scan_fast_ms", Rule::Unsigned(1, 4294967295)),
    ("tune_duration_ms", Rule::Unsigned(1, 5000)),
];
pub(in super::super) const GENERAL: Fields = &[
    ("parrot", Rule::Object(GENERAL_PARROT)),
    ("timing", Rule::Object(GENERAL_TIMING)),
    ("startup_memory", Rule::Unsigned(1, 4294967295)),
    ("offset_magnitude_mhz", Rule::Value(mhz)),
    ("duplex_mode", Rule::Choice(&["half", "full"])),
];
