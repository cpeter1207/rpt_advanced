//! Carrier, VAD and subaudible qualifiers; independent transmit timing and signaling.
mod binding;
pub(super) mod fields;
use super::super::normalize::warn;
use super::{CandidateWarning, RadioProfileError, ResourceScope, WiringProfile, inputs::Inputs};
pub use binding::SignalBinding;
use binding::Use;
use serde_json::Value;

choices!(/// Source of a carrier, tone, code, or push-to-talk signal.
SignalSource{Dsp=>"dsp",SoundChipGpio=>"sound_chip_gpio",Parallel=>"parallel",Serial=>"serial",KeyDown=>"key_down",External=>"external",Hamlib=>"hamlib",Auto=>"auto"});
choices!(/// Carrier noise-filter response.
NoiseFilter{Standard=>"standard",Alternate=>"alternate"});
choices!(/// Speech qualification policy.
VadMode{Kerchunk=>"kerchunk",Continuous=>"continuous"});
choices!(/// CTCSS transmitter tail.
TurnoffMode{None=>"none",PhaseShift=>"phase_shift",ToneRemove=>"tone_remove",TailTone=>"tail_tone"});

/// Receiver carrier qualification.
#[derive(Clone, Debug, PartialEq)]
pub struct Squelch {
    /// Carrier is an active receive qualifier.
    pub enabled: bool,
    /// Carrier source, pins and per-pin polarity.
    pub binding: SignalBinding,
    /// Operator noise squelch level (0 through 999).
    pub level: u32,
    /// DSP hysteresis.
    pub hysteresis: u32,
    /// DSP noise-filter response.
    pub noise_filter: NoiseFilter,
}
/// Ordered tone definitions and independently selected operating frequency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tones {
    /// Decoder or encoder frequencies in tenths of a hertz.
    pub ordered: Vec<u16>,
    /// Operating frequency in tenths of a hertz.
    pub frequency: u16,
}
impl Tones {
    fn read(v: &Inputs<'_>, dsp: bool, receive: bool, w: &mut Vec<CandidateWarning>) -> Self {
        let frequency = v
            .values(&["ctcss", "frequency_hz"])
            .find_map(|(f, b)| {
                let hz = fields::tenths(f)?;
                if dsp && !fields::DSP_TONES.contains(&hz) {
                    warn(w, b, "frequency_hz");
                    None
                } else {
                    Some(hz)
                }
            })
            .unwrap_or(1000);
        let ordered = v
            .get(&["ctcss", "tones_hz"])
            .and_then(Value::as_array)
            .map_or_else(
                || vec![if receive && dsp { frequency } else { 1000 }],
                |a| a.iter().filter_map(fields::tenths).collect(),
            );
        Self { ordered, frequency }
    }
}
/// On-air nine-bit octal DCS code and polarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigitalCode {
    /// Numeric nine-bit codeword.
    pub code: u16,
    /// Inverted DCS polarity.
    pub inverted: bool,
}
impl DigitalCode {
    fn read(v: &Inputs<'_>) -> Self {
        let raw = v.text(&["dcs", "code"], "023N");
        Self {
            code: u16::from_str_radix(&raw[..3], 8).expect("validated DCS"),
            inverted: raw.ends_with('I'),
        }
    }
}
/// Receiver CTCSS decoder and qualification.
#[derive(Clone, Debug, PartialEq)]
pub struct RxCtcss {
    /// Use this qualification.
    pub enabled: bool,
    /// Source and physical indication polarity.
    pub binding: SignalBinding,
    /// Operating and programmed tones.
    pub tones: Tones,
    /// Decoder gain in dB.
    pub gain_db: f64,
    /// Relaxed tone matching.
    pub relaxed: bool,
    /// Decoder override.
    pub override_enabled: bool,
}
/// Receiver DCS selection.
#[derive(Clone, Debug, PartialEq)]
pub struct RxDcs {
    /// Use this qualification.
    pub enabled: bool,
    /// Decode owner.
    pub source: SignalSource,
    /// Code and polarity.
    pub code: DigitalCode,
}
/// Receiver qualifiers and associated delays.
#[derive(Clone, Debug, PartialEq)]
pub struct RxSignals {
    /// Carrier qualification.
    pub squelch: Squelch,
    /// Speech qualification.
    pub vad_enabled: bool,
    /// Speech mode.
    pub vad_mode: VadMode,
    /// Hold after speech.
    pub vad_hang_ms: u32,
    /// CTCSS qualification.
    pub ctcss: RxCtcss,
    /// DCS qualification.
    pub dcs: RxDcs,
    /// Delay before opening receive in 20-ms blocks.
    pub on_delay_blocks: u32,
    /// DSP squelch delay.
    pub squelch_delay_ms: u32,
    /// Maximum kerchunk length.
    pub kerchunk_max_ms: u32,
}
/// Transmit CTCSS program.
#[derive(Clone, Debug, PartialEq)]
pub struct TxCtcss {
    /// Enable transmit programming.
    pub enabled: bool,
    /// Encoder owner.
    pub source: SignalSource,
    /// Optional external encoder output.
    pub output: Option<SignalBinding>,
    /// Ordered and selected tones.
    pub tones: Tones,
    /// Generator level.
    pub level_dbfs: f64,
    /// Turnoff sequence.
    pub turnoff: TurnoffMode,
    /// Turnoff phase shift in degrees.
    pub phase_shift_degrees: f64,
    /// Tail tone frequency in hertz.
    pub tail_tone_hz: f64,
    /// Turnoff interval.
    pub duration_ms: u32,
    /// Gate generator on qualified receive.
    pub on_input: bool,
    /// Post-activity tone interval.
    pub hang_ms: u32,
}
/// Transmit DCS program.
#[derive(Clone, Debug, PartialEq)]
pub struct TxDcs {
    /// Enable transmit programming.
    pub enabled: bool,
    /// Encoder owner.
    pub source: SignalSource,
    /// Optional external encoder output.
    pub output: Option<SignalBinding>,
    /// Code and polarity.
    pub code: DigitalCode,
    /// Generator level.
    pub level_dbfs: f64,
    /// Emit a turnoff sequence.
    pub turnoff_enabled: bool,
    /// Turnoff interval.
    pub duration_ms: u32,
}
/// PTT binding, watchdog and transmitter encoders.
#[derive(Clone, Debug, PartialEq)]
pub struct TxSignals {
    /// Logical PTT actuator.
    pub ptt: SignalBinding,
    /// Key-up settling time.
    pub settle_ms: u32,
    /// Key-down tail in 20-ms blocks.
    pub off_delay_blocks: u32,
    /// Transmitter activity hang.
    pub hang_ms: u32,
    /// Continuous-source watchdog.
    pub timeout_ms: u32,
    /// Blanking of receiver after transmit.
    pub rx_blanking_ms: u32,
    /// CTCSS encoder.
    pub ctcss: TxCtcss,
    /// DCS encoder.
    pub dcs: TxDcs,
}
/// Effective receive/transmit signaling controls.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioSignals {
    /// Receiver signaling.
    pub rx: RxSignals,
    /// Transmitter signaling.
    pub tx: TxSignals,
}
fn enabled_pair(v: &Inputs<'_>) -> Result<[bool; 2], RadioProfileError> {
    let enabled = ["ctcss", "dcs"].map(|name| {
        v.values(&[name, "enabled"])
            .next()
            .filter(|(flag, _)| flag.as_bool() == Some(true))
            .map(|(_, owner)| owner)
    });
    if let [Some(c), Some(d)] = enabled {
        let (resource, other) = if matches!(d.scope, ResourceScope::Port(_)) {
            (d, c)
        } else {
            (c, d)
        };
        return Err(RadioProfileError {
            resource,
            conflicting: (resource != other).then_some(other),
            field: "ctcss_dcs",
        });
    }
    Ok(enabled.map(|owner| owner.is_some()))
}
impl RxSignals {
    fn read(
        v: &Inputs<'_>,
        wiring: &super::Interface,
        [ctcss, dcs]: [bool; 2],
        w: &mut Vec<CandidateWarning>,
    ) -> Self {
        let squelch = Squelch {
            enabled: v.flag(&["squelch", "enabled"], true),
            binding: binding::resolve(
                v,
                &["squelch"],
                Use::Squelch,
                Some(SignalSource::Dsp),
                wiring,
                w,
            )
            .expect("built-in squelch"),
            level: v.uint(&["squelch", "level"], 500),
            hysteresis: v.uint(&["squelch", "hysteresis"], 3000),
            noise_filter: NoiseFilter::read(v.text(&["squelch", "noise_filter"], "standard")),
        };
        let vad_enabled = v.flag(&["vad", "enabled"], false);
        let vad_mode = v
            .values(&["vad", "mode"])
            .find_map(|(raw, b)| {
                let mode = VadMode::read(raw.as_str()?);
                if mode == VadMode::Kerchunk && vad_enabled && !squelch.enabled && !ctcss && !dcs {
                    warn(w, b, "mode");
                    None
                } else {
                    Some(mode)
                }
            })
            .unwrap_or(VadMode::Continuous);
        let binding = binding::resolve(
            v,
            &["ctcss"],
            Use::Decode,
            Some(SignalSource::Dsp),
            wiring,
            w,
        )
        .expect("built-in decoder");
        Self {
            squelch,
            vad_enabled,
            vad_mode,
            vad_hang_ms: v.uint(&["vad", "timing", "hang_ms"], 1000),
            ctcss: RxCtcss {
                enabled: ctcss,
                tones: Tones::read(v, ctcss && binding.source == SignalSource::Dsp, true, w),
                binding,
                gain_db: v.number(&["ctcss", "gain_db"], 0.0),
                relaxed: v.flag(&["ctcss", "relaxed"], true),
                override_enabled: v.flag(&["ctcss", "override"], false),
            },
            dcs: RxDcs {
                enabled: dcs,
                source: SignalSource::read(v.text(&["dcs", "source"], "dsp")),
                code: DigitalCode::read(v),
            },
            on_delay_blocks: v.uint(&["timing", "on_delay_ms"], 0).div_ceil(20),
            squelch_delay_ms: v.uint(&["timing", "squelch_delay_ms"], 0),
            kerchunk_max_ms: v.uint(&["timing", "kerchunk_max_ms"], 500),
        }
    }
}
impl TxSignals {
    fn read(
        v: &Inputs<'_>,
        wiring: &super::Interface,
        [ctcss, dcs]: [bool; 2],
        w: &mut Vec<CandidateWarning>,
    ) -> Self {
        let hang_ms = v.uint(&["ptt", "timing", "hang_ms"], 0);
        let tone_hang = v
            .values(&["ctcss", "timing", "hang_ms"])
            .find_map(|(value, b)| {
                let n = u32::try_from(value.as_u64()?).ok()?;
                if n > hang_ms {
                    warn(w, b, "hang_ms");
                    None
                } else {
                    Some(n)
                }
            })
            .unwrap_or(0);
        let source = SignalSource::read(v.text(&["ctcss", "source"], "dsp"));
        let ptt = binding::resolve(
            v,
            &["ptt"],
            Use::Ptt,
            Some(SignalSource::SoundChipGpio),
            wiring,
            w,
        )
        .expect("built-in PTT");
        Self {
            ptt,
            settle_ms: v.uint(&["ptt", "timing", "settle_ms"], 500),
            off_delay_blocks: v.uint(&["ptt", "timing", "off_delay_ms"], 0).div_ceil(20),
            hang_ms,
            timeout_ms: v.uint(&["ptt", "timing", "timeout_ms"], 180000),
            rx_blanking_ms: v.uint(&["ptt", "timing", "rx_blanking_ms"], 0),
            ctcss: TxCtcss {
                enabled: ctcss,
                source,
                tones: Tones::read(v, source == SignalSource::Dsp, false, w),
                output: binding::resolve(v, &["ctcss", "output"], Use::Encoder, None, wiring, w),
                level_dbfs: v.number(&["ctcss", "level_dbfs"], -24.0),
                turnoff: TurnoffMode::read(v.text(&["ctcss", "turnoff", "mode"], "phase_shift")),
                phase_shift_degrees: v.number(&["ctcss", "turnoff", "phase_shift_degrees"], 120.0),
                tail_tone_hz: v.number(&["ctcss", "turnoff", "tail_tone_hz"], 55.0),
                duration_ms: v.uint(&["ctcss", "turnoff", "timing", "duration_ms"], 180),
                on_input: v.flag(&["ctcss", "on_input"], false),
                hang_ms: tone_hang,
            },
            dcs: TxDcs {
                enabled: dcs,
                source: SignalSource::read(v.text(&["dcs", "source"], "dsp")),
                output: binding::resolve(v, &["dcs", "output"], Use::Encoder, None, wiring, w),
                code: DigitalCode::read(v),
                level_dbfs: v.number(&["dcs", "level_dbfs"], -24.0),
                turnoff_enabled: v.flag(&["dcs", "turnoff", "enabled"], true),
                duration_ms: v.uint(&["dcs", "turnoff", "timing", "duration_ms"], 180),
            },
        }
    }
}
impl RadioSignals {
    pub(super) fn read(
        rx: &Inputs<'_>,
        tx: &Inputs<'_>,
        wiring: &super::Interface,
        w: &mut Vec<CandidateWarning>,
    ) -> Result<Self, RadioProfileError> {
        let mut signals = Self {
            rx: RxSignals::read(rx, wiring, enabled_pair(rx)?, w),
            tx: TxSignals::read(tx, wiring, enabled_pair(tx)?, w),
        };
        let use_rx_operating_tone =
            signals.rx.ctcss.enabled && signals.rx.ctcss.binding.source == SignalSource::Dsp;
        if signals.rx.ctcss.enabled
            && signals.tx.ctcss.enabled
            && signals.rx.ctcss.tones.ordered.len() != signals.tx.ctcss.tones.ordered.len()
        {
            let choices = [rx, tx].map(|inputs| {
                inputs
                    .values(&["ctcss", "tones_hz"])
                    .filter_map(|(v, b)| Some((Some(v.as_array()?.as_slice()), Some(b))))
                    .collect()
            });
            let [receive, transmit] = super::inputs::choose_pair(
                choices,
                [None, None],
                ["tones_hz"; 2],
                |a, b| a.map_or(1, <[Value]>::len) == b.map_or(1, <[Value]>::len),
                w,
            );
            let operating = if use_rx_operating_tone {
                signals.rx.ctcss.tones.frequency
            } else {
                1000
            };
            for (tones, selected, default) in [
                (&mut signals.rx.ctcss.tones, receive, operating),
                (&mut signals.tx.ctcss.tones, transmit, 1000),
            ] {
                tones.ordered = selected.map_or_else(
                    || vec![default],
                    |values| values.iter().filter_map(fields::tenths).collect(),
                );
            }
        }
        let decoder = &signals.rx.ctcss;
        if use_rx_operating_tone
            && decoder.tones.ordered != [decoder.tones.frequency]
            && let Some((_, resource)) = rx
                .values(&["ctcss", "frequency_hz"])
                .find(|(v, _)| fields::tenths(v) == Some(decoder.tones.frequency))
        {
            return Err(RadioProfileError {
                resource,
                conflicting: None,
                field: "frequency_hz",
            });
        }
        Ok(signals)
    }
}

impl Tones {
    /// Index of a supported on-air tone in the released native decoder mask and mapping table.
    pub fn native_index(tenths_hz: u16) -> Option<usize> {
        fields::DSP_TONES.iter().position(|&tone| tone == tenths_hz)
    }
}

/// Clean controls of a replaced discriminator only after receiving the complete authored object.
pub(super) fn normalize(
    b: super::ResourceBinding,
    source: &serde_json::Map<String, Value>,
    w: &mut Vec<CandidateWarning>,
) -> serde_json::Map<String, Value> {
    let schema = if b.kind == super::ResourceKind::RxSignaling {
        fields::RX
    } else {
        fields::TX
    };
    let mut map = super::super::normalize::normalize(b, source, schema, w);
    fn clean(v: Option<&mut Value>) {
        let Some(body) = v.and_then(Value::as_object_mut) else {
            return;
        };
        let Some(source) = body.get("source").and_then(Value::as_str) else {
            return;
        };
        let use_pin = matches!(source, "sound_chip_gpio" | "parallel" | "serial");
        let use_key = source == "key_down";
        if !use_pin {
            body.remove("pin");
        }
        if !use_key {
            body.remove("scancode");
        }
    }
    for key in ["squelch", "ptt", "ctcss"] {
        clean(map.get_mut(key))
    }
    for key in ["ctcss", "dcs"] {
        clean(map.get_mut(key).and_then(|v| v.get_mut("output")))
    }
    map
}
