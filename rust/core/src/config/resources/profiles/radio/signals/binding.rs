//! Resolve source-dependent controls together after inherited fields are available.
use super::{CandidateWarning, Inputs, SignalSource, WiringProfile, warn};
use serde_json::Value;

/// Resolved signal observation or actuator and its polarity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalBinding {
    /// Selected signal owner.
    pub source: SignalSource,
    /// One-based GPIO or parallel connector pin numbers. Empty selects a dedicated provider signal.
    pub pins: Vec<u8>,
    /// Applied independently to each raw observation or driven output.
    pub inverted: bool,
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
            Self::Squelch => matches!(source, Dsp | SoundChipGpio | Parallel),
            Self::Ptt => matches!(source, SoundChipGpio | Parallel | Hamlib),
            Self::Decode => matches!(
                source,
                Dsp | SoundChipGpio | Parallel | External | Hamlib | Auto
            ),
            Self::Encoder => matches!(source, SoundChipGpio | Parallel),
        }
    }
    fn pins(self, source: SignalSource, raw: &Value, wiring: WiringProfile) -> Option<Vec<u8>> {
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
) -> Option<SignalBinding> {
    let mut selected = default.map(|source| SignalBinding {
        source,
        pins: Vec::new(),
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
        let same = old.filter(|b| b.source == source);
        let mut pins = if let Some(pin) = object.get("pin") {
            if !matches!(source, SignalSource::Parallel | SignalSource::SoundChipGpio) {
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
        if (usage == Use::Encoder || source == SignalSource::Parallel) && pins.is_empty() {
            warn(w, owner, "pin");
            continue;
        }
        selected = Some(SignalBinding {
            source,
            pins,
            inverted: object
                .get("inverted")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| old.is_some_and(|b| b.inverted)),
        });
    }
    selected
}
