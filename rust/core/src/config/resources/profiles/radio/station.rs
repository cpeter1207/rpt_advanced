//! Effective station controls and physical interface intentions.
use super::inputs::Inputs;
use serde_json::Value;
#[path = "station_fields.rs"]
mod fields;
pub(super) use fields::{GENERAL, INTERFACE};

choices!(/// One physical radio duplex policy.
DuplexMode{Half=>"half",Full=>"full"});
choices!(/// A provider audio output routing choice.
OutputRoute{Disabled=>"disabled",Voice=>"voice",Tone=>"tone",Composite=>"composite",AuxiliaryVoice=>"auxiliary_voice"});
choices!(/// Native sound-card selection policy.
DeviceSelection{Exact=>"exact",Automatic=>"automatic_lowest_alsa_card"});
choices!(/// Selected audio hardware capabilities.
AudioBackend{SoundChip=>"sound_chip",Alsa=>"alsa"});
choices!(/// CM119 provider wiring profile.
WiringProfile{DudeUsb=>"dudeusb",SphUsb=>"sphusb",Nhrc=>"nhrc",Custom=>"custom"});
choices!(/// Ordinary CM119 GPIO direction and initial output state.
GpioMode{Input=>"in",Low=>"out0",High=>"out1"});
choices!(/// Parallel connector output assignment.
ParallelOutput{Unassigned=>"",Low=>"out0",High=>"out1",PushToTalk=>"ptt"});
choices!(/// Parallel connector input assignment.
ParallelInput{Unassigned=>"",Input=>"in",Carrier=>"cor",Ctcss=>"ctcss"});
choices!(/// Serial line data parity.
SerialParity{None=>"none",Even=>"even",Odd=>"odd",Mark=>"mark",Space=>"space"});

/// General settings shared by the radio and its optional remote control.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioGeneral {
    /// Only physical duplex selector.
    pub duplex_mode: DuplexMode,
    /// Controller parrot playback policy.
    pub parrot_enabled: bool,
    /// Requested status publication interval.
    pub status_snapshot_interval_ms: u32,
    /// Selected startup memory.
    pub startup_memory: Option<u32>,
    /// Remote offset magnitude in exact hertz.
    pub offset_magnitude_hz: Option<u64>,
    /// Slow, medium, fast scan-delay presets.
    pub scan_presets_ms: [Option<u64>; 3],
    /// Maximum remote tuning carrier interval.
    pub tune_duration_ms: u32,
}
impl RadioGeneral {
    /// Whether to ask the physical radio to run full duplex.
    pub fn full_duplex(&self) -> bool {
        self.duplex_mode == DuplexMode::Full
    }
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            duplex_mode: DuplexMode::read(v.text(&["duplex_mode"], "half")),
            parrot_enabled: v.flag(&["parrot", "enabled"], false),
            status_snapshot_interval_ms: v.uint(&["timing", "status_snapshot_interval_ms"], 50),
            startup_memory: v
                .get(&["startup_memory"])
                .and_then(Value::as_u64)
                .map(|n| n as u32),
            offset_magnitude_hz: v
                .get(&["offset_magnitude_mhz"])
                .and_then(Value::as_number)
                .and_then(|n| crate::command_language::scaled(n.as_str(), 6))
                .map(|n| n as u64),
            scan_presets_ms: ["scan_slow_ms", "scan_medium_ms", "scan_fast_ms"]
                .map(|k| v.get(&["timing", k]).and_then(Value::as_u64)),
            tune_duration_ms: v.uint(&["timing", "tune_duration_ms"], 1000),
        }
    }
}

/// RX or TX emphasis filter intention.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Emphasis {
    /// Whether the filter is selected.
    pub enabled: bool,
    /// Filter corner in hertz.
    pub corner_hz: f64,
}
impl Emphasis {
    fn read(v: &Inputs<'_>, key: &str, enabled: bool) -> Self {
        Self {
            enabled: v.flag(&[key, "enabled"], enabled),
            corner_hz: v.number(&[key, "corner_hz"], 300.0),
        }
    }
}
/// Local receiver audio controls outside of any explicit processing role.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RxAudio {
    /// Independent receive programme enablement.
    pub enabled: bool,
    /// Software input gain.
    pub input_gain_db: f64,
    /// Software output gain.
    pub output_gain_db: f64,
    /// Discriminator de-emphasis.
    pub deemphasis: Emphasis,
}
impl RxAudio {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            enabled: v.flag(&["enabled"], true),
            input_gain_db: v.number(&["input_gain_db"], 0.0),
            output_gain_db: v.number(&["output_gain_db"], 0.0),
            deemphasis: Emphasis::read(v, "deemphasis", true),
        }
    }
}
/// Local transmitter programme controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TxAudio {
    /// Independent transmit programme enablement.
    pub enabled: bool,
    /// Voice pre-emphasis.
    pub preemphasis: Emphasis,
    /// Physical A and B routes.
    pub routes: [OutputRoute; 2],
}
impl TxAudio {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            enabled: v.flag(&["enabled"], true),
            preemphasis: Emphasis::read(v, "preemphasis", false),
            routes: [
                OutputRoute::read(v.text(&["output_a_assignment"], "voice")),
                OutputRoute::read(v.text(&["output_b_assignment"], "tone")),
            ],
        }
    }
}
/// Selected sound card, exact identifiers and physical mixer intentions.
#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceAudio {
    /// Backend controls available on the selected audio device.
    pub backend: AudioBackend,
    /// Device selection policy.
    pub selection: DeviceSelection,
    /// Unchanged UTF-8 device identifier.
    pub device_identifier: String,
    /// Unchanged USB serial number.
    pub usb_serial: String,
    /// Physical capture channel count.
    pub input_device_channels: u32,
    /// Physical playback channel count.
    pub output_device_channels: u32,
    /// Additional capture buffering.
    pub input_extra_buffer_ms: u32,
    /// Additional playback buffering.
    pub output_extra_buffer_ms: u32,
    /// Capture mixer gain; omission preserves hardware.
    pub input_gain_db: Option<f64>,
    /// Output A mixer gain; omission preserves hardware.
    pub output_a_gain_db: Option<f64>,
    /// Output B mixer gain; omission preserves hardware.
    pub output_b_gain_db: Option<f64>,
    /// EEPROM tuning enabled.
    pub eeprom_enabled: bool,
}
impl InterfaceAudio {
    /// Recognize the physical provider's exact decimal ALSA card or card/PCM selector.
    pub(super) fn exact_alsa_pcm(&self) -> bool {
        if self.backend != AudioBackend::Alsa || self.selection != DeviceSelection::Exact {
            return false;
        }
        let Some(address) = self.device_identifier.strip_prefix("hw:") else {
            return false;
        };
        let number = |s: &str| {
            !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && s.parse::<u32>().is_ok()
        };
        match address.split_once(',') {
            Some((card, pcm)) => number(card) && number(pcm),
            None => number(address),
        }
    }

    fn read(v: &Inputs<'_>) -> Self {
        let gain = |key| v.get(&["audio", key]).and_then(Value::as_f64);
        Self {
            backend: AudioBackend::read(v.text(&["audio", "backend"], "sound_chip")),
            selection: DeviceSelection::read(v.text(&["audio", "device_selection"], "exact")),
            device_identifier: v.text(&["audio", "device_identifier"], "").into(),
            usb_serial: v.text(&["audio", "usb_serial"], "").into(),
            input_device_channels: v.uint(&["audio", "input_device_channels"], 1),
            output_device_channels: v.uint(&["audio", "output_device_channels"], 1),
            input_extra_buffer_ms: v.uint(&["audio", "timing", "input_extra_buffer_ms"], 0),
            output_extra_buffer_ms: v.uint(&["audio", "timing", "output_extra_buffer_ms"], 0),
            input_gain_db: gain("input_gain_db"),
            output_a_gain_db: gain("output_a_gain_db"),
            output_b_gain_db: gain("output_b_gain_db"),
            eeprom_enabled: v.flag(&["audio", "eeprom_enabled"], false),
        }
    }
}
/// CM119 wiring and ordinary pin intentions.
#[derive(Clone, Debug, PartialEq)]
pub struct Gpio {
    /// Profile chooses dedicated carrier, tone and PTT functions.
    pub wiring_profile: WiringProfile,
    /// Ordinary pins 1 through 8, in physical order.
    pub pins: [GpioMode; 8],
    /// Optional one-based clipping-indicator pin.
    pub clip_led_pin: Option<u8>,
}
/// Parallel-port layout, keeping connector pin order.
#[derive(Clone, Debug, PartialEq)]
pub struct Parallel {
    /// Parallel device as supplied.
    pub device: String,
    /// Hardware I/O base address.
    pub base_address: u32,
    /// Output pins 2 through 9.
    pub outputs: [ParallelOutput; 8],
    /// Input pins 10, 12, 13, 15.
    pub inputs: [ParallelInput; 4],
}
/// Authored serial endpoint settings. Acquisition still requires a signaling backend.
#[derive(Clone, Debug, PartialEq)]
pub struct Serial {
    /// Optional explicit device path.
    pub device: Option<String>,
    /// Nominal line rate.
    pub baud_rate: u32,
    /// Data bits per character.
    pub data_bits: u8,
    /// Selected parity.
    pub parity: SerialParity,
    /// Stop bits per character.
    pub stop_bits: f64,
}
/// Physical interface settings kept independently from hardware acquisition.
#[derive(Clone, Debug, PartialEq)]
pub struct Interface {
    /// Audio selectors and gains.
    pub audio: InterfaceAudio,
    /// Sound-chip assignments.
    pub gpio: Gpio,
    /// Parallel-port configuration.
    pub parallel: Parallel,
    /// Serial endpoint configuration.
    pub serial: Serial,
    /// Explicit Linux evdev keyboard observation endpoint.
    pub keyboard_device: Option<String>,
}
impl WiringProfile {
    /// Return whether a one-based CM119 pin is available for ordinary wiring on this profile.
    pub fn ordinary_pin(self, pin: u8) -> bool {
        (1..=8).contains(&pin) && self.ordinary_mask() & (1 << (pin - 1)) != 0
    }

    pub(super) fn ordinary_mask(self) -> u8 {
        match self {
            Self::DudeUsb => 0xfb,
            Self::SphUsb | Self::Custom => 0x01,
            Self::Nhrc => 0,
        }
    }
}
impl Gpio {
    fn read(
        v: &Inputs<'_>,
        w: &mut Vec<super::CandidateWarning>,
        site: &mut super::SitePrerequisites,
    ) -> Self {
        let wiring_profile = WiringProfile::read(v.text(&["gpio", "wiring_profile"], "dudeusb"));
        let keys = ["1", "2", "3", "4", "5", "6", "7", "8"];
        let pins = std::array::from_fn(|i| {
            v.values(&["gpio", "pins", keys[i]])
                .find_map(|(value, b)| {
                    let mode = GpioMode::read(value.as_str()?);
                    if mode != GpioMode::Input && wiring_profile.ordinary_mask() & (1 << i) == 0 {
                        super::super::normalize::warn(w, b, "pins");
                        None
                    } else {
                        Some(mode)
                    }
                })
                .unwrap_or(GpioMode::Input)
        });
        // Explicit null clears an inherited indicator. Bad non-null claims can fall back.
        let clip_led_pin = v
            .values(&["gpio", "clip_led_pin"])
            .find_map(|(value, b)| {
                if value.is_null() {
                    return Some(None);
                }
                let n = value.as_u64()? as u8;
                if pins[usize::from(n - 1)] != GpioMode::Input || site.defer(b, "clip_led_pin") {
                    Some(Some(n))
                } else {
                    super::super::normalize::warn(w, b, "clip_led_pin");
                    None
                }
            })
            .flatten();
        Self {
            wiring_profile,
            pins,
            clip_led_pin,
        }
    }
}
impl Interface {
    pub(super) fn read(
        v: &Inputs<'_>,
        w: &mut Vec<super::CandidateWarning>,
        site: &mut super::SitePrerequisites,
    ) -> Self {
        let choices = ["data_bits", "stop_bits"].map(|field| {
            v.values(&["serial", field])
                .map(|(value, source)| {
                    (
                        value.as_f64().expect("normalized serial framing"),
                        Some(source),
                    )
                })
                .collect()
        });
        let [data_bits, stop_bits] = super::inputs::choose_pair(
            choices,
            [8.0, 1.0],
            ["data_bits", "stop_bits"],
            |bits, stops| {
                stops == 1.0 || (bits == 5.0 && stops == 1.5) || (bits >= 6.0 && stops == 2.0)
            },
            w,
        );
        Self {
            audio: InterfaceAudio::read(v),
            gpio: Gpio::read(v, w, site),
            parallel: Parallel {
                device: v.text(&["parallel", "device"], "/dev/parport0").into(),
                base_address: v.uint(&["parallel", "base_address"], 888),
                outputs: ["2", "3", "4", "5", "6", "7", "8", "9"]
                    .map(|k| ParallelOutput::read(v.text(&["parallel", "outputs", k], ""))),
                inputs: ["10", "12", "13", "15"]
                    .map(|k| ParallelInput::read(v.text(&["parallel", "inputs", k], ""))),
            },
            keyboard_device: v
                .get(&["keyboard", "device"])
                .and_then(Value::as_str)
                .map(str::to_owned),
            serial: Serial {
                device: v
                    .get(&["serial", "device"])
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                baud_rate: v.uint(&["serial", "baud_rate"], 9600),
                data_bits: data_bits as u8,
                parity: SerialParity::read(v.text(&["serial", "parity"], "none")),
                stop_bits,
            },
        }
    }
}
