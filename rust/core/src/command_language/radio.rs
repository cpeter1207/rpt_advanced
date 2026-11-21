//! Typed radio operations in native units; capabilities remain the physical owner's responsibility.
use super::{
    ActionError,
    fields::{Fields, Result},
};
use crate::remote_base_command::{
    ArgumentDecoder as D, CatalogKind, DecimalValue, RemoteCommand as C, RemoteOffset,
};
use serde_json::Value;

/// Capability value before provider metadata selects enum/boolean/scalar conversion.
#[derive(Clone)]
pub enum CapabilityValue {
    /// JSON boolean.
    Boolean(bool),
    /// Exact numeric token, including sign, exponent and decimal precision.
    Number(serde_json::Number),
    /// Declared enum identity or decimal text.
    Text(String),
}
/// Sparse normalized tone/signaling request, preserving omission and explicit null.
#[derive(Clone, Default)]
pub struct OperatingSignal {
    /// Declared provider source.
    pub source: Option<&'static str>,
    /// Explicit controller source or null to clear.
    pub control_source: Option<Option<&'static str>>,
    /// Enable or disable this direction.
    pub enabled: Option<bool>,
    /// Exact tenths of hertz.
    pub ctcss: Option<u32>,
    /// Nine-bit digital code.
    pub dcs: Option<u16>,
    /// Digital code polarity.
    pub inverted: Option<bool>,
}
/// Sparse provider operating-state request expressed in native units.
#[derive(Clone, Default)]
pub struct OperatingState {
    /// Requested modulation.
    pub mode: Option<&'static str>,
    /// Selected RX VFO.
    pub rx_vfo: Option<&'static str>,
    /// Selected TX VFO.
    pub tx_vfo: Option<&'static str>,
    /// Split operation.
    pub split: Option<bool>,
    /// Receive incremental tuning enable.
    pub rit_enabled: Option<bool>,
    /// Transmit incremental tuning enable.
    pub xit_enabled: Option<bool>,
    /// Optional RF power override; explicit null clears it.
    pub power_mw: Option<Option<u32>>,
    /// Optional channel override; explicit null clears it.
    pub channel: Option<Option<u8>>,
    /// Exact receive frequency in hertz.
    pub rx_hz: Option<u64>,
    /// Transmit frequency in hertz, or null for receive only.
    pub tx_hz: Option<Option<u64>>,
    /// Exact bandwidth in hertz.
    pub bandwidth_hz: Option<u64>,
    /// Signed offset in hertz.
    pub offset_hz: Option<i64>,
    /// Signed receive adjustment in hertz.
    pub rit_hz: Option<i64>,
    /// Signed transmit adjustment in hertz.
    pub xit_hz: Option<i64>,
    /// Transmit signaling settings.
    pub encode: Option<OperatingSignal>,
    /// Receive signaling settings.
    pub decode: Option<OperatingSignal>,
}
/// Prepared radio intent or metadata-checked input for the physical owner.
#[derive(Clone)]
pub enum RadioAction {
    /// Existing remote operation, including the canonical integer wire conversion.
    Command(C),
    /// Declared selector, value and optional normalized units.
    Setting(u32, CapabilityValue, Option<String>),
    /// Raw DTMF value and authored constraints for provider type, units and range validation.
    /// The owner must verify agreement before applying the request.
    ConstrainedSetting {
        /// Decoded public setting selector.
        selector: u32,
        /// Original sign, precision and magnitude of the received tuple.
        raw: DecimalValue,
        /// A fixed typed value, when supplied.
        value: Option<CapabilityValue>,
        /// Authored units, when supplied.
        units: Option<String>,
    },
    /// Exact requested operating state.
    Set(Box<OperatingState>),
}
const SOURCES: &[&str] = &["disabled", "dsp", "external", "hamlib", "auto"];
fn choice(
    f: &mut Fields<'_>,
    k: &'static str,
    options: &'static [&'static str],
) -> Result<Option<&'static str>> {
    f.take(k)
        .map(|_| f.choice(k, options).map(|i| options[i]))
        .transpose()
}
fn signal(v: &Value) -> Result<OperatingSignal> {
    let mut f = Fields::new(v)?;
    let source = choice(&mut f, "source", SOURCES)?;
    let control_source = match f.take("control_source") {
        Some(Value::Null) => Some(None),
        Some(_) => Some(choice(&mut f, "control_source", SOURCES)?),
        None => None,
    };
    let enabled = f.take("enabled").map(|_| f.flag("enabled")).transpose()?;
    let ctcss = f
        .take("ctcss")
        .map(|v| {
            let mut c = Fields::new(v)?;
            let hz = c
                .take("frequency_hz")
                .map(|_| {
                    c.scaled("frequency_hz", 1, 0, u32::MAX.into())
                        .map(|v| v as u32)
                })
                .transpose()?;
            c.finish()?;
            Ok(hz)
        })
        .transpose()?
        .flatten();
    let (dcs, inverted) = if let Some(v) = f.take("dcs") {
        let mut c = Fields::new(v)?;
        let bits = c
            .take("bits")
            .map(|_| c.uint("bits", 0, 511).map(|v| v as u16))
            .transpose()?;
        let inverted = c.take("inverted").map(|_| c.flag("inverted")).transpose()?;
        c.finish()?;
        (bits, inverted)
    } else {
        (None, None)
    };
    f.finish()?;
    Ok(OperatingSignal {
        source,
        control_source,
        enabled,
        ctcss,
        dcs,
        inverted,
    })
}
pub(super) fn operating(v: &Value) -> Result<OperatingState> {
    if v.as_object().is_none_or(|m| m.is_empty()) {
        return Err(ActionError::Field("state"));
    }
    let mut f = Fields::new(v)?;
    const VFO: &[&str] = &["current", "A", "B", "main", "sub", "memory"];
    let mut s = OperatingState {
        mode: choice(
            &mut f,
            "mode",
            &[
                "FM", "AM", "USB", "LSB", "CW", "CWR", "RTTY", "RTTYR", "WFM", "PKTFM", "PKTUSB",
                "PKTLSB",
            ],
        )?,
        rx_vfo: choice(&mut f, "rx_vfo", VFO)?,
        tx_vfo: choice(&mut f, "tx_vfo", VFO)?,
        ..Default::default()
    };
    for (k, out) in [
        ("split", &mut s.split),
        ("rit_enabled", &mut s.rit_enabled),
        ("xit_enabled", &mut s.xit_enabled),
    ] {
        *out = f.take(k).map(|_| f.flag(k)).transpose()?;
    }
    if let Some(v) = f.take("power_mw") {
        s.power_mw = Some(
            (!v.is_null())
                .then(|| f.uint("power_mw", 0, u32::MAX.into()).map(|n| n as u32))
                .transpose()?,
        );
    }
    if let Some(v) = f.take("channel") {
        s.channel = Some(
            (!v.is_null())
                .then(|| f.uint("channel", 0, 15).map(|n| n as u8))
                .transpose()?,
        );
    }
    if let Some(v) = f.take("tx_frequency_mhz") {
        s.tx_hz = Some(
            (!v.is_null())
                .then(|| {
                    f.scaled("tx_frequency_mhz", 6, 1, u64::MAX.into())
                        .map(|n| n as u64)
                })
                .transpose()?,
        );
    }
    for (k, out, min) in [
        ("rx_frequency_mhz", &mut s.rx_hz, 1),
        ("bandwidth_mhz", &mut s.bandwidth_hz, 0),
    ] {
        *out = f
            .take(k)
            .map(|_| f.scaled(k, 6, min, u64::MAX.into()).map(|n| n as u64))
            .transpose()?;
    }
    for (k, out) in [
        ("offset_mhz", &mut s.offset_hz),
        ("rit_mhz", &mut s.rit_hz),
        ("xit_mhz", &mut s.xit_hz),
    ] {
        *out = f
            .take(k)
            .map(|_| {
                f.scaled(k, 6, i64::MIN.into(), i64::MAX.into())
                    .map(|n| n as i64)
            })
            .transpose()?;
    }
    s.encode = f.take("encode").map(signal).transpose()?;
    s.decode = f.take("decode").map(signal).transpose()?;
    f.finish()?;
    Ok(s)
}
fn selector(f: &mut Fields<'_>, k: &'static str) -> Result<u32> {
    Ok(f.uint(k, 1, u32::MAX.into())? as u32)
}
fn offset(f: &mut Fields<'_>) -> Result<RemoteOffset> {
    Ok([
        RemoteOffset::Minus,
        RemoteOffset::Simplex,
        RemoteOffset::Plus,
    ][f.choice("offset_direction", &["minus", "simplex", "plus"])?])
}

fn setting_value(f: &mut Fields<'_>) -> Result<CapabilityValue> {
    match f.get("value")? {
        Value::Number(n) => Ok(CapabilityValue::Number(n.clone())),
        Value::Bool(v) => Ok(CapabilityValue::Boolean(*v)),
        Value::String(s) => Ok(CapabilityValue::Text(s.clone())),
        _ => Err(ActionError::Field("value")),
    }
}
fn setting_units(f: &mut Fields<'_>) -> Result<Option<String>> {
    f.take("units")
        .map(|_| f.text("units").map(String::from))
        .transpose()
}

pub(super) fn typed(f: &mut Fields<'_>) -> Result<RadioAction> {
    let command = match f.text("operation")? {
        "recall_memory" => C::Recall(selector(f, "selector")?),
        "set_frequency" => C::Frequency {
            hertz: f.scaled("frequency_mhz", 6, 1, u64::MAX.into())? as u64,
            offset: offset(f)?,
        },
        "set_tone" => C::Tone {
            encode: f.choice("direction", &["rx", "tx"])? == 1,
            tenths_hz: f.scaled("frequency_hz", 1, 500, 3000)? as u32,
        },
        "set_tone_enabled" => C::ToneEnabled {
            encode: f.choice("direction", &["rx", "tx"])? == 1,
            enabled: f.flag("enabled")?,
        },
        "set_power_preset" => C::PowerPreset(f.choice("preset", &["low", "medium", "high"])? as u8),
        "status" => C::Status {
            long: f.choice("detail", &["short", "long"])? == 1,
        },
        "set_mode" => C::Mode(selector(f, "selector")?),
        "step" => C::Step(f.scaled("step_mhz", 6, i64::MIN.into(), i64::MAX.into())? as i64),
        "scan_preset" => C::ScanPreset {
            up: f.choice("direction", &["down", "up"])? == 1,
            preset: f.choice("preset", &["slow", "medium", "fast"])? as u8,
        },
        "tune" => C::Tune,
        "get_setting" => C::GetSetting(selector(f, "selector")?),
        "set_setting" => {
            let selector = selector(f, "selector")?;
            let value = setting_value(f)?;
            let units = setting_units(f)?;
            return Ok(RadioAction::Setting(selector, value, units));
        }
        "start_scan" => C::StartScan(selector(f, "selector")?),
        "pause_scan" => C::PauseScan,
        "resume_scan" => C::ResumeScan,
        "stop_scan" => C::StopScan,
        "set_scan_direction" => C::ScanDirection(f.choice("direction", &["down", "up"])? == 1),
        "skip" => C::Skip,
        "get_memory" => C::Memory(selector(f, "selector")?),
        "catalog" => C::Catalog {
            kind: [
                CatalogKind::Capabilities,
                CatalogKind::Memories,
                CatalogKind::ScanLists,
            ][f.choice("kind", &["capabilities", "memories", "scan_lists"])?],
            page: selector(f, "page")?,
        },
        "scan_status" => C::ScanStatus(selector(f, "selector")?),
        "set_inhibit" => C::Inhibit(f.flag("enabled")?),
        "set_scan_dwell" => C::Dwell(f.duration(1, u64::MAX)?),
        "set_scan_resume_delay" => C::ResumeDelay(f.duration(0, u64::MAX)?),
        "set_scan_priority" => C::Priority(if f.get("selector")?.is_null() {
            None
        } else {
            Some(selector(f, "selector")?)
        }),
        "current_status" => C::CurrentStatus,
        "invoke" => C::Invoke(selector(f, "selector")?),
        "set" => return Ok(RadioAction::Set(Box::new(operating(f.get("state")?)?))),
        _ => return Err(ActionError::Field("operation")),
    };
    Ok(RadioAction::Command(command))
}
/// Select a parameter decoder only for operations with an audited DTMF grammar.
#[derive(Clone)]
pub(super) struct RadioDecoder {
    operation: D,
    fixed: RadioConstraint,
}
#[derive(Clone)]
enum RadioConstraint {
    None,
    Frequency(Option<u64>, Option<RemoteOffset>),
    Setting {
        selector: Option<u32>,
        value: Option<CapabilityValue>,
        units: Option<String>,
    },
}
impl RadioDecoder {
    pub(super) fn decode(&self, tail: &str) -> Option<RadioAction> {
        let command = self.operation.decode(tail)?;
        let valid = match (&self.fixed, command) {
            (RadioConstraint::None, _) => true,
            (RadioConstraint::Frequency(hz, dir), C::Frequency { hertz, offset }) => {
                hz.is_none_or(|v| v == hertz) && dir.is_none_or(|v| v == offset)
            }
            (
                RadioConstraint::Setting {
                    selector: fixed,
                    value,
                    units,
                },
                C::SetSetting {
                    selector,
                    value: raw,
                },
            ) => {
                if fixed.is_some_and(|id| id != selector) {
                    return None;
                }
                if let (Some(CapabilityValue::Number(n)), None) = (value, units) {
                    let incoming = i128::from(raw.at_decimal_places(raw.decimal_places)?);
                    if super::scaled(n.as_str(), raw.decimal_places as i32) != Some(incoming) {
                        return None;
                    }
                }
                if value.is_some() || units.is_some() {
                    return Some(RadioAction::ConstrainedSetting {
                        selector,
                        raw,
                        value: value.clone(),
                        units: units.clone(),
                    });
                }
                true
            }
            _ => false,
        };
        valid.then_some(RadioAction::Command(command))
    }
}
pub(super) fn decoder(f: &mut Fields<'_>) -> Result<RadioDecoder> {
    let mut fixed = RadioConstraint::None;
    let operation = match f.text("operation")? {
        "recall_memory" => D::Recall,
        "set_frequency" => {
            fixed = RadioConstraint::Frequency(
                f.take("frequency_mhz")
                    .map(|_| {
                        f.scaled("frequency_mhz", 6, 1, u64::MAX.into())
                            .map(|n| n as u64)
                    })
                    .transpose()?,
                f.take("offset_direction").map(|_| offset(f)).transpose()?,
            );
            D::Frequency
        }
        "set_tone" => {
            if f.choice("direction", &["rx", "tx"])? == 0 {
                D::ReceiveTone
            } else {
                D::TransmitTone
            }
        }
        "get_setting" => D::GetSetting,
        "set_setting" => {
            fixed = RadioConstraint::Setting {
                selector: f
                    .take("selector")
                    .map(|_| selector(f, "selector"))
                    .transpose()?,
                value: f.take("value").map(|_| setting_value(f)).transpose()?,
                units: setting_units(f)?,
            };
            D::SetSetting
        }
        "start_scan" => D::StartScan,
        "get_memory" => D::Memory,
        "catalog" => [
            D::CatalogCapabilities,
            D::CatalogMemories,
            D::CatalogScanLists,
        ][f.choice("kind", &["capabilities", "memories", "scan_lists"])?],
        "scan_status" => D::ScanStatus,
        "set_scan_dwell" => D::Dwell,
        "set_scan_resume_delay" => D::ResumeDelay,
        "set_scan_priority" => D::Priority,
        "invoke" => D::Invoke,
        _ => return Err(ActionError::Field("operation")),
    };
    Ok(RadioDecoder { operation, fixed })
}
