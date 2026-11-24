//! Resolve source-dependent controls together after inherited fields are available.
use super::{CandidateWarning, Inputs, SignalSource, WiringProfile, warn};
use serde_json::Value;

/// Resolved signal observation or actuator and its polarity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalBinding {
    /// Selected signal owner.
    pub source: SignalSource,
    /// Physical connector pin numbers including DTE DB-9 pins for serial. Empty selects a dedicated provider signal.
    pub pins: Vec<u8>,
    /// Selected Linux evdev EV_KEY code for a held-key observation.
    pub scancode: Option<u32>,
    /// Applied independently to each raw observation or driven output.
    pub inverted: bool,
}
const SERIAL_PINS: [(u8, &str); 6] = [
    (1, "DCD"),
    (4, "DTR"),
    (6, "DSR"),
    (7, "RTS"),
    (8, "CTS"),
    (9, "RI"),
];

impl SignalBinding {
    /// Authored order of the selected serial modem line names.
    pub fn serial_pins(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.pins.iter().filter_map(move |pin| {
            SERIAL_PINS
                .iter()
                .find(|(number, _)| self.source == SignalSource::Serial && number == pin)
                .map(|(_, name)| *name)
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Use {
    Squelch,
    Ptt,
    Decode,
    Encoder,
}
impl Use {
    fn output(self) -> bool {
        matches!(self, Self::Ptt | Self::Encoder)
    }
    fn allows(self, source: SignalSource) -> bool {
        use SignalSource::*;
        match self {
            Self::Squelch => matches!(source, Dsp | SoundChipGpio | Parallel | Serial | KeyDown),
            Self::Ptt => matches!(
                source,
                SoundChipGpio | Parallel | Serial | Hamlib | NoActuator
            ),
            Self::Decode => matches!(
                source,
                Dsp | SoundChipGpio | Parallel | Serial | External | Hamlib | Auto
            ),
            Self::Encoder => matches!(source, SoundChipGpio | Parallel | Serial),
        }
    }
    pub(super) fn serial_pins(self, raw: &Value) -> Option<Vec<u8>> {
        let pins = raw
            .as_array()?
            .iter()
            .map(|v| {
                let name = v.as_str()?;
                SERIAL_PINS
                    .iter()
                    .find(|(number, line)| {
                        *line == name
                            && if self.output() {
                                matches!(number, 4 | 7)
                            } else {
                                matches!(number, 1 | 6 | 8 | 9)
                            }
                    })
                    .map(|(number, _)| *number)
            })
            .collect::<Option<Vec<_>>>()?;
        (!pins.is_empty()).then_some(pins)
    }
    fn pins(self, source: SignalSource, raw: &Value, wiring: WiringProfile) -> Option<Vec<u8>> {
        if source == SignalSource::Serial {
            return self.serial_pins(raw);
        }
        let list = raw.as_array()?;
        let parse = |v: &Value| match source {
            SignalSource::Parallel => u8::try_from(v.as_u64()?).ok(),
            SignalSource::SoundChipGpio => v
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .or_else(|| v.as_str()?.parse().ok()),
            _ => None,
        };
        let pins = list.iter().map(parse).collect::<Option<Vec<_>>>()?;
        let valid = match source {
            SignalSource::Parallel => {
                pins.iter().all(|n| {
                    if self.output() {
                        (2..=9).contains(n)
                    } else {
                        [10, 12, 13, 15].contains(n)
                    }
                }) && (self != Self::Encoder || pins.len() == 1)
            }
            SignalSource::SoundChipGpio => {
                // These are the GPIO bits of the selected CM119 wiring profile.
                // The standard COR and tone indications are HID buttons, without GPIO numbers.
                let sph = wiring == WiringProfile::SphUsb;
                match (self, pins.as_slice()) {
                    (Self::Ptt, [n]) => *n == if sph { 4 } else { 3 },
                    (Self::Squelch, [3]) => sph,
                    (Self::Decode, [2]) => sph || wiring == WiringProfile::Custom,
                    (Self::Encoder, [n @ 1..=8]) => wiring.ordinary_mask() & (1 << (*n - 1)) != 0,
                    _ => false,
                }
            }
            _ => false,
        };
        valid.then_some(pins)
    }
}
pub(super) fn resolve(
    v: &Inputs<'_>,
    path: &[&str],
    usage: Use,
    default: Option<SignalSource>,
    interface: &super::super::Interface,
    w: &mut Vec<CandidateWarning>,
    site: &mut super::super::SitePrerequisites,
) -> Option<SignalBinding> {
    let mut selected = default.map(|source| SignalBinding {
        source,
        pins: Vec::new(),
        scancode: None,
        inverted: false,
    });
    for (object, owner) in v.values(path).collect::<Vec<_>>().into_iter().rev() {
        let src = object
            .get("source")
            .and_then(Value::as_str)
            .map(SignalSource::read);
        let old = selected.as_ref();
        let Some(source) = src.or_else(|| old.map(|b| b.source)) else {
            continue;
        };
        if !usage.allows(source) {
            warn(w, owner, "source");
            continue;
        }
        if source == SignalSource::Serial
            && interface.serial.device.is_none()
            && !site.defer(owner, "device")
        {
            warn(w, owner, "source");
            continue;
        }
        let same = old.filter(|b| b.source == source);
        let scancode = if source == SignalSource::KeyDown {
            if interface.keyboard_device.is_none() && !site.defer(owner, "device") {
                warn(w, owner, "source");
                continue;
            }
            let previous = same.and_then(|b| b.scancode);
            let code = if let Some(value) = object.get("scancode") {
                let next = value
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| (1..=0x2ff).contains(n));
                if next.is_none() {
                    warn(w, owner, "scancode");
                }
                next.or(previous)
            } else {
                previous
            };
            if code.is_none() && !site.defer(owner, "scancode") {
                warn(w, owner, "scancode");
                continue;
            }
            code
        } else {
            None
        };
        let mut pins = if let Some(pin) = object.get("pin") {
            if !matches!(
                source,
                SignalSource::Parallel | SignalSource::SoundChipGpio | SignalSource::Serial
            ) {
                Vec::new()
            } else if let Some(pins) = usage.pins(source, pin, interface.gpio.wiring_profile) {
                pins
            } else {
                warn(w, owner, "pin");
                let Some(previous) = same else { continue };
                previous.pins.clone()
            }
        } else {
            same.map_or_else(Vec::new, |b| b.pins.clone())
        };
        if source == SignalSource::Parallel && pins.is_empty() {
            use super::super::{ParallelInput, ParallelOutput};
            pins = match usage {
                Use::Ptt => interface
                    .parallel
                    .outputs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, m)| (*m == ParallelOutput::PushToTalk).then_some(i as u8 + 2))
                    .collect(),
                Use::Squelch | Use::Decode => [10, 12, 13, 15]
                    .into_iter()
                    .zip(interface.parallel.inputs)
                    .filter_map(|(n, m)| {
                        (m == if usage == Use::Squelch {
                            ParallelInput::Carrier
                        } else {
                            ParallelInput::Ctcss
                        })
                        .then_some(n)
                    })
                    .collect(),
                Use::Encoder => Vec::new(),
            };
        }
        if (usage == Use::Encoder
            || matches!(source, SignalSource::Parallel | SignalSource::Serial))
            && pins.is_empty()
            && !site.defer(owner, "pin")
        {
            warn(w, owner, "pin");
            continue;
        }
        selected = Some(SignalBinding {
            source,
            pins,
            scancode,
            inverted: object
                .get("inverted")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| old.is_some_and(|b| b.inverted)),
        });
    }
    selected
}
