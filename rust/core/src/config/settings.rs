//! Owned node settings and inherited default resolution.

use super::{ConfigDocument, ConfigError, Resolution, Schema, parse};
use crate::access::AccessPolicy;
use crate::command::{CommandMapping, DtmfCommandMap, LinkAction};
use crate::schedule::{date_selector_valid, weekday_selector_valid};
use std::collections::BTreeMap;

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;

/// Directory lookup source selected after the local static directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkLookupMethod {
    /// Query DNS and then the external file.
    #[default]
    Both,
    /// Query DNS only.
    Dns,
    /// Query the external file only.
    File,
}

/// Fully owned node settings after general and node-scoped inheritance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedNodeSettings {
    /// Whether the node starts.
    pub enabled: bool,
    /// Whether local receive may transmit concurrently.
    pub full_duplex: bool,
    /// Whether completed local DTMF frames are muted.
    pub dtmf_muting: bool,
    /// Whether this node records and replays receive bursts after unkey.
    pub parrot_enabled: bool,
    /// Extra receive-to-transmit delay used for squelch-tail and DTMF lookback.
    pub squelch_delay_ms: u64,
    /// Interval for publishing receive/transmit meter snapshots.
    pub status_snapshot_interval_ms: u64,
    /// Transmit hang duration in milliseconds.
    pub hang_ms: u64,
    /// Enable CTCSS only for received traffic or command-response telemetry.
    pub ctcss_encode_on_input: bool,
    /// Received-traffic CTCSS hang in milliseconds, no longer than transmit hang.
    pub ctcss_hang_ms: u64,
    /// Continuous-source watchdog duration; zero disables it.
    pub transmit_timeout_ms: u64,
    /// Post-watchdog lockout duration.
    pub timeout_lockout_ms: u64,
    /// Kerchunk threshold in milliseconds.
    pub kerchunk_max_ms: u64,
    /// Receive-active telemetry attenuation in dB.
    pub telemetry_duck_db: i64,
    /// Unkey-to-courtesy delay in milliseconds.
    pub courtesy_delay_ms: u64,
    /// Node radio channel.
    pub channel: String,
    /// Standalone radio-device and processing configuration.
    pub radio: ResolvedRadioSettings,
    /// Selected Fluent locale, inherited from general settings.
    pub language: String,
    /// Optional callsign.
    pub callsign: String,
    /// Optional AllStarLink-compatible status reporting URL; empty disables reporting.
    pub statpost_url: String,
    /// Status reporting interval in seconds.
    pub statpost_time: u64,
    /// ASL3 HTTPS node-registration endpoint; empty disables registration.
    pub iax_registration_url: String,
    /// HTTPS registration refresh interval in seconds.
    pub iax_registration_interval_s: u64,
    /// Locally advertised IAX2 UDP port.
    pub iax_local_port: u16,
    /// Incoming allowlist.
    pub link_allow_nodes: String,
    /// Incoming denylist.
    pub link_deny_nodes: String,
    /// Optional local static directory path.
    pub link_static_directory_file: String,
    /// Optional external directory path.
    pub link_directory_file: String,
    /// Directory lookup policy.
    pub link_lookup_method: LinkLookupMethod,
    /// Configured DTMF command prefixes by option name.
    pub link_commands: BTreeMap<String, String>,
    /// Optional PHC-encoded Argon2id digest required to unlock DTMF administration.
    pub dtmf_admin_unlock_hash: String,
    /// Optional PHC-encoded Argon2id digest required to lock DTMF administration.
    pub dtmf_admin_lock_hash: String,
    /// Idle timeout for an unlocked DTMF administration session.
    pub dtmf_admin_timeout_ms: u64,
}

/// USB audio device selection policy supported by the PortAudio adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioDeviceSelection {
    /// Require the configured stable device identity.
    #[default]
    Exact,
    /// Select the lowest-index usable physical USB audio device.
    AutomaticLowestAlsaCard,
}

/// CM119 interface wiring profile passed to the GPIO hardware adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cm119Profile {
    /// Standard DudeUSB/URI wiring.
    #[default]
    DudeUsb,
    /// SPH USB interface wiring.
    SphUsb,
    /// NHRC/N1KDO interface wiring.
    Nhrc,
    /// Existing custom USBRadioPlus wiring.
    Custom,
}

/// Configured state for one ordinary CM119 GPIO.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cm119GpioMode {
    /// Use the pin as an input.
    #[default]
    Input,
    /// Drive the pin low when the interface opens.
    OutputLow,
    /// Drive the pin high when the interface opens.
    OutputHigh,
}

/// Radio signaling method selected for reception or transmission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioSignalingMode {
    /// Use carrier qualification or transmit no subaudible signal.
    #[default]
    Carrier,
    /// Decode or transmit CTCSS.
    Ctcss,
    /// Decode or transmit DCS.
    Dcs,
}

/// Audio source used by the local receiver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioReceiveAudioSource {
    /// Do not pass receiver audio.
    Disabled,
    /// Use speaker audio.
    Speaker,
    /// Use flat discriminator audio and apply configured deemphasis.
    #[default]
    Flat,
}

/// Carrier-operated-squelch source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioCarrierSource {
    /// Do not qualify on carrier.
    Disabled,
    /// Use the native discriminator-noise detector.
    #[default]
    Dsp,
    /// Use voice-activity thresholding.
    Vox,
    /// Use normal-polarity CM119 GPIO.
    Cm119,
    /// Use inverted CM119 GPIO.
    Cm119Inverted,
    /// Use normal-polarity parallel input.
    Parallel,
    /// Use inverted parallel input.
    ParallelInverted,
}

/// Source of receive CTCSS/DCS qualification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioSubaudibleSource {
    /// Do not use external tone/code qualification.
    Disabled,
    /// Use normal-polarity CM119 GPIO.
    Cm119,
    /// Use inverted CM119 GPIO.
    Cm119Inverted,
    /// Use the native DSP decoder.
    #[default]
    Dsp,
    /// Use normal-polarity parallel input.
    Parallel,
    /// Use inverted parallel input.
    ParallelInverted,
}

/// Native noise squelch detector response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioNoiseFilter {
    /// Standard discriminator-noise response.
    #[default]
    Standard,
    /// Alternate discriminator-noise response.
    Alternate,
}

/// Radio duplex capability, independent of controller link duplex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioDuplexMode {
    /// Radio cannot receive while transmitting.
    #[default]
    Half,
    /// Radio can receive while transmitting.
    Full,
}

/// CTCSS action before transmitter unkey.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CtcssTurnoffMode {
    /// End CTCSS with transmitter release.
    None,
    /// Apply a phase-shift reverse burst.
    #[default]
    PhaseShift,
    /// Remove the tone before transmitter release.
    ToneRemove,
    /// Send the configured tail tone before release.
    TailTone,
}

/// DCS code value and polarity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DcsCode {
    /// Numeric three-digit octal value.
    pub value: u16,
    /// Whether inverse polarity is selected.
    pub inverted: bool,
}

/// Receive and transmit signaling settings after `[radio]` inheritance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRadioSignalingSettings {
    /// Local receiver audio source.
    pub receive_audio_source: RadioReceiveAudioSource,
    /// Receive qualification mode.
    pub receive_mode: RadioSignalingMode,
    /// Carrier qualification source.
    pub carrier_source: RadioCarrierSource,
    /// CTCSS/DCS external indication source.
    pub receive_subaudible_source: RadioSubaudibleSource,
    /// Supported receive CTCSS tones in tenths of a hertz.
    pub receive_ctcss_tones_tenths_hz: Vec<u16>,
    /// Receive CTCSS decoder gain in dB.
    pub receive_ctcss_decoder_gain_db: i64,
    /// Use relaxed native CTCSS decode tolerance.
    pub receive_ctcss_relaxed: bool,
    /// Permit carrier without a decoded CTCSS tone when override is active.
    pub receive_ctcss_override: bool,
    /// Receive DCS code.
    pub receive_dcs_code: DcsCode,
    /// DSP squelch threshold on the 0–999 tuning scale.
    pub squelch_level: u16,
    /// DSP squelch closing hysteresis in PCM codes.
    pub squelch_hysteresis: u16,
    /// DSP noise-filter response.
    pub noise_filter: RadioNoiseFilter,
    /// VOX threshold in PCM codes.
    pub vox_threshold: u16,
    /// VOX hang time in milliseconds.
    pub vox_hang_ms: u32,
    /// Receiver carrier qualification delay in milliseconds.
    pub receive_on_delay_ms: u32,
    /// Whether the radio supports full-duplex operation.
    pub radio_duplex_mode: RadioDuplexMode,
    /// Transmit signaling mode.
    pub transmit_mode: RadioSignalingMode,
    /// Transmitted CTCSS tone-map values in tenths of a hertz.
    pub transmit_ctcss_tones_tenths_hz: Vec<u16>,
    /// Default transmitted CTCSS tone in tenths of a hertz.
    pub transmit_ctcss_default_tenths_hz: u16,
    /// Transmitted CTCSS peak in dBFS.
    pub transmit_ctcss_level_dbfs: i64,
    /// CTCSS turnoff mode.
    pub transmit_ctcss_turnoff_mode: CtcssTurnoffMode,
    /// CTCSS turnoff phase shift in degrees.
    pub transmit_ctcss_phase_shift_degrees: u16,
    /// CTCSS turnoff duration in milliseconds.
    pub transmit_ctcss_turnoff_duration_ms: u32,
    /// Replacement CTCSS tail-tone frequency in hertz.
    pub transmit_ctcss_tail_tone_hz: u16,
    /// Transmit DCS code.
    pub transmit_dcs_code: DcsCode,
    /// Transmit DCS peak in dBFS.
    pub transmit_dcs_level_dbfs: i64,
    /// Whether to send DCS turnoff code.
    pub transmit_dcs_turnoff_enabled: bool,
    /// DCS turnoff duration in milliseconds.
    pub transmit_dcs_turnoff_duration_ms: u32,
    /// Delay between PTT assertion and program audio in milliseconds.
    pub transmit_settle_ms: u32,
    /// Receive blanking interval during transmit in milliseconds.
    pub transmit_receive_blanking_ms: u32,
    /// Receiver ignore interval after transmit in milliseconds.
    pub transmit_off_delay_ms: u32,
}

impl Default for ResolvedRadioSignalingSettings {
    fn default() -> Self {
        let tone = 1_000;
        let dcs = DcsCode {
            value: 0o23,
            inverted: false,
        };
        Self {
            receive_audio_source: RadioReceiveAudioSource::Flat,
            receive_mode: RadioSignalingMode::Carrier,
            carrier_source: RadioCarrierSource::Dsp,
            receive_subaudible_source: RadioSubaudibleSource::Dsp,
            receive_ctcss_tones_tenths_hz: vec![tone],
            receive_ctcss_decoder_gain_db: 0,
            receive_ctcss_relaxed: true,
            receive_ctcss_override: false,
            receive_dcs_code: dcs,
            squelch_level: 500,
            squelch_hysteresis: 3_000,
            noise_filter: RadioNoiseFilter::Standard,
            vox_threshold: 0,
            vox_hang_ms: 2_000,
            receive_on_delay_ms: 0,
            radio_duplex_mode: RadioDuplexMode::Half,
            transmit_mode: RadioSignalingMode::Carrier,
            transmit_ctcss_tones_tenths_hz: vec![tone],
            transmit_ctcss_default_tenths_hz: tone,
            transmit_ctcss_level_dbfs: -24,
            transmit_ctcss_turnoff_mode: CtcssTurnoffMode::PhaseShift,
            transmit_ctcss_phase_shift_degrees: 120,
            transmit_ctcss_turnoff_duration_ms: 180,
            transmit_ctcss_tail_tone_hz: 55,
            transmit_dcs_code: dcs,
            transmit_dcs_level_dbfs: -24,
            transmit_dcs_turnoff_enabled: true,
            transmit_dcs_turnoff_duration_ms: 180,
            transmit_settle_ms: 500,
            transmit_receive_blanking_ms: 0,
            transmit_off_delay_ms: 0,
        }
    }
}

/// Standalone audio-device and FFmpeg graph settings after inheritance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRadioSettings {
    /// Device selection policy.
    pub device_selection: RadioDeviceSelection,
    /// Stable USB topology or native ALSA hardware identifier.
    pub device_identifier: String,
    /// Optional exact USB serial number.
    pub usb_serial: String,
    /// Physical PortAudio input channels, one or two.
    pub input_device_channels: u32,
    /// Physical PortAudio output channels, one or two.
    pub output_device_channels: u32,
    /// Additional capture buffering requested from PortAudio, 0 through 500 ms.
    pub input_extra_buffer_ms: u32,
    /// Additional playback buffering requested from PortAudio, 0 through 500 ms.
    pub output_extra_buffer_ms: u32,
    /// Mono 48 kHz receive-processing graph.
    pub receive_graph: String,
    /// Mono 48 kHz transmit-processing graph.
    pub transmit_graph: String,
    /// CM119 GPIO wiring profile.
    pub cm119_profile: Cm119Profile,
    /// Invert the dedicated CM119 PTT output.
    pub cm119_ptt_inverted: bool,
    /// Initial direction and value for each ordinary CM119 GPIO.
    pub cm119_gpio_modes: [Cm119GpioMode; 8],
    /// Optional ordinary CM119 GPIO used as a clipping indicator.
    pub cm119_clip_led_gpio: Option<u8>,
    /// Receive qualification, COR, squelch and transmit signaling policy.
    pub signaling: ResolvedRadioSignalingSettings,
}

impl Default for ResolvedRadioSettings {
    fn default() -> Self {
        Self {
            device_selection: RadioDeviceSelection::Exact,
            device_identifier: String::new(),
            usb_serial: String::new(),
            input_device_channels: 1,
            output_device_channels: 1,
            input_extra_buffer_ms: 0,
            output_extra_buffer_ms: 0,
            receive_graph: "anull".to_owned(),
            transmit_graph: "anull".to_owned(),
            cm119_profile: Cm119Profile::DudeUsb,
            cm119_ptt_inverted: false,
            cm119_gpio_modes: [Cm119GpioMode::Input; 8],
            cm119_clip_led_gpio: None,
            signaling: ResolvedRadioSignalingSettings::default(),
        }
    }
}

impl ResolvedRadioSettings {
    /// Convert configured CM119 GPIO directions and initial levels to adapter masks.
    ///
    /// Bit zero represents GPIO 1; the returned values are output-enable and initial-high masks.
    pub fn cm119_gpio_output_masks(&self) -> (u8, u8) {
        self.cm119_gpio_modes.iter().enumerate().fold(
            (0, 0),
            |(enabled, initial), (index, mode)| {
                let bit = 1 << index;
                match mode {
                    Cm119GpioMode::Input => (enabled, initial),
                    Cm119GpioMode::OutputLow => (enabled | bit, initial),
                    Cm119GpioMode::OutputHigh => (enabled | bit, initial | bit),
                }
            },
        )
    }

    /// Resolve `[radio]` defaults and `[radio <node>]` overrides.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        Ok(Resolution {
            value: Self::from_document(document, node),
            warnings: schema.warnings,
        })
    }

    fn from_document(document: &ConfigDocument, node: &NodeId) -> Self {
        let scopes = [format!("radio {}", node.as_str()), "radio".to_owned()];
        let mut value = Self::default();
        if let Some(selection) =
            lookup_valid(document, "device_selection", &scopes, |raw| match raw {
                "exact" => Some(RadioDeviceSelection::Exact),
                "automatic_lowest_alsa_card" => Some(RadioDeviceSelection::AutomaticLowestAlsaCard),
                _ => None,
            })
        {
            value.device_selection = selection;
        }
        macro_rules! text {
            ($field:ident, $key:literal, $allow_empty:literal, $maximum:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, |raw| {
                    (raw.len() <= $maximum && ($allow_empty || !raw.is_empty()))
                        .then(|| raw.to_owned())
                }) {
                    value.$field = parsed;
                }
            };
        }
        macro_rules! number {
            ($field:ident, $key:literal, $minimum:literal, $maximum:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, |raw| {
                    parse::unsigned(raw, $minimum, $maximum)
                }) {
                    value.$field = parsed as u32;
                }
            };
        }
        text!(device_identifier, "device_identifier", true, 255);
        text!(usb_serial, "usb_serial", true, 255);
        text!(receive_graph, "receive_graph", false, 4096);
        text!(transmit_graph, "transmit_graph", false, 4096);
        if let Some(profile) = lookup_valid(document, "cm119_profile", &scopes, |raw| {
            Some(match raw {
                "dudeusb" => Cm119Profile::DudeUsb,
                "sphusb" => Cm119Profile::SphUsb,
                "nhrc" => Cm119Profile::Nhrc,
                "custom" => Cm119Profile::Custom,
                _ => return None,
            })
        }) {
            value.cm119_profile = profile;
        }
        if let Some(inverted) =
            lookup_valid(document, "cm119_ptt_inverted", &scopes, parse::boolean)
        {
            value.cm119_ptt_inverted = inverted;
        }
        for (index, mode) in value.cm119_gpio_modes.iter_mut().enumerate() {
            let key = format!("cm119_gpio_{}_mode", index + 1);
            if let Some(parsed) = lookup_valid(document, &key, &scopes, |raw| match raw {
                "in" => Some(Cm119GpioMode::Input),
                "out0" => Some(Cm119GpioMode::OutputLow),
                "out1" => Some(Cm119GpioMode::OutputHigh),
                _ => None,
            }) {
                *mode = parsed;
            }
        }
        if let Some(pin) = lookup_valid(document, "cm119_clip_led_gpio", &scopes, |raw| {
            parse::unsigned(raw, 0, 8)
        }) {
            value.cm119_clip_led_gpio = (pin != 0).then_some(pin as u8);
        }
        number!(input_device_channels, "input_device_channels", 1, 2);
        number!(output_device_channels, "output_device_channels", 1, 2);
        number!(input_extra_buffer_ms, "input_extra_buffer_ms", 0, 500);
        number!(output_extra_buffer_ms, "output_extra_buffer_ms", 0, 500);
        value.signaling = ResolvedRadioSignalingSettings::from_document(document, &scopes);
        value
    }
}

impl ResolvedRadioSignalingSettings {
    fn from_document(document: &ConfigDocument, scopes: &[String; 2]) -> Self {
        let mut value = Self::default();
        macro_rules! enum_value {
            ($field:ident, $key:literal, $parser:expr) => {
                if let Some(parsed) = lookup_valid(document, $key, scopes, $parser) {
                    value.$field = parsed;
                }
            };
        }
        macro_rules! unsigned_value {
            ($field:ident, $key:literal, $minimum:literal, $maximum:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, scopes, |raw| {
                    parse::unsigned(raw, $minimum, $maximum)
                }) {
                    value.$field = parsed as _;
                }
            };
        }
        macro_rules! signed_value {
            ($field:ident, $key:literal, $minimum:literal, $maximum:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, scopes, |raw| {
                    parse::signed(raw, $minimum, $maximum)
                }) {
                    value.$field = parsed;
                }
            };
        }
        enum_value!(receive_audio_source, "receive_audio", |raw| match raw {
            "disabled" => Some(RadioReceiveAudioSource::Disabled),
            "speaker" => Some(RadioReceiveAudioSource::Speaker),
            "flat" => Some(RadioReceiveAudioSource::Flat),
            _ => None,
        });
        enum_value!(receive_mode, "receive_signaling", parse_signaling_mode);
        enum_value!(carrier_source, "carrier_source", parse_carrier_source);
        enum_value!(
            receive_subaudible_source,
            "ctcss_source",
            parse_subaudible_source
        );
        if let Some(parsed) = lookup_valid(
            document,
            "receive_ctcss_tones_hz",
            scopes,
            parse::ctcss_tones,
        ) {
            value.receive_ctcss_tones_tenths_hz = parsed;
        }
        signed_value!(
            receive_ctcss_decoder_gain_db,
            "ctcss_decoder_gain_db",
            -60,
            24
        );
        if let Some(parsed) = lookup_valid(document, "ctcss_relaxed", scopes, parse::boolean) {
            value.receive_ctcss_relaxed = parsed;
        }
        if let Some(parsed) = lookup_valid(document, "ctcss_override", scopes, parse::boolean) {
            value.receive_ctcss_override = parsed;
        }
        enum_value!(receive_dcs_code, "dcs_receive_code", parse_dcs_code);
        unsigned_value!(squelch_level, "squelch_level", 0, 999);
        unsigned_value!(squelch_hysteresis, "squelch_hysteresis", 0, 32767);
        enum_value!(noise_filter, "noise_filter", |raw| match raw {
            "standard" => Some(RadioNoiseFilter::Standard),
            "alternate" => Some(RadioNoiseFilter::Alternate),
            _ => None,
        });
        unsigned_value!(vox_threshold, "vox_threshold", 0, 32767);
        unsigned_value!(vox_hang_ms, "vox_hang_ms", 0, 32767);
        unsigned_value!(receive_on_delay_ms, "receive_on_delay_ms", 0, 65535);
        enum_value!(radio_duplex_mode, "radio_duplex_mode", |raw| match raw {
            "half" => Some(RadioDuplexMode::Half),
            "full" => Some(RadioDuplexMode::Full),
            _ => None,
        });
        enum_value!(transmit_mode, "transmit_signaling", parse_signaling_mode);
        if let Some(parsed) = lookup_valid(
            document,
            "transmit_ctcss_tones_hz",
            scopes,
            parse::ctcss_tones,
        ) {
            value.transmit_ctcss_tones_tenths_hz = parsed;
        }
        if let Some(parsed) = lookup_valid(
            document,
            "transmit_ctcss_default_hz",
            scopes,
            parse::ctcss_tone_tenths_hz,
        ) {
            value.transmit_ctcss_default_tenths_hz = parsed;
        }
        signed_value!(
            transmit_ctcss_level_dbfs,
            "transmit_ctcss_level_dbfs",
            -60,
            0
        );
        enum_value!(
            transmit_ctcss_turnoff_mode,
            "transmit_ctcss_turnoff_mode",
            |raw| match raw {
                "none" => Some(CtcssTurnoffMode::None),
                "phase_shift" => Some(CtcssTurnoffMode::PhaseShift),
                "tone_remove" => Some(CtcssTurnoffMode::ToneRemove),
                "tail_tone" => Some(CtcssTurnoffMode::TailTone),
                _ => None,
            }
        );
        unsigned_value!(
            transmit_ctcss_phase_shift_degrees,
            "transmit_ctcss_phase_shift_degrees",
            0,
            360
        );
        unsigned_value!(
            transmit_ctcss_turnoff_duration_ms,
            "transmit_ctcss_turnoff_duration_ms",
            0,
            1000
        );
        unsigned_value!(
            transmit_ctcss_tail_tone_hz,
            "transmit_ctcss_tail_tone_hz",
            0,
            300
        );
        enum_value!(transmit_dcs_code, "transmit_dcs_code", parse_dcs_code);
        signed_value!(transmit_dcs_level_dbfs, "transmit_dcs_level_dbfs", -60, 0);
        if let Some(parsed) = lookup_valid(
            document,
            "transmit_dcs_turnoff_enabled",
            scopes,
            parse::boolean,
        ) {
            value.transmit_dcs_turnoff_enabled = parsed;
        }
        unsigned_value!(
            transmit_dcs_turnoff_duration_ms,
            "transmit_dcs_turnoff_duration_ms",
            150,
            200
        );
        unsigned_value!(transmit_settle_ms, "transmit_settle_ms", 0, 32767);
        unsigned_value!(
            transmit_receive_blanking_ms,
            "transmit_receive_blanking_ms",
            0,
            32767
        );
        unsigned_value!(transmit_off_delay_ms, "transmit_off_delay_ms", 0, 32767);
        value
    }
}

fn parse_signaling_mode(raw: &str) -> Option<RadioSignalingMode> {
    match raw {
        "carrier" => Some(RadioSignalingMode::Carrier),
        "ctcss" => Some(RadioSignalingMode::Ctcss),
        "dcs" => Some(RadioSignalingMode::Dcs),
        _ => None,
    }
}

fn parse_carrier_source(raw: &str) -> Option<RadioCarrierSource> {
    match raw {
        "disabled" => Some(RadioCarrierSource::Disabled),
        "dsp" => Some(RadioCarrierSource::Dsp),
        "vox" => Some(RadioCarrierSource::Vox),
        "cm119" => Some(RadioCarrierSource::Cm119),
        "cm119_inverted" => Some(RadioCarrierSource::Cm119Inverted),
        "parallel" => Some(RadioCarrierSource::Parallel),
        "parallel_inverted" => Some(RadioCarrierSource::ParallelInverted),
        _ => None,
    }
}

fn parse_subaudible_source(raw: &str) -> Option<RadioSubaudibleSource> {
    match raw {
        "disabled" => Some(RadioSubaudibleSource::Disabled),
        "cm119" => Some(RadioSubaudibleSource::Cm119),
        "cm119_inverted" => Some(RadioSubaudibleSource::Cm119Inverted),
        "dsp" => Some(RadioSubaudibleSource::Dsp),
        "parallel" => Some(RadioSubaudibleSource::Parallel),
        "parallel_inverted" => Some(RadioSubaudibleSource::ParallelInverted),
        _ => None,
    }
}

fn parse_dcs_code(raw: &str) -> Option<DcsCode> {
    let (value, inverted) = parse::dcs_code(raw)?;
    Some(DcsCode { value, inverted })
}

/// Owned identifier media and scheduling defaults after scope resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedIdentifierSettings {
    /// Positive identifier interval in milliseconds.
    pub interval_ms: u64,
    /// Nonnegative scheduler priority.
    pub priority: u64,
    /// Whether identification waits for the first qualifying key-up.
    pub first_key_only: bool,
    /// Whether to identify even while the node is inactive.
    pub regardless_of_activity: bool,
    /// Whether to defer identification while traffic is active.
    pub polite: bool,
    /// Maximum polite deferral in milliseconds.
    pub polite_maximum_wait_ms: u64,
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain in dB.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain in dB.
    pub morse_level_db: i64,
}

/// Owned announcement settings after flat, node, and named resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAnnouncementSettings {
    /// Zero plays after each ordinary transmitter release.
    pub interval_ms: u64,
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain in dB.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain in dB.
    pub morse_level_db: i64,
}

/// Resolved Morse defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMorseSettings {
    /// Tone frequency in hertz.
    pub frequency_hz: u64,
    /// Speed in words per minute.
    pub speed_wpm: u64,
    /// Level in dB.
    pub level_db: i64,
}
/// Resolved speech defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSpeechSettings {
    /// Voice model path.
    pub voice: String,
    /// Speaking speed percentage.
    pub speed_percent: u64,
    /// Level in dB.
    pub level_db: i64,
}
/// Resolved clock-announcement defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTimeSettings {
    /// Twelve- or twenty-four-hour display.
    pub format: u64,
}
/// Resolved named courtesy assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCourtesySettings {
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain, fixed at zero because courtesy uses its common level.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain, inherited from the common courtesy level.
    pub morse_level_db: i64,
    /// Optional generated tone sequence.
    pub tone_sequence: String,
    /// Courtesy input.
    pub input: String,
    /// Optional linked peer.
    pub remote_node: String,
    /// Common courtesy level in dB.
    pub level_db: i64,
}
/// Resolved named message template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTemplateSettings {
    /// Template label.
    pub name: String,
    /// Template text.
    pub text: String,
}
/// Resolved named controller macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMacroSettings {
    /// Macro label.
    pub name: String,
    action: crate::schedule::ScheduledAction,
    /// Optional decimal target.
    pub target_node: String,
}
/// Resolved zero-time event declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEventSettings {
    /// Event label.
    pub name: String,
    /// Civil trigger.
    pub at: String,
    /// Optional template.
    pub template: String,
    /// Optional message.
    pub message: String,
    /// Optional macro.
    pub macro_name: String,
}
/// Resolved permanent direct-link declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPermanentLinkSettings {
    /// Link label.
    pub name: String,
    /// Ordered remote decimal nodes, with index zero at highest priority.
    pub remote_nodes: Vec<String>,
    /// Optional display name for this permanent-link group.
    pub group_name: Option<String>,
}
/// Resolved replacement-window declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedScheduleSettings {
    /// Window label.
    pub name: String,
    /// Ordered replacement nodes, with index zero at highest priority.
    pub remote_nodes: Vec<String>,
    /// Optional display name for this scheduled-link group.
    pub group_name: Option<String>,
    /// Permanent label replaced by the window.
    pub replace_permanent: String,
    /// Optional weekday selector.
    pub days: String,
    /// Optional explicit-date selector.
    pub dates: String,
    /// Inclusive local start.
    pub start_time: String,
    /// Exclusive local end.
    pub end_time: String,
    /// Post-window inactivity grace in milliseconds.
    pub end_inactivity_ms: u64,
    /// Optional ordered warning leads before each occurrence starts.
    pub warning_before_start_ms: Vec<u64>,
    /// Optional ordered warning leads before the link is expected to disconnect.
    pub warning_before_end_ms: Vec<u64>,
    /// Stable Fluent message ID used when either warning list is configured.
    pub warning_message_id: Option<String>,
}

fn scoped<'a>(
    document: &'a ConfigDocument,
    node: &NodeId,
    family: &str,
    key: &str,
) -> Option<&'a str> {
    let node_scope = format!("{family} {}", node.as_str());
    document.lookup(key, &[node_scope.as_str(), family])
}

fn lookup_valid_scopes<T>(
    document: &ConfigDocument,
    key: &str,
    scopes: &[String],
    parse_value: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    for scope in scopes {
        if let Some(raw) = document.lookup(key, &[scope.as_str()]) {
            if let Some(value) = parse_value(raw) {
                return Some(value);
            }
        }
    }
    None
}

fn lookup_scopes(document: &ConfigDocument, key: &str, scopes: &[String]) -> Option<String> {
    scopes
        .iter()
        .find_map(|scope| document.lookup(key, &[scope.as_str()]).map(str::to_owned))
}

fn node_value(value: &str) -> Option<String> {
    (value.len() <= 63 && value.bytes().all(|byte| byte.is_ascii_digit())).then(|| value.to_owned())
}

fn family_scopes(node: &NodeId, family: &str) -> [String; 2] {
    [format!("{family} {}", node.as_str()), family.to_owned()]
}
impl ResolvedMorseSettings {
    /// Resolve flat and node-specific Morse defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self {
                frequency_hz: lookup_valid_scopes(
                    document,
                    "frequency_hz",
                    &family_scopes(node, "morse"),
                    |value| parse::unsigned(value, 1, u32::MAX as u64),
                )
                .unwrap_or(800),
                speed_wpm: lookup_valid_scopes(
                    document,
                    "speed_wpm",
                    &family_scopes(node, "morse"),
                    |value| parse::unsigned(value, 1, 100),
                )
                .unwrap_or(20),
                level_db: lookup_valid_scopes(
                    document,
                    "level_db",
                    &family_scopes(node, "morse"),
                    |value| parse::signed(value, -60, 0),
                )
                .unwrap_or(-6),
            },
            warnings,
        })
    }
}

impl ResolvedSpeechSettings {
    /// Resolve flat and node-specific speech defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self {
                voice: scoped(document, node, "speech", "voice")
                    .unwrap_or("en_US-lessac-medium.onnx")
                    .to_owned(),
                speed_percent: lookup_valid_scopes(
                    document,
                    "speed_percent",
                    &family_scopes(node, "speech"),
                    |value| parse::unsigned(value, 1, 1000),
                )
                .unwrap_or(100),
                level_db: lookup_valid_scopes(
                    document,
                    "level_db",
                    &family_scopes(node, "speech"),
                    |value| parse::signed(value, -60, 0),
                )
                .unwrap_or(0),
            },
            warnings,
        })
    }
}

impl ResolvedTimeSettings {
    /// Resolve flat and node-specific time defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self {
                format: lookup_valid_scopes(
                    document,
                    "format",
                    &family_scopes(node, "time"),
                    |value| {
                        matches!(value, "12" | "24")
                            .then(|| value.parse().ok())
                            .flatten()
                    },
                )
                .unwrap_or(12),
            },
            warnings,
        })
    }
}

impl ResolvedCourtesySettings {
    /// Resolve one named courtesy assignment.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("courtesy {} {label}", node.as_str());
        let named_scopes = vec![section.clone()];
        let courtesy_scopes = family_scopes(node, "courtesy");
        let input = document
            .lookup("input", &[section.as_str()])
            .unwrap_or("")
            .to_owned();
        let remote_node = lookup_valid_scopes(document, "remote_node", &named_scopes, node_value)
            .unwrap_or_default();
        if !matches!(input.as_str(), "receiver" | "link") {
            return Err(ConfigError::structure(
                &section,
                "courtesy input is required",
            ));
        }
        let level_db = lookup_valid_scopes(document, "level_db", &named_scopes, |value| {
            parse::signed(value, -60, 0)
        })
        .or_else(|| {
            lookup_valid_scopes(document, "level_db", &courtesy_scopes, |value| {
                parse::signed(value, -60, 0)
            })
        })
        .unwrap_or(-20);
        Ok(Resolution {
            value: Self {
                sound_file: lookup_scopes(document, "sound_file", &named_scopes)
                    .or_else(|| lookup_scopes(document, "sound_file", &courtesy_scopes))
                    .unwrap_or_default(),
                speech_text: lookup_scopes(document, "speech_text", &named_scopes)
                    .or_else(|| lookup_scopes(document, "speech_text", &courtesy_scopes))
                    .unwrap_or_default(),
                speech_model: lookup_scopes(document, "speech_model", &named_scopes)
                    .or_else(|| lookup_scopes(document, "voice", &family_scopes(node, "speech")))
                    .or_else(|| lookup_scopes(document, "speech_model", &courtesy_scopes))
                    .unwrap_or_else(|| "en_US-lessac-medium.onnx".to_owned()),
                speech_speed_percent: lookup_valid_scopes(
                    document,
                    "speech_speed_percent",
                    &named_scopes,
                    |value| parse::unsigned(value, 1, 1000),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_percent",
                        &family_scopes(node, "speech"),
                        |value| parse::unsigned(value, 1, 1000),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_speed_percent",
                        &courtesy_scopes,
                        |value| parse::unsigned(value, 1, 1000),
                    )
                })
                .unwrap_or(100),
                speech_level_db: 0,
                morse_text: lookup_scopes(document, "morse_text", &named_scopes)
                    .or_else(|| lookup_scopes(document, "morse_text", &courtesy_scopes))
                    .unwrap_or_default(),
                morse_speed_wpm: lookup_valid_scopes(
                    document,
                    "morse_speed_wpm",
                    &named_scopes,
                    |value| parse::unsigned(value, 1, 100),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_wpm",
                        &family_scopes(node, "morse"),
                        |value| parse::unsigned(value, 1, 100),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(document, "morse_speed_wpm", &courtesy_scopes, |value| {
                        parse::unsigned(value, 1, 100)
                    })
                })
                .unwrap_or(20),
                morse_frequency_hz: lookup_valid_scopes(
                    document,
                    "morse_frequency_hz",
                    &named_scopes,
                    |value| parse::unsigned(value, 1, u32::MAX as u64),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "frequency_hz",
                        &family_scopes(node, "morse"),
                        |value| parse::unsigned(value, 1, u32::MAX as u64),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(document, "morse_frequency_hz", &courtesy_scopes, |value| {
                        parse::unsigned(value, 1, u32::MAX as u64)
                    })
                })
                .unwrap_or(800),
                morse_level_db: level_db,
                tone_sequence: lookup_scopes(document, "tone_sequence", &named_scopes)
                    .or_else(|| lookup_scopes(document, "tone_sequence", &courtesy_scopes))
                    .unwrap_or_default(),
                input,
                remote_node,
                level_db,
            },
            warnings,
        })
    }
}

impl ResolvedTemplateSettings {
    /// Resolve a global template and same-label node override.
    pub fn resolve(
        document: &ConfigDocument,
        node: Option<&NodeId>,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let mut scopes = Vec::new();
        if let Some(node) = node {
            scopes.push(format!("template {} {label}", node.as_str()));
        }
        scopes.push(format!("template {label}"));
        let text = scopes
            .iter()
            .find_map(|scope| document.lookup("text", &[scope.as_str()]))
            .unwrap_or("");
        if text.is_empty() {
            return Err(ConfigError::structure(
                &scopes[0],
                "template text is required",
            ));
        }
        Ok(Resolution {
            value: Self {
                name: label.to_owned(),
                text: text.to_owned(),
            },
            warnings,
        })
    }
}

impl ResolvedMacroSettings {
    /// Validated controller operation, fixed when the macro definition is resolved.
    pub fn action(&self) -> crate::schedule::ScheduledAction {
        self.action
    }
    /// Resolve a global macro and same-label node override.
    pub fn resolve(
        document: &ConfigDocument,
        node: Option<&NodeId>,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let mut scopes = Vec::new();
        if let Some(node) = node {
            scopes.push(format!("macro {} {label}", node.as_str()));
        }
        scopes.push(format!("macro {label}"));
        let action = lookup_valid_scopes(document, "action", &scopes, |value| {
            value.parse::<crate::schedule::ScheduledAction>().ok()
        });
        let target =
            lookup_valid_scopes(document, "target_node", &scopes, node_value).unwrap_or_default();
        let action =
            action.ok_or_else(|| ConfigError::structure(&scopes[0], "macro action is required"))?;
        Ok(Resolution {
            value: Self {
                name: label.to_owned(),
                action,
                target_node: target,
            },
            warnings,
        })
    }
}

impl ResolvedEventSettings {
    /// Resolve a required per-node event declaration.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("event {} {label}", node.as_str());
        let get = |key| {
            document
                .lookup(key, &[section.as_str()])
                .unwrap_or("")
                .to_owned()
        };
        let value = Self {
            name: label.to_owned(),
            at: get("at"),
            template: get("template"),
            message: get("message"),
            macro_name: get("macro"),
        };
        if value.at.is_empty() {
            return Err(ConfigError::structure(&section, "event at is required"));
        }
        Ok(Resolution { value, warnings })
    }
}

impl ResolvedPermanentLinkSettings {
    /// Resolve a required per-node permanent link.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("permanent {} {label}", node.as_str());
        let remote_nodes = document
            .lookup("remote_node", &[section.as_str()])
            .unwrap_or("");
        if remote_nodes.trim().is_empty() {
            return Err(ConfigError::structure(
                &section,
                "permanent remote node is required",
            ));
        }
        Ok(Resolution {
            value: Self {
                name: label.to_owned(),
                remote_nodes: remote_nodes
                    .split(',')
                    .map(str::trim)
                    .map(str::to_owned)
                    .collect(),
                group_name: document
                    .lookup("group_name", &[section.as_str()])
                    .filter(|value| !value.trim().is_empty() && value.len() <= 63)
                    .map(str::to_owned),
            },
            warnings,
        })
    }
}

impl ResolvedScheduleSettings {
    /// Resolve a required same-node replacement window.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("schedule {} {label}", node.as_str());
        let get = |key| {
            document
                .lookup(key, &[section.as_str()])
                .unwrap_or("")
                .to_owned()
        };
        let value = Self {
            name: label.to_owned(),
            remote_nodes: get("remote_node")
                .split(',')
                .map(str::trim)
                .map(str::to_owned)
                .collect(),
            group_name: document
                .lookup("group_name", &[section.as_str()])
                .filter(|value| !value.trim().is_empty() && value.len() <= 63)
                .map(str::to_owned),
            replace_permanent: get("replace_permanent"),
            days: document
                .lookup("days", &[section.as_str()])
                .filter(|raw| weekday_selector_valid(raw))
                .unwrap_or("")
                .to_owned(),
            dates: document
                .lookup("dates", &[section.as_str()])
                .filter(|raw| date_selector_valid(raw))
                .unwrap_or("")
                .to_owned(),
            start_time: get("start_time"),
            end_time: get("end_time"),
            end_inactivity_ms: document
                .lookup("end_inactivity_ms", &[section.as_str()])
                .and_then(|raw| parse::unsigned(raw, 0, u64::MAX))
                .unwrap_or(0),
            warning_before_start_ms: document
                .lookup("warning_before_start_ms", &[section.as_str()])
                .and_then(parse::positive_milliseconds)
                .unwrap_or_default(),
            warning_before_end_ms: document
                .lookup("warning_before_end_ms", &[section.as_str()])
                .and_then(parse::positive_milliseconds)
                .unwrap_or_default(),
            warning_message_id: document
                .lookup("warning_message_id", &[section.as_str()])
                .filter(|value| *value == "scheduled-link-change")
                .map(str::to_owned),
        };
        if value.remote_nodes.iter().any(String::is_empty) {
            return Err(ConfigError::structure(
                &section,
                "schedule remote node, replacement, start time, and end time are required",
            ));
        }
        Ok(Resolution { value, warnings })
    }
}

impl ResolvedAnnouncementSettings {
    /// Resolve announcement values with named settings taking precedence.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        set: Option<&str>,
    ) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        let mut announcement_scopes = Vec::new();
        if let Some(set) = set {
            announcement_scopes.push(format!("announcement {} {set}", node.as_str()));
        }
        announcement_scopes.push(format!("announcement {}", node.as_str()));
        announcement_scopes.push("announcement".to_owned());
        let named_count = usize::from(set.is_some());
        Ok(Resolution {
            value: Self {
                interval_ms: lookup_valid_scopes(
                    document,
                    "interval_ms",
                    &announcement_scopes,
                    |raw| parse::unsigned(raw, 0, u64::MAX),
                )
                .unwrap_or(0),
                sound_file: lookup_scopes(document, "sound_file", &announcement_scopes)
                    .unwrap_or_default(),
                speech_text: lookup_scopes(document, "speech_text", &announcement_scopes)
                    .unwrap_or_default(),
                speech_model: lookup_scopes(
                    document,
                    "speech_model",
                    &announcement_scopes[..named_count],
                )
                .or_else(|| lookup_scopes(document, "voice", &family_scopes(node, "speech")))
                .or_else(|| {
                    lookup_scopes(
                        document,
                        "speech_model",
                        &announcement_scopes[named_count..],
                    )
                })
                .unwrap_or_else(|| "en_US-lessac-medium.onnx".to_owned()),
                speech_speed_percent: lookup_valid_scopes(
                    document,
                    "speech_speed_percent",
                    &announcement_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 1000),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_percent",
                        &family_scopes(node, "speech"),
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_speed_percent",
                        &announcement_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .unwrap_or(100),
                speech_level_db: lookup_valid_scopes(
                    document,
                    "speech_level_db",
                    &announcement_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "speech"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_level_db",
                        &announcement_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(0),
                morse_text: lookup_scopes(document, "morse_text", &announcement_scopes)
                    .unwrap_or_default(),
                morse_speed_wpm: lookup_valid_scopes(
                    document,
                    "morse_speed_wpm",
                    &announcement_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 100),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_wpm",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_speed_wpm",
                        &announcement_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .unwrap_or(20),
                morse_frequency_hz: lookup_valid_scopes(
                    document,
                    "morse_frequency_hz",
                    &announcement_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "frequency_hz",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_frequency_hz",
                        &announcement_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .unwrap_or(800),
                morse_level_db: lookup_valid_scopes(
                    document,
                    "morse_level_db",
                    &announcement_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "morse"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_level_db",
                        &announcement_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(-6),
            },
            warnings: schema.warnings,
        })
    }
}

impl ResolvedIdentifierSettings {
    /// Resolve flat, node, and named identifier values with the most-specific value first.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        set: Option<&str>,
    ) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        let mut identifier_scopes = Vec::new();
        if let Some(set) = set {
            identifier_scopes.push(format!("identifier {} {set}", node.as_str()));
        }
        identifier_scopes.push(format!("identifier {}", node.as_str()));
        identifier_scopes.push("identifier".to_owned());
        let named_count = usize::from(set.is_some());
        let number = |key, default, minimum| {
            lookup_valid_scopes(document, key, &identifier_scopes, |raw| {
                parse::unsigned(raw, minimum, u64::MAX)
            })
            .unwrap_or(default)
        };
        let boolean = |key, default| {
            lookup_valid_scopes(document, key, &identifier_scopes, parse::boolean)
                .unwrap_or(default)
        };
        Ok(Resolution {
            value: Self {
                interval_ms: number("interval_ms", 600_000, 1),
                priority: lookup_valid_scopes(document, "priority", &identifier_scopes, |raw| {
                    parse::unsigned(raw, 0, i32::MAX as u64)
                })
                .unwrap_or(0),
                first_key_only: boolean("first_key_only", false),
                regardless_of_activity: boolean("regardless_of_activity", false),
                polite: boolean("polite", false),
                polite_maximum_wait_ms: number("polite_maximum_wait_ms", 60_000, 1),
                sound_file: lookup_scopes(document, "sound_file", &identifier_scopes)
                    .unwrap_or_default(),
                speech_text: lookup_scopes(document, "speech_text", &identifier_scopes)
                    .unwrap_or_default(),
                speech_model: lookup_scopes(
                    document,
                    "speech_model",
                    &identifier_scopes[..named_count],
                )
                .or_else(|| lookup_scopes(document, "voice", &family_scopes(node, "speech")))
                .or_else(|| {
                    lookup_scopes(document, "speech_model", &identifier_scopes[named_count..])
                })
                .unwrap_or_else(|| "en_US-lessac-medium.onnx".to_owned()),
                speech_speed_percent: lookup_valid_scopes(
                    document,
                    "speech_speed_percent",
                    &identifier_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 1000),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_percent",
                        &family_scopes(node, "speech"),
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_speed_percent",
                        &identifier_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .unwrap_or(100),
                speech_level_db: lookup_valid_scopes(
                    document,
                    "speech_level_db",
                    &identifier_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "speech"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_level_db",
                        &identifier_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(0),
                morse_text: lookup_scopes(document, "morse_text", &identifier_scopes)
                    .unwrap_or_default(),
                morse_speed_wpm: lookup_valid_scopes(
                    document,
                    "morse_speed_wpm",
                    &identifier_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 100),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_wpm",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_speed_wpm",
                        &identifier_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .unwrap_or(20),
                morse_frequency_hz: lookup_valid_scopes(
                    document,
                    "morse_frequency_hz",
                    &identifier_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "frequency_hz",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_frequency_hz",
                        &identifier_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .unwrap_or(800),
                morse_level_db: lookup_valid_scopes(
                    document,
                    "morse_level_db",
                    &identifier_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "morse"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_level_db",
                        &identifier_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(-6),
            },
            warnings: schema.warnings,
        })
    }
}

impl ResolvedNodeSettings {
    /// Build the effective command map, rejecting unknown option names or invalid prefixes.
    pub fn command_map(&self) -> Result<DtmfCommandMap, crate::command::CommandMapError> {
        DtmfCommandMap::new(
            self.link_commands
                .iter()
                .map(|(key, digits)| {
                    command_action(key)
                        .map(|action| CommandMapping::new(digits, action))
                        .ok_or(crate::command::CommandMapError::UnknownAction)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
    /// Resolve general defaults and an exact node override, returning warnings for recoverable values.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let mut schema = Schema::validate(document)?;
        let mut value = Self::defaults(node);
        let scopes = [node.as_str().to_owned(), "general".to_owned()];
        macro_rules! text {
            ($field:ident, $key:literal) => {
                if let Some(raw) = lookup(document, $key, &scopes) {
                    value.$field = raw.to_owned();
                }
            };
        }
        macro_rules! boolean {
            ($field:ident, $key:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, parse::boolean) {
                    value.$field = parsed;
                }
            };
        }
        macro_rules! number {
            ($field:ident, $key:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, |raw| {
                    parse::unsigned(raw, 0, u64::MAX)
                }) {
                    value.$field = parsed;
                }
            };
        }
        boolean!(enabled, "node_enabled");
        boolean!(full_duplex, "full_duplex");
        boolean!(dtmf_muting, "dtmf_muting");
        boolean!(parrot_enabled, "parrot_enabled");
        if let Some(parsed) = lookup_valid(document, "dtmf_admin_timeout_ms", &scopes, |raw| {
            parse::unsigned(raw, 1, u64::MAX)
        }) {
            value.dtmf_admin_timeout_ms = parsed;
        }
        number!(squelch_delay_ms, "squelch_delay_ms");
        if let Some(parsed) =
            lookup_valid(document, "status_snapshot_interval_ms", &scopes, |raw| {
                parse::unsigned(raw, 1, u64::MAX)
            })
        {
            value.status_snapshot_interval_ms = parsed;
        }
        number!(hang_ms, "transmit_hang_ms");
        boolean!(ctcss_encode_on_input, "ctcss_encode_on_input");
        for scope in &scopes {
            let Some(raw) = document.lookup("ctcss_hang_ms", &[scope.as_str()]) else {
                continue;
            };
            let Some(parsed) = parse::unsigned(raw, 0, u64::MAX) else {
                continue;
            };
            if parsed > value.hang_ms {
                let line = document
                    .sections()
                    .iter()
                    .position(|section| section == scope)
                    .map_or(1, |index| document.section_line(index));
                schema.warnings.push(crate::config::ConfigWarning::new(
                    line,
                    scope,
                    "ctcss_hang_ms",
                    raw,
                    "exceeds transmit hang time",
                    "0",
                ));
            } else {
                value.ctcss_hang_ms = parsed;
                break;
            }
        }
        number!(transmit_timeout_ms, "transmit_timeout_ms");
        number!(timeout_lockout_ms, "timeout_lockout_ms");
        number!(kerchunk_max_ms, "kerchunk_max_ms");
        number!(courtesy_delay_ms, "courtesy_delay_ms");
        if let Some(parsed) = lookup_valid(document, "telemetry_duck_db", &scopes, |raw| {
            parse::signed(raw, -60, 0)
        }) {
            value.telemetry_duck_db = parsed;
        }
        text!(channel, "radio_channel");
        value.radio = ResolvedRadioSettings::from_document(document, node);
        if let Some(language) = lookup_valid(document, "language", &scopes, |raw| {
            raw.parse::<unic_langid::LanguageIdentifier>().ok()
        }) {
            value.language = language.to_string();
        }
        text!(statpost_url, "statpost_url");
        if let Some(parsed) = lookup_valid(document, "statpost_time", &scopes, |raw| {
            parse::unsigned(raw, 30, 600)
        }) {
            value.statpost_time = parsed;
        }
        if let Some(url) = lookup_valid(document, "iax_registration_url", &scopes, |raw| {
            if raw.is_empty() {
                return Some(String::new());
            }
            url::Url::parse(raw)
                .ok()
                .filter(|url| {
                    url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
                })
                .map(|_| raw.to_owned())
        }) {
            value.iax_registration_url = url;
        }
        if let Some(parsed) =
            lookup_valid(document, "iax_registration_interval_s", &scopes, |raw| {
                parse::unsigned(raw, 30, 600)
            })
        {
            value.iax_registration_interval_s = parsed;
        }
        if let Some(parsed) = lookup_valid(document, "iax_local_port", &scopes, |raw| {
            parse::unsigned(raw, 1, u16::MAX as u64)
        }) {
            value.iax_local_port = parsed as u16;
        }
        if let Some(raw) = lookup_valid(document, "callsign", &scopes, |raw| {
            (raw.len() <= 63).then(|| raw.to_owned())
        }) {
            value.callsign = raw;
        }
        if let Some(raw) = lookup_valid(document, "link_allow_nodes", &scopes, |raw| {
            AccessPolicy::list_valid(raw).then(|| raw.to_owned())
        }) {
            value.link_allow_nodes = raw;
        }
        if let Some(raw) = lookup_valid(document, "link_deny_nodes", &scopes, |raw| {
            AccessPolicy::list_valid(raw).then(|| raw.to_owned())
        }) {
            value.link_deny_nodes = raw;
        }
        text!(link_static_directory_file, "link_static_directory_file");
        text!(link_directory_file, "link_directory_file");
        if let Some(parsed) = lookup_valid(document, "link_lookup_method", &scopes, |raw| match raw
        {
            "dns" => Some(LinkLookupMethod::Dns),
            "file" => Some(LinkLookupMethod::File),
            "both" => Some(LinkLookupMethod::Both),
            _ => None,
        }) {
            value.link_lookup_method = parsed;
        }
        for key in command_keys()
            .into_iter()
            .filter(|key| key.starts_with("link_command_"))
        {
            if let Some(raw) = lookup_valid(document, key, &scopes, |raw| {
                DtmfCommandMap::validate(&[CommandMapping::new(raw, LinkAction::Status)])
                    .is_ok()
                    .then(|| raw.to_owned())
            }) {
                value.link_commands.insert(key.to_owned(), raw);
            }
        }
        if let Some(hash) =
            lookup_valid(document, "dtmf_admin_unlock_hash", &scopes, valid_argon2id)
        {
            value.dtmf_admin_unlock_hash = hash;
        }
        if let Some(hash) = lookup_valid(document, "dtmf_admin_lock_hash", &scopes, valid_argon2id)
        {
            value.dtmf_admin_lock_hash = hash;
        }
        value
            .command_map()
            .map_err(|error| ConfigError::structure(node.as_str(), &error.to_string()))?;
        Ok(Resolution {
            value,
            warnings: schema.warnings,
        })
    }

    fn defaults(node: &NodeId) -> Self {
        Self {
            enabled: true,
            full_duplex: true,
            dtmf_muting: true,
            parrot_enabled: false,
            squelch_delay_ms: 0,
            status_snapshot_interval_ms: 50,
            hang_ms: 0,
            ctcss_encode_on_input: false,
            ctcss_hang_ms: 0,
            transmit_timeout_ms: 180_000,
            timeout_lockout_ms: 30_000,
            kerchunk_max_ms: 500,
            telemetry_duck_db: -20,
            courtesy_delay_ms: 250,
            channel: node.as_str().to_owned(),
            radio: ResolvedRadioSettings::default(),
            language: "en-US".to_owned(),
            callsign: String::new(),
            statpost_url: String::new(),
            statpost_time: 60,
            iax_registration_url: "https://register.allstarlink.org/".to_owned(),
            iax_registration_interval_s: 60,
            iax_local_port: 4569,
            link_allow_nodes: String::new(),
            link_deny_nodes: String::new(),
            link_static_directory_file: String::new(),
            link_directory_file: String::new(),
            link_lookup_method: LinkLookupMethod::Both,
            link_commands: command_keys()
                .into_iter()
                .map(|key| (key.to_owned(), default_command(key)))
                .collect(),
            dtmf_admin_unlock_hash: String::new(),
            dtmf_admin_lock_hash: String::new(),
            dtmf_admin_timeout_ms: 300_000,
        }
    }
}

fn lookup<'a>(document: &'a ConfigDocument, key: &str, scopes: &[String; 2]) -> Option<&'a str> {
    document.lookup(key, &[scopes[0].as_str(), scopes[1].as_str()])
}

fn lookup_valid<T>(
    document: &ConfigDocument,
    key: &str,
    scopes: &[String; 2],
    parse_value: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    for scope in scopes {
        if let Some(raw) = document.lookup(key, &[scope.as_str()]) {
            if let Some(value) = parse_value(raw) {
                return Some(value);
            }
        }
    }
    None
}

fn command_keys() -> [&'static str; 20] {
    [
        "link_command_disconnect",
        "link_command_monitor",
        "link_command_transceive",
        "link_command_remote",
        "link_command_status",
        "link_command_disconnect_all",
        "link_command_last_keyed",
        "link_command_local_monitor",
        "link_command_disconnect_permanent",
        "link_command_permanent_monitor",
        "link_command_permanent_transceive",
        "link_command_full_status",
        "link_command_reconnect_all",
        "link_command_permanent_local_monitor",
        "fixed_10",
        "fixed_722",
        "link_command_admin_unlock",
        "link_command_admin_lock",
        "link_command_parrot_enable",
        "link_command_parrot_disable",
    ]
}

fn default_command(key: &str) -> String {
    match key {
        "link_command_disconnect" => "1",
        "link_command_monitor" => "2",
        "link_command_transceive" => "3",
        "link_command_remote" => "4",
        "link_command_status" => "70",
        "link_command_disconnect_all" => "806",
        "link_command_last_keyed" => "72",
        "link_command_local_monitor" => "75",
        "link_command_disconnect_permanent" => "811",
        "link_command_permanent_monitor" => "812",
        "link_command_permanent_transceive" => "813",
        "link_command_full_status" => "73",
        "link_command_reconnect_all" => "816",
        "link_command_permanent_local_monitor" => "818",
        "fixed_10" => "10",
        "fixed_722" => "722",
        "link_command_admin_unlock" => "800",
        "link_command_admin_lock" => "801",
        "link_command_parrot_enable" => "804",
        "link_command_parrot_disable" => "805",
        _ => "",
    }
    .to_owned()
}

fn valid_argon2id(value: &str) -> Option<String> {
    use argon2::password_hash::PasswordHash;
    let hash = PasswordHash::new(value).ok()?;
    (hash.algorithm.as_str() == "argon2id").then(|| value.to_owned())
}

fn command_action(key: &str) -> Option<LinkAction> {
    Some(match key {
        "link_command_disconnect" => LinkAction::Disconnect,
        "link_command_monitor" => LinkAction::Monitor,
        "link_command_transceive" => LinkAction::Transceive,
        "link_command_remote" => LinkAction::Command,
        "link_command_status" => LinkAction::Status,
        "link_command_disconnect_all" => LinkAction::DisconnectPermanentAll,
        "link_command_last_keyed" => LinkAction::LastKeyed,
        "link_command_local_monitor" => LinkAction::LocalMonitor,
        "link_command_disconnect_permanent" => LinkAction::DisconnectPermanent,
        "link_command_permanent_monitor" => LinkAction::PermanentMonitor,
        "link_command_permanent_transceive" => LinkAction::PermanentTransceive,
        "link_command_full_status" => LinkAction::FullStatus,
        "link_command_reconnect_all" => LinkAction::ReconnectPermanentAll,
        "link_command_permanent_local_monitor" => LinkAction::PermanentLocalMonitor,
        "fixed_10" => LinkAction::DisconnectNonPermanentAll,
        "fixed_722" => LinkAction::Time,
        "link_command_admin_unlock" => LinkAction::AdminUnlock,
        "link_command_admin_lock" => LinkAction::AdminLock,
        "link_command_parrot_enable" => LinkAction::ParrotEnable,
        "link_command_parrot_disable" => LinkAction::ParrotDisable,
        _ => return None,
    })
}

/// A validated node identity with the transport's 63-byte limit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    /// Construct an identity containing no whitespace/brackets and at most 63 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 63
            || value
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '[' | ']'))
        {
            return Err(ConfigError::structure(&value, "invalid node identity"));
        }
        Ok(Self(value))
    }

    /// Borrow the identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
