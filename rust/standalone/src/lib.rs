//! Standalone process setup and lifecycle.
#![deny(warnings, missing_docs)]

use rpt_advanced_core::config::{
    Cm119Profile, ConfigDocument, ConfigError, NodeId, RadioDeviceSelection, Resolution,
    ResolvedNodeSettings, ResolvedRadioSettings, Schema,
};

#[allow(warnings, missing_docs, clippy::all)]
pub(crate) mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}

pub mod audio_callbacks;
/// PortAudio stream ownership for one resolved CM119 generation.
pub mod audio_stream;
/// Standalone AllStarLink destination lookup without Asterisk resolver APIs.
pub mod directory;
/// Signal-driven standalone process lifecycle.
pub mod foreground;
/// CM119 HID/GPIO ownership and non-audio hardware service calls.
pub mod gpio_device;
/// Asterisk-free implementations of product host services.
pub mod host_services;
/// Dynamically loaded outbound AllStarLink-compatible IAX2 client.
pub mod iax;
/// Prepared FFmpeg graphs exposed as allocation-free radio-core processor ports.
pub mod processing;
/// Product transmit audio bridge into the native radio program source.
pub mod program_audio;
/// Runtime provider loading and version validation.
pub mod providers;
/// Safe ordered activation of one standalone CM119 radio path.
pub mod radio_activation;
/// Mapping from resolved node signaling settings to the radio-core session ABI.
pub mod radio_config;
/// Owned radio-core sessions and the FFmpeg ports used by each generation.
pub mod radio_generation;
#[cfg(test)]
#[path = "radio_generation_tests.rs"]
mod radio_generation_tests;
/// Standalone product requests resolved to configured radio-channel reservations.
pub mod radio_host;
/// Prepared radio-core session retained for a native audio generation.
pub mod radio_session;
/// ASL3-compatible HTTPS registration kept outside real-time audio callbacks.
pub mod registration;
/// Product start/reload/stop lifetime for the standalone process.
pub mod runtime;
/// Secure IAX credentials loaded separately from the ordinary node configuration.
pub mod secrets;

/// One configured node's resolved channel and CM119-owned hardware settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRadioNode {
    /// Configured node identity.
    pub node: NodeId,
    /// Radio channel name used by the product host-services boundary.
    pub channel: String,
    /// Whether this node is enabled for standalone operation.
    pub enabled: bool,
    /// CM119, PortAudio, and processing settings resolved for this node.
    pub settings: ResolvedRadioSettings,
}

/// PortAudio device-selection request produced from one node's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDeviceRequest {
    /// Explicit selection policy.
    pub policy: RadioDeviceSelection,
    /// Optional USB topology or native ALSA hardware identifier.
    pub device_identifier: Option<String>,
    /// Optional exact USB serial number.
    pub usb_serial: Option<String>,
    /// Required physical input-channel count.
    pub input_channels: u32,
    /// Required physical output-channel count.
    pub output_channels: u32,
}

/// Exact device selection could not identify a physical interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioDeviceRequestError {
    /// Exact selection needs an identifier, a serial number, or both.
    MissingExactDeviceIdentity,
}

/// CM119 GPIO configuration ready to pass to the hardware adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cm119HardwareRequest {
    /// CM119 wiring profile.
    pub profile: Cm119Profile,
    /// Whether the dedicated PTT output is inverted.
    pub ptt_inverted: bool,
    /// Ordinary GPIO output-enable mask, GPIO 1 in bit zero.
    pub output_enable_mask: u8,
    /// Initial output levels, GPIO 1 in bit zero.
    pub initial_output_mask: u8,
    /// Optional ordinary GPIO used as the clipping indicator.
    pub clip_led_gpio: Option<u8>,
}

impl ResolvedRadioNode {
    /// Convert this node's settings to a validated PortAudio selection request.
    pub fn audio_device_request(&self) -> Result<AudioDeviceRequest, RadioDeviceRequestError> {
        let settings = &self.settings;
        let device_identifier = nonempty(&settings.device_identifier);
        let usb_serial = nonempty(&settings.usb_serial);
        if settings.device_selection == RadioDeviceSelection::Exact
            && device_identifier.is_none()
            && usb_serial.is_none()
        {
            return Err(RadioDeviceRequestError::MissingExactDeviceIdentity);
        }
        Ok(AudioDeviceRequest {
            policy: settings.device_selection,
            device_identifier,
            usb_serial,
            input_channels: settings.input_device_channels,
            output_channels: settings.output_device_channels,
        })
    }

    /// Convert CM119 settings to the GPIO adapter's platform-neutral request.
    pub fn cm119_hardware_request(&self) -> Cm119HardwareRequest {
        let (output_enable_mask, initial_output_mask) = self.settings.cm119_gpio_output_masks();
        Cm119HardwareRequest {
            profile: self.settings.cm119_profile,
            ptt_inverted: self.settings.cm119_ptt_inverted,
            output_enable_mask,
            initial_output_mask,
            clip_led_gpio: self.settings.cm119_clip_led_gpio,
        }
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// Resolve each configured node's radio settings before any hardware is opened.
pub fn resolve_radio_nodes(
    document: &ConfigDocument,
) -> Result<Resolution<Vec<ResolvedRadioNode>>, ConfigError> {
    let schema = Schema::validate(document)?;
    let nodes = document.nodes();
    let mut radios = Vec::with_capacity(nodes.len());
    for name in nodes {
        let node = NodeId::new(name.to_owned())?;
        let settings = ResolvedNodeSettings::resolve(document, &node)?.value;
        radios.push(ResolvedRadioNode {
            node,
            channel: settings.channel,
            enabled: settings.enabled,
            settings: settings.radio,
        });
    }
    Ok(Resolution {
        value: radios,
        warnings: schema.warnings,
    })
}

/// Validate a configuration without opening runtime providers or devices.
pub fn check_config(text: &str) -> Result<Vec<String>, ConfigError> {
    let document = ConfigDocument::parse(text)?;
    let resolution = Schema::validate(&document)?;
    Ok(resolution
        .warnings
        .into_iter()
        .map(|warning| {
            format!(
                "line {} [{}] {}: {}",
                warning.line, warning.section, warning.key, warning.message
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{RadioDeviceRequestError, check_config, radio_config, resolve_radio_nodes};
    use rpt_advanced_core::config::ConfigDocument;

    #[test]
    fn check_config_accepts_the_shipped_example_without_opening_devices() {
        let example = include_str!("../../../examples/rpt_advanced.conf");
        assert!(check_config(example).unwrap().is_empty());
    }

    #[test]
    fn check_config_rejects_invalid_structure() {
        let error = check_config("[node 1000]\n[permanent remote 2000]\n").unwrap_err();
        assert!(error.to_string().contains("unknown node"));
    }

    #[test]
    fn check_config_returns_nonfatal_schema_warnings() {
        let warnings = check_config("[1000]\nunknown_setting=value\n").unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("[1000] unknown_setting"));
    }

    #[test]
    fn radio_configuration_is_resolved_per_node_and_channel() {
        let document = ConfigDocument::parse(
            "[radio]\ndevice_identifier=3-1\ncm119_profile=sphusb\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\ncm119_clip_led_gpio=2\n[1000]\nradio_channel=vhf\n[radio 1000]\ncm119_profile=nhrc\ncm119_ptt_inverted=no\ncm119_gpio_1_mode=out0\n[2000]\nradio_channel=uhf\n[radio 2000]\nusb_serial=SECOND\n",
        )
        .unwrap();

        let radios = resolve_radio_nodes(&document).unwrap().value;

        assert_eq!(radios.len(), 2);
        assert_eq!(radios[0].channel, "vhf");
        assert_eq!(radios[0].node.as_str(), "1000");
        assert_eq!(radios[0].settings.device_identifier, "3-1");
        assert_eq!(radios[0].settings.cm119_gpio_output_masks(), (1, 0));
        assert_eq!(radios[1].channel, "uhf");
        assert_eq!(radios[1].node.as_str(), "2000");
        assert_eq!(radios[1].settings.device_identifier, "3-1");
        assert_eq!(radios[1].settings.usb_serial, "SECOND");
        assert_eq!(radios[1].settings.cm119_gpio_output_masks(), (1, 1));
        let request = radios[0].audio_device_request().unwrap();
        assert_eq!(request.device_identifier.as_deref(), Some("3-1"));
        assert_eq!(request.input_channels, 1);
        assert_eq!(request.output_channels, 1);
        let gpio = radios[0].cm119_hardware_request();
        assert_eq!(gpio.profile, rpt_advanced_core::config::Cm119Profile::Nhrc);
        assert!(!gpio.ptt_inverted);
        assert_eq!(gpio.output_enable_mask, 1);
        assert_eq!(gpio.initial_output_mask, 0);
        assert_eq!(gpio.clip_led_gpio, Some(2));
        let inherited_request = radios[1].audio_device_request().unwrap();
        assert_eq!(inherited_request.device_identifier.as_deref(), Some("3-1"));
        assert_eq!(inherited_request.usb_serial.as_deref(), Some("SECOND"));
        let inherited_gpio = radios[1].cm119_hardware_request();
        assert_eq!(
            inherited_gpio.profile,
            rpt_advanced_core::config::Cm119Profile::SphUsb
        );
        assert!(inherited_gpio.ptt_inverted);
        assert_eq!(inherited_gpio.output_enable_mask, 1);
        assert_eq!(inherited_gpio.initial_output_mask, 1);
        assert_eq!(inherited_gpio.clip_led_gpio, Some(2));
    }

    #[test]
    fn exact_radio_selection_requires_a_device_identity() {
        let document = ConfigDocument::parse("[1000]\n").unwrap();

        let radio = &resolve_radio_nodes(&document).unwrap().value[0];

        assert_eq!(
            radio.audio_device_request(),
            Err(RadioDeviceRequestError::MissingExactDeviceIdentity)
        );

        let identified =
            ConfigDocument::parse("[radio]\ndevice_selection=exact\nusb_serial=CM119-1\n[1000]\n")
                .unwrap();
        assert_eq!(
            resolve_radio_nodes(&identified).unwrap().value[0]
                .audio_device_request()
                .unwrap()
                .usb_serial
                .as_deref(),
            Some("CM119-1")
        );
    }

    #[test]
    fn automatic_radio_selection_omits_explicit_device_identity() {
        let document =
            ConfigDocument::parse("[radio]\ndevice_selection=automatic_lowest_alsa_card\n[1000]\n")
                .unwrap();

        let radios = resolve_radio_nodes(&document).unwrap().value;
        let request = radios[0].audio_device_request().unwrap();

        assert_eq!(request.device_identifier, None);
        assert_eq!(request.usb_serial, None);
        assert_eq!(
            request.policy,
            rpt_advanced_core::config::RadioDeviceSelection::AutomaticLowestAlsaCard
        );
    }

    #[test]
    fn resolved_signaling_settings_map_to_the_radio_core_session() {
        let document = ConfigDocument::parse(
            "[1000]\n[radio 1000]\nreceive_signaling=ctcss\nctcss_source=dsp\nreceive_ctcss_tones_hz=100.0,103.5\ntransmit_signaling=ctcss\ntransmit_ctcss_tones_hz=100.0,103.5\ntransmit_ctcss_default_hz=103.5\ntransmit_ctcss_level_dbfs=-18\ntransmit_ctcss_turnoff_mode=tail_tone\ntransmit_ctcss_tail_tone_hz=55\nreceive_on_delay_ms=21\ntransmit_off_delay_ms=20\n",
        )
        .unwrap();
        let node = &resolve_radio_nodes(&document).unwrap().value[0];

        let config = radio_config::radio_session_config(node, 7, 4096, 50).unwrap();

        assert_eq!(config.generation_id, 7);
        assert_eq!(config.native_sample_rate_hz, 48_000);
        assert_eq!(config.maximum_receive_frame_count, 4096);
        assert_eq!(config.receive.ctcss_enabled, 1);
        assert_eq!(config.receive.ctcss_tone_mask, (1 << 11) | (1 << 12));
        assert_eq!(config.qualification.subaudible_source, 3);
        assert_eq!(config.qualification.rx_on_delay_blocks, 2);
        assert_eq!(config.transmit.ctcss_transmit_enabled, 1);
        assert_eq!(config.transmit.default_ctcss_frequency_tenths_hz, 1035);
        assert_eq!(config.transmit.mapped_ctcss_frequency_tenths_hz[11], 1000);
        assert_eq!(config.transmit.mapped_ctcss_frequency_tenths_hz[12], 1035);
        assert_eq!(config.transmit.tone_off_mode, 3);
        assert_eq!(config.transmit.ctcss_turnoff_tail_tone_hz, 55.0);
        assert_eq!(config.qualification.tx_off_delay_blocks, 1);
    }

    #[test]
    fn radio_settings_prepare_and_warm_the_released_core_without_hardware() {
        let library = unsafe { libloading::Library::new("librptadvradio.so.4") }.unwrap();
        let descriptor = unsafe {
            let symbol: libloading::Symbol<
                unsafe extern "C" fn() -> *const super::abi::rptadv_radio_descriptor,
            > = library.get(b"rptadv_radio_descriptor\0").unwrap();
            symbol()
        };
        let descriptor = unsafe { descriptor.as_ref() }.unwrap();
        let document = ConfigDocument::parse("[1000]\n[radio]\n").unwrap();
        let radio = &resolve_radio_nodes(&document).unwrap().value[0];
        let mut ports: super::abi::rptadv_radio_session_ports = unsafe { std::mem::zeroed() };
        ports.struct_size = std::mem::size_of_val(&ports) as u32;
        let config = radio_config::radio_session_config(radio, 1, 960, 50).unwrap();

        let session =
            super::radio_session::NativeRadioSession::prepare(descriptor, &config, &ports).unwrap();

        drop(session);
    }
}
