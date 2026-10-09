use super::{
    ConfigDocument, ConfigError, NodeId, RadioCarrierSource, RadioDuplexMode, RadioNoiseFilter,
    RadioReceiveAudioSource, RadioSignalingMode, ResolvedAnnouncementSettings,
    ResolvedCourtesySettings, ResolvedEventSettings, ResolvedIdentifierSettings,
    ResolvedMorseSettings, ResolvedNodeSettings, ResolvedPermanentLinkSettings,
    ResolvedRadioSettings, ResolvedScheduleSettings, ResolvedSpeechSettings, ResolvedTimeSettings,
    Schema,
};
use std::collections::BTreeMap;

fn validate_command_map(
    commands: &BTreeMap<String, String>,
) -> Result<(), crate::command::CommandMapError> {
    let mut settings = ResolvedNodeSettings::resolve(
        &ConfigDocument::parse("[1000]\n").unwrap(),
        &NodeId::new("1000").unwrap(),
    )
    .unwrap()
    .value;
    settings.link_commands = commands.clone();
    settings.command_map().map(|_| ())
}

#[test]
fn caller_supplied_unknown_command_action_is_rejected_without_panicking() {
    let mut settings = ResolvedNodeSettings::resolve(
        &ConfigDocument::parse("[1000]\n").unwrap(),
        &NodeId::new("1000").unwrap(),
    )
    .unwrap()
    .value;
    settings
        .link_commands
        .insert("unknown_action".into(), "99".into());
    let error = settings.command_map().unwrap_err();
    assert_eq!(error, crate::command::CommandMapError::UnknownAction);
    assert_eq!(error.to_string(), "unknown command action");
}

#[test]
fn node_directory_access_and_lookup_settings_keep_valid_overrides() {
    for (method, expected) in [
        ("dns", super::LinkLookupMethod::Dns),
        ("file", super::LinkLookupMethod::File),
        ("both", super::LinkLookupMethod::Both),
    ] {
        let document = ConfigDocument::parse(&format!("[1000]\ncallsign=W1AW\nlink_allow_nodes=2000,3000\nlink_deny_nodes=4000\nlink_static_directory_file=static.conf\nlink_directory_file=directory.conf\nlink_lookup_method={method}\n")).unwrap();
        let value = ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap())
            .unwrap()
            .value;
        assert_eq!(value.callsign, "W1AW");
        assert_eq!(value.link_allow_nodes, "2000,3000");
        assert_eq!(value.link_deny_nodes, "4000");
        assert_eq!(value.link_static_directory_file, "static.conf");
        assert_eq!(value.link_directory_file, "directory.conf");
        assert_eq!(value.link_lookup_method, expected);
    }
    let commands = BTreeMap::from([("link_command_status".into(), "?".into())]);
    assert_eq!(
        validate_command_map(&commands),
        Err(crate::command::CommandMapError::InvalidPrefix)
    );
    assert_eq!(
        NodeId::new("").unwrap_err().to_string(),
        ": invalid node identity"
    );
    assert_eq!(
        ConfigDocument::parse("key=value\n")
            .unwrap_err()
            .to_string(),
        "line 1: option before section"
    );
}

#[test]
fn statpost_settings_inherit_global_values_allow_node_overrides_and_clear_url() {
    let document = ConfigDocument::parse(
        "[general]\nstatpost_url=https://stats.example/status\nstatpost_time=90\n[1000]\n[2000]\nstatpost_url=http://local.test/report\nstatpost_time=120\n[3000]\nstatpost_url=\n",
    )
    .unwrap();

    let inherited = ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    assert_eq!(inherited.statpost_url, "https://stats.example/status");
    assert_eq!(inherited.statpost_time, 90);

    let overridden = ResolvedNodeSettings::resolve(&document, &NodeId::new("2000").unwrap())
        .unwrap()
        .value;
    assert_eq!(overridden.statpost_url, "http://local.test/report");
    assert_eq!(overridden.statpost_time, 120);

    let cleared = ResolvedNodeSettings::resolve(&document, &NodeId::new("3000").unwrap())
        .unwrap()
        .value;
    assert!(cleared.statpost_url.is_empty());
    assert_eq!(cleared.statpost_time, 90);
}

#[test]
fn iax_registration_settings_inherit_and_allow_node_overrides() {
    let document = ConfigDocument::parse(
        "[general]\niax_registration_url=https://register.example/\niax_registration_interval_s=120\niax_local_port=4570\n[1000]\n[2000]\niax_registration_url=https://backup.example/\niax_registration_interval_s=300\niax_local_port=4569\n",
    )
    .unwrap();

    let inherited = ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    let overridden = ResolvedNodeSettings::resolve(&document, &NodeId::new("2000").unwrap())
        .unwrap()
        .value;

    assert_eq!(inherited.iax_registration_url, "https://register.example/");
    assert_eq!(inherited.iax_registration_interval_s, 120);
    assert_eq!(inherited.iax_local_port, 4570);
    assert_eq!(overridden.iax_registration_url, "https://backup.example/");
    assert_eq!(overridden.iax_registration_interval_s, 300);
    assert_eq!(overridden.iax_local_port, 4569);
}

#[test]
fn iax_registration_defaults_to_asl_endpoint_and_standard_port_and_refresh() {
    let document = ConfigDocument::parse("[1000]\n").unwrap();
    let settings = ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;

    assert_eq!(
        settings.iax_registration_url,
        "https://register.allstarlink.org/"
    );
    assert_eq!(settings.iax_registration_interval_s, 60);
    assert_eq!(settings.iax_local_port, 4569);
}

#[test]
fn invalid_iax_registration_url_uses_the_safe_inherited_endpoint() {
    let document = ConfigDocument::parse(
        "[general]\niax_registration_url=https://register.example/\n[1000]\niax_registration_url=http://untrusted.example/\n",
    )
    .unwrap();

    let resolution =
        ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap()).unwrap();

    assert_eq!(
        resolution.value.iax_registration_url,
        "https://register.example/"
    );
    assert!(resolution.warnings.iter().any(|warning| {
        warning.key == "iax_registration_url" && warning.value == "<redacted URL>"
    }));
}

#[test]
fn iax_registration_url_can_be_cleared_and_credentials_are_rejected() {
    let cleared = ConfigDocument::parse(
        "[general]\niax_registration_url=https://register.example/\n[1000]\niax_registration_url=\n",
    )
    .unwrap();
    let settings = ResolvedNodeSettings::resolve(&cleared, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    assert!(settings.iax_registration_url.is_empty());

    let credentials = ConfigDocument::parse(
        "[general]\niax_registration_url=https://register.example/\n[1000]\niax_registration_url=https://user:secret@register.example/\n",
    )
    .unwrap();
    let resolution =
        ResolvedNodeSettings::resolve(&credentials, &NodeId::new("1000").unwrap()).unwrap();
    assert_eq!(
        resolution.value.iax_registration_url,
        "https://register.example/"
    );
    assert!(resolution.warnings.iter().any(|warning| {
        warning.key == "iax_registration_url" && warning.value == "<redacted URL>"
    }));
}

#[test]
fn radio_settings_inherit_defaults_and_allow_node_overrides() {
    let document = ConfigDocument::parse(
        "[radio]\ndevice_selection=automatic_lowest_alsa_card\ndevice_identifier=3-1:1.0\nusb_serial=SERIAL\ninput_extra_buffer_ms=40\nreceive_graph=highpass=f=150\n[1000]\n[radio 1000]\ndevice_selection=exact\nusb_serial=NODE-SERIAL\noutput_device_channels=2\noutput_extra_buffer_ms=20\ntransmit_graph=anull\n[2000]\n",
    )
    .unwrap();

    let resolved = ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    assert_eq!(resolved.device_identifier, "3-1:1.0");
    assert_eq!(
        resolved.device_selection,
        super::RadioDeviceSelection::Exact
    );
    assert_eq!(resolved.usb_serial, "NODE-SERIAL");
    assert_eq!(resolved.input_extra_buffer_ms, 40);
    assert_eq!(resolved.output_extra_buffer_ms, 20);
    assert_eq!(resolved.output_device_channels, 2);
    assert_eq!(resolved.receive_graph, "highpass=f=150");
    assert_eq!(resolved.transmit_graph, "anull");
    let node = ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    assert_eq!(node.radio, resolved);
    let inherited = ResolvedRadioSettings::resolve(&document, &NodeId::new("2000").unwrap())
        .unwrap()
        .value;
    assert_eq!(
        inherited.device_selection,
        super::RadioDeviceSelection::AutomaticLowestAlsaCard
    );
}

#[test]
fn radio_receive_gains_resolve_defaults_inheritance_and_node_overrides() {
    for (source, expected, warning_count) in [
        ("[1000]\n", (0, 0), 0),
        (
            "[radio]\nreceive_input_gain_db=-2\nreceive_output_gain_db=-6\n[1000]\n",
            (-2, -6),
            0,
        ),
        (
            "[radio]\nreceive_input_gain_db=-2\nreceive_output_gain_db=-6\n[1000]\n[radio 1000]\nreceive_input_gain_db=30\nreceive_output_gain_db=-30\n",
            (30, -30),
            0,
        ),
        (
            "[radio]\nreceive_input_gain_db=-30\nreceive_output_gain_db=30\n[1000]\n",
            (-30, 30),
            0,
        ),
        (
            "[radio]\nreceive_input_gain_db=-2\nreceive_output_gain_db=-6\n[1000]\n[radio 1000]\nreceive_input_gain_db=-31\nreceive_output_gain_db=31\n",
            (-2, -6),
            2,
        ),
        (
            "[radio]\nreceive_input_gain_db=-2\nreceive_output_gain_db=-6\n[1000]\n[radio 1000]\nreceive_input_gain_db=-2.5\nreceive_output_gain_db=invalid\n",
            (-2, -6),
            2,
        ),
        (
            "[radio]\nreceive_input_gain_db=-31\nreceive_output_gain_db=31\n[1000]\n",
            (0, 0),
            2,
        ),
    ] {
        let resolved = ResolvedRadioSettings::resolve(
            &ConfigDocument::parse(source).unwrap(),
            &NodeId::new("1000").unwrap(),
        )
        .unwrap();
        assert_eq!(
            (
                resolved.value.receive_input_gain_db,
                resolved.value.receive_output_gain_db,
            ),
            expected,
            "{source}"
        );
        assert_eq!(resolved.warnings.len(), warning_count, "{source}");
    }
}

#[test]
fn radio_signaling_options_are_supported_in_default_and_node_scopes() {
    let document = ConfigDocument::parse(
        "[radio]\nreceive_audio=flat\nreceive_signaling=ctcss\ncarrier_source=dsp\nctcss_source=dsp\nreceive_ctcss_tones_hz=100.0,103.5\nctcss_relaxed=yes\nctcss_decoder_gain_db=0\ndcs_receive_code=023N\nsquelch_level=500\nsquelch_hysteresis=3000\nnoise_filter=standard\nvox_threshold=0\nvox_hang_ms=2000\nreceive_on_delay_ms=0\nradio_duplex_mode=half\ntransmit_signaling=ctcss\ntransmit_ctcss_tones_hz=100.0\ntransmit_ctcss_default_hz=100.0\ntransmit_ctcss_level_dbfs=-24\ntransmit_ctcss_turnoff_mode=phase_shift\ntransmit_ctcss_phase_shift_degrees=120\ntransmit_ctcss_turnoff_duration_ms=180\ntransmit_ctcss_tail_tone_hz=55\ntransmit_dcs_code=023N\ntransmit_dcs_level_dbfs=-24\ntransmit_dcs_turnoff_enabled=yes\ntransmit_dcs_turnoff_duration_ms=180\ntransmit_settle_ms=500\ntransmit_receive_blanking_ms=0\ntransmit_off_delay_ms=0\n[1000]\n[radio 1000]\nreceive_signaling=dcs\ndcs_receive_code=047I\ntransmit_signaling=dcs\ntransmit_dcs_code=047I\n",
    )
    .unwrap();

    let resolved =
        ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap()).unwrap();
    assert!(resolved.warnings.is_empty());
    assert_eq!(
        resolved.value.signaling.receive_mode,
        super::RadioSignalingMode::Dcs
    );
    assert_eq!(resolved.value.signaling.receive_dcs_code.value, 0o47);
    assert!(resolved.value.signaling.receive_dcs_code.inverted);
    assert_eq!(
        resolved.value.signaling.transmit_mode,
        super::RadioSignalingMode::Dcs
    );
    assert_eq!(resolved.value.signaling.transmit_dcs_code.value, 0o47);

    let inherited = ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    assert_eq!(
        inherited.signaling.receive_ctcss_tones_tenths_hz,
        vec![1_000, 1_035]
    );
    assert_eq!(inherited.signaling.squelch_level, 500);
    assert_eq!(inherited.signaling.squelch_hysteresis, 3_000);
    assert_eq!(inherited.signaling.transmit_settle_ms, 500);
    assert_eq!(
        inherited.signaling.transmit_ctcss_turnoff_mode,
        super::CtcssTurnoffMode::PhaseShift
    );
}

#[test]
fn radio_signaling_defaults_match_native_radio_defaults_and_invalid_values_fall_back() {
    let defaults = ResolvedRadioSettings::resolve(
        &ConfigDocument::parse("[1000]\n[radio 1000]\n").unwrap(),
        &NodeId::new("1000").unwrap(),
    )
    .unwrap();
    assert!(defaults.warnings.is_empty());
    assert_eq!(
        defaults.value.signaling.receive_audio_source,
        super::RadioReceiveAudioSource::Flat
    );
    assert_eq!(
        defaults.value.signaling.carrier_source,
        super::RadioCarrierSource::Dsp
    );
    assert_eq!(
        defaults.value.signaling.receive_mode,
        super::RadioSignalingMode::Carrier
    );
    assert_eq!(
        defaults.value.signaling.receive_ctcss_tones_tenths_hz,
        vec![1_000]
    );
    assert_eq!(defaults.value.signaling.receive_dcs_code.value, 0o23);
    assert_eq!(defaults.value.signaling.vox_hang_ms, 2_000);
    assert_eq!(defaults.value.signaling.transmit_ctcss_level_dbfs, -24);
    assert_eq!(defaults.value.signaling.transmit_dcs_level_dbfs, -24);

    let invalid = ResolvedRadioSettings::resolve(
        &ConfigDocument::parse(
            "[1000]\n[radio]\nsquelch_level=1000\nctcss_source=bogus\nreceive_ctcss_tones_hz=100.1\ndcs_receive_code=028N\ntransmit_ctcss_turnoff_mode=bad\ntransmit_dcs_turnoff_duration_ms=100\n",
        )
        .unwrap(),
        &NodeId::new("1000").unwrap(),
    )
    .unwrap();
    assert_eq!(invalid.value.signaling.squelch_level, 500);
    assert_eq!(
        invalid.value.signaling.receive_subaudible_source,
        super::RadioSubaudibleSource::Dsp
    );
    assert_eq!(
        invalid.value.signaling.receive_ctcss_tones_tenths_hz,
        vec![1_000]
    );
    assert_eq!(invalid.value.signaling.receive_dcs_code.value, 0o23);
    assert_eq!(
        invalid.value.signaling.transmit_dcs_turnoff_duration_ms,
        180
    );
    assert_eq!(invalid.warnings.len(), 6);
}

#[test]
fn radio_enum_settings_resolve_supported_values_and_fall_back_on_unknown_values() {
    let carrier_sources = [
        ("disabled", RadioCarrierSource::Disabled),
        ("vox", RadioCarrierSource::Vox),
        ("cm119", RadioCarrierSource::Cm119),
        ("cm119_inverted", RadioCarrierSource::Cm119Inverted),
        ("parallel", RadioCarrierSource::Parallel),
        ("parallel_inverted", RadioCarrierSource::ParallelInverted),
    ];
    for (source, expected) in carrier_sources {
        let document = ConfigDocument::parse(&format!(
            "[1000]\n[radio 1000]\ncarrier_source={source}\nreceive_audio=invalid\nreceive_signaling=invalid\nctcss_override=yes\nnoise_filter=alternate\n"
        ))
        .unwrap();
        let settings = ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap())
            .unwrap()
            .value
            .signaling;
        assert_eq!(settings.carrier_source, expected);
        assert_eq!(settings.receive_audio_source, RadioReceiveAudioSource::Flat);
        assert_eq!(settings.receive_mode, RadioSignalingMode::Carrier);
        assert!(settings.receive_ctcss_override);
        assert_eq!(settings.noise_filter, RadioNoiseFilter::Alternate);
    }

    let invalid_noise_filter = ConfigDocument::parse(
        "[1000]\n[radio 1000]\nnoise_filter=unsupported\nradio_duplex_mode=full\n",
    )
    .unwrap();
    let settings =
        ResolvedRadioSettings::resolve(&invalid_noise_filter, &NodeId::new("1000").unwrap())
            .unwrap()
            .value
            .signaling;
    assert_eq!(settings.noise_filter, RadioNoiseFilter::Standard);
    assert_eq!(settings.radio_duplex_mode, RadioDuplexMode::Full);

    let invalid_sources = ConfigDocument::parse(
        "[1000]\n[radio 1000]\ncarrier_source=unsupported\nradio_duplex_mode=unsupported\n",
    )
    .unwrap();
    let settings = ResolvedRadioSettings::resolve(&invalid_sources, &NodeId::new("1000").unwrap())
        .unwrap()
        .value
        .signaling;
    assert_eq!(settings.carrier_source, RadioCarrierSource::Dsp);
    assert_eq!(settings.radio_duplex_mode, RadioDuplexMode::Half);
}

#[test]
fn cm119_hardware_settings_inherit_and_resolve_profile_and_gpio_modes() {
    let document = ConfigDocument::parse(
        "[radio]\ncm119_profile=sphusb\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out0\ncm119_gpio_2_mode=out1\ncm119_gpio_8_mode=out1\ncm119_clip_led_gpio=2\n[1000]\n[radio 1000]\ncm119_profile=nhrc\ncm119_ptt_inverted=no\ncm119_gpio_2_mode=in\ncm119_gpio_8_mode=out0\ncm119_clip_led_gpio=0\n[2000]\n",
    )
    .unwrap();

    let node = ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap()).unwrap();
    assert!(
        node.warnings.is_empty(),
        "supported CM119 settings must not be reported as unknown"
    );
    assert_eq!(node.value.cm119_profile, super::Cm119Profile::Nhrc);
    assert!(!node.value.cm119_ptt_inverted);
    assert_eq!(
        node.value.cm119_gpio_modes,
        [
            super::Cm119GpioMode::OutputLow,
            super::Cm119GpioMode::Input,
            super::Cm119GpioMode::Input,
            super::Cm119GpioMode::Input,
            super::Cm119GpioMode::Input,
            super::Cm119GpioMode::Input,
            super::Cm119GpioMode::Input,
            super::Cm119GpioMode::OutputLow,
        ]
    );
    assert_eq!(node.value.cm119_clip_led_gpio, None);

    let inherited = ResolvedRadioSettings::resolve(&document, &NodeId::new("2000").unwrap())
        .unwrap()
        .value;
    assert_eq!(inherited.cm119_profile, super::Cm119Profile::SphUsb);
    assert!(inherited.cm119_ptt_inverted);
    assert_eq!(
        inherited.cm119_gpio_modes[1],
        super::Cm119GpioMode::OutputHigh
    );
    assert_eq!(
        inherited.cm119_gpio_modes[7],
        super::Cm119GpioMode::OutputHigh
    );
    assert_eq!(inherited.cm119_clip_led_gpio, Some(2));
}

#[test]
fn cm119_gpio_modes_convert_to_adapter_masks() {
    let document = ConfigDocument::parse(
        "[radio]\ncm119_gpio_1_mode=out0\ncm119_gpio_2_mode=out1\ncm119_gpio_8_mode=out1\n[1000]\n",
    )
    .unwrap();
    let settings = ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;

    assert_eq!(
        settings.cm119_gpio_output_masks(),
        (0b1000_0011, 0b1000_0010)
    );
}

#[test]
fn invalid_radio_settings_warn_and_keep_safe_defaults() {
    let document = ConfigDocument::parse(
        "[radio]\ndevice_selection=invalid\ninput_extra_buffer_ms=20\ninput_device_channels=3\ncm119_profile=bad\ncm119_ptt_inverted=maybe\ncm119_gpio_8_mode=out2\ncm119_clip_led_gpio=9\n[1000]\n[radio 1000]\ninput_extra_buffer_ms=501\n",
    )
    .unwrap();

    let resolved =
        ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap()).unwrap();
    assert_eq!(resolved.value.input_extra_buffer_ms, 20);
    assert_eq!(resolved.value.input_device_channels, 1);
    assert_eq!(
        resolved.value.device_selection,
        super::RadioDeviceSelection::Exact
    );
    assert_eq!(resolved.value.cm119_profile, super::Cm119Profile::DudeUsb);
    assert!(!resolved.value.cm119_ptt_inverted);
    assert_eq!(
        resolved.value.cm119_gpio_modes[7],
        super::Cm119GpioMode::Input
    );
    assert_eq!(resolved.value.cm119_clip_led_gpio, None);
    assert_eq!(resolved.warnings.len(), 7);
}

#[test]
fn oversized_radio_device_identifier_is_ignored_with_warning() {
    let identifier = "x".repeat(256);
    let document = ConfigDocument::parse(&format!(
        "[radio]\ndevice_identifier={identifier}\n[1000]\n"
    ))
    .unwrap();

    let resolved =
        ResolvedRadioSettings::resolve(&document, &NodeId::new("1000").unwrap()).unwrap();
    assert!(resolved.value.device_identifier.is_empty());
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn language_setting_inherits_global_and_invalid_node_override_with_warning() {
    let document = ConfigDocument::parse(
        "[general]\nlanguage=fr-CA\n[1000]\n[2000]\nlanguage=de-DE\n[3000]\nlanguage=not_a_locale\n",
    )
    .unwrap();
    let resolve =
        |node| ResolvedNodeSettings::resolve(&document, &NodeId::new(node).unwrap()).unwrap();
    assert_eq!(resolve("1000").value.language, "fr-CA");
    assert_eq!(resolve("2000").value.language, "de-DE");
    let invalid = resolve("3000");
    assert_eq!(invalid.value.language, "fr-CA");
    assert!(
        invalid
            .warnings
            .iter()
            .any(|warning| warning.key == "language")
    );
}

#[test]
fn statpost_interval_defaults_and_rejects_out_of_range_values() {
    for (source, expected) in [
        ("[1000]\n", 60),
        ("[1000]\nstatpost_time=30\n", 30),
        ("[1000]\nstatpost_time=600\n", 600),
    ] {
        let value = ResolvedNodeSettings::resolve(
            &ConfigDocument::parse(source).unwrap(),
            &NodeId::new("1000").unwrap(),
        )
        .unwrap()
        .value;
        assert_eq!(value.statpost_time, expected);
    }

    let document =
        ConfigDocument::parse("[general]\nstatpost_time=90\n[1000]\nstatpost_time=29\n").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("1000").unwrap()).unwrap();
    assert_eq!(resolved.value.statpost_time, 90);
    assert!(resolved.warnings.iter().any(|warning| {
        warning.key == "statpost_time" && warning.message == "invalid option value"
    }));
}

#[test]
fn status_snapshot_interval_inherits_node_override_and_default() {
    let document = ConfigDocument::parse(
        "[general]\nstatus_snapshot_interval_ms=80\n[1000]\n[2000]\nstatus_snapshot_interval_ms=25\n[3000]\nstatus_snapshot_interval_ms=0\n",
    )
    .unwrap();

    let resolve = |node| {
        ResolvedNodeSettings::resolve(&document, &NodeId::new(node).unwrap())
            .unwrap()
            .value
            .status_snapshot_interval_ms
    };
    assert_eq!(resolve("1000"), 80);
    assert_eq!(resolve("2000"), 25);
    assert_eq!(resolve("3000"), 80);
    assert_eq!(
        ResolvedNodeSettings::resolve(
            &ConfigDocument::parse("[4000]\n").unwrap(),
            &NodeId::new("4000").unwrap()
        )
        .unwrap()
        .value
        .status_snapshot_interval_ms,
        50
    );
}

#[test]
fn invalid_optional_node_values_fall_back_and_courtesy_level_inherits() {
    let document = ConfigDocument::parse(&format!(
        "[1000]\ncallsign={}\nlink_allow_nodes=?\nlink_deny_nodes=?\nlink_lookup_method=?\n",
        "A".repeat(64)
    ))
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let settings = ResolvedNodeSettings::resolve(&document, &node).unwrap();
    assert_eq!(settings.warnings.len(), 4);
    assert_eq!(settings.value.callsign, "");
    assert_eq!(settings.value.link_allow_nodes, "");
    assert_eq!(settings.value.link_deny_nodes, "");
    assert_eq!(
        settings.value.link_lookup_method,
        super::LinkLookupMethod::Both
    );
    for (level, expected) in [("", -20), ("level_db=-15\n", -15), ("level_db=?\n", -20)] {
        let document = ConfigDocument::parse(&format!(
            "[1000]\n[courtesy]\n{level}[courtesy 1000 test]\ninput=receiver\n"
        ))
        .unwrap();
        assert_eq!(
            ResolvedCourtesySettings::resolve(&document, &node, "test")
                .unwrap()
                .value
                .level_db,
            expected
        );
    }
    for invalid in ["bad node".to_owned(), "[node]".to_owned(), "n".repeat(64)] {
        assert!(NodeId::new(invalid).is_err());
    }
}

#[test]
fn named_resolvers_keep_structural_validation_and_reject_absent_definitions() {
    let node = NodeId::new("1000").unwrap();
    let resolve = |family, document: &ConfigDocument| match family {
        "courtesy" => ResolvedCourtesySettings::resolve(document, &node, "test").map(|_| ()),
        "macro" => super::ResolvedMacroSettings::resolve(document, Some(&node), "test").map(|_| ()),
        "event" => ResolvedEventSettings::resolve(document, &node, "test").map(|_| ()),
        "permanent" => ResolvedPermanentLinkSettings::resolve(document, &node, "test").map(|_| ()),
        "template" => {
            super::ResolvedTemplateSettings::resolve(document, Some(&node), "test").map(|_| ())
        }
        _ => ResolvedScheduleSettings::resolve(document, &node, "test").map(|_| ()),
    };
    for (family, body) in [
        ("courtesy", "input=receiver\nremote_node=2000\n"),
        ("macro", "action=connect\n"),
        ("event", "at=daily 09:07\nmessage=TEST\ntemplate=missing\n"),
        ("event", "at=daily 09:07\n"),
        ("permanent", "remote_node=1000\n"),
        ("permanent", "remote_node=invalid\n"),
        ("template", ""),
        ("schedule", "remote_node=2000\n"),
    ] {
        let document =
            ConfigDocument::parse(&format!("[1000]\n[{family} 1000 test]\n{body}")).unwrap();
        let error = resolve(family, &document).unwrap_err();
        assert!(matches!(error, ConfigError::Structure { .. }));
        assert!(resolve(family, &ConfigDocument::parse("[1000]\n").unwrap()).is_err());
    }
}

#[test]
fn media_settings_respect_named_family_flat_and_invalid_override_precedence() {
    let node = NodeId::new("1000").unwrap();
    for family in ["identifier", "announcement", "courtesy"] {
        for (stage, speed, frequency, speech_speed, model) in [
            (0, 23, 603, 113, "flat.onnx"),
            (1, 22, 602, 112, "family.onnx"),
            (2, 21, 601, 111, "named.onnx"),
            (3, 23, 603, 113, "family.onnx"),
        ] {
            let mut source = format!(
                "[1000]\n[{family}]\nmorse_speed_wpm=23\nmorse_frequency_hz=603\nmorse_level_db=-7\nspeech_speed_percent=113\nspeech_level_db=-8\nspeech_model=flat.onnx\n"
            );
            if stage > 0 {
                source.push_str(if stage == 3 {
                    "[morse]\nspeed_wpm=invalid\nfrequency_hz=invalid\nlevel_db=invalid\n[speech]\nvoice=family.onnx\nspeed_percent=invalid\nlevel_db=invalid\n"
                } else {
                    "[morse]\nspeed_wpm=22\nfrequency_hz=602\nlevel_db=-5\n[speech]\nvoice=family.onnx\nspeed_percent=112\nlevel_db=-6\n"
                });
            }
            source.push_str(&format!("[{family} 1000 named]\nmorse_text=TEST\n"));
            if family == "courtesy" {
                source.push_str("input=link\nremote_node=2000\nlevel_db=-9\n");
            }
            if family == "announcement" {
                source.push_str("interval_ms=100\n");
            }
            if stage == 2 {
                source.push_str("morse_speed_wpm=21\nmorse_frequency_hz=601\nmorse_level_db=-3\nspeech_speed_percent=111\nspeech_level_db=-4\nspeech_model=named.onnx\n");
            } else if stage == 3 {
                source.push_str("morse_speed_wpm=invalid\nmorse_frequency_hz=invalid\nmorse_level_db=invalid\nspeech_speed_percent=invalid\nspeech_level_db=invalid\n");
            }
            let document = ConfigDocument::parse(&source).unwrap();
            let (
                actual_speed,
                actual_frequency,
                actual_speech_speed,
                actual_model,
                morse_level,
                speech_level,
            ) = match family {
                "identifier" => {
                    let value =
                        ResolvedIdentifierSettings::resolve(&document, &node, Some("named"))
                            .unwrap()
                            .value;
                    (
                        value.morse_speed_wpm,
                        value.morse_frequency_hz,
                        value.speech_speed_percent,
                        value.speech_model,
                        value.morse_level_db,
                        value.speech_level_db,
                    )
                }
                "announcement" => {
                    let value =
                        ResolvedAnnouncementSettings::resolve(&document, &node, Some("named"))
                            .unwrap()
                            .value;
                    (
                        value.morse_speed_wpm,
                        value.morse_frequency_hz,
                        value.speech_speed_percent,
                        value.speech_model,
                        value.morse_level_db,
                        value.speech_level_db,
                    )
                }
                _ => {
                    let value = ResolvedCourtesySettings::resolve(&document, &node, "named")
                        .unwrap()
                        .value;
                    assert_eq!(value.remote_node, "2000");
                    (
                        value.morse_speed_wpm,
                        value.morse_frequency_hz,
                        value.speech_speed_percent,
                        value.speech_model,
                        value.morse_level_db,
                        value.speech_level_db,
                    )
                }
            };
            assert_eq!(
                (
                    actual_speed,
                    actual_frequency,
                    actual_speech_speed,
                    actual_model.as_str()
                ),
                (speed, frequency, speech_speed, model),
                "{family}, stage {stage}"
            );
            let expected = if family == "courtesy" {
                (-9, 0)
            } else {
                match stage {
                    1 => (-5, -6),
                    2 => (-3, -4),
                    _ => (-7, -8),
                }
            };
            assert_eq!((morse_level, speech_level), expected);
        }
    }
}

#[test]
fn dedicated_media_family_settings_apply_node_overrides_and_warn_before_fallback() {
    let node = NodeId::new("1000").unwrap();
    for (overrides, frequency, speed, level, speech_speed, speech_level, format) in [
        (
            "frequency_hz=901\nspeed_wpm=31\nlevel_db=-11",
            901,
            31,
            -11,
            151,
            -13,
            24,
        ),
        (
            "frequency_hz=bad\nspeed_wpm=bad\nlevel_db=bad",
            902,
            32,
            -12,
            152,
            -14,
            12,
        ),
    ] {
        let invalid = overrides.contains("bad");
        let speech = if invalid {
            "speed_percent=bad\nlevel_db=bad"
        } else {
            "speed_percent=151\nlevel_db=-13"
        };
        let time = if invalid { "invalid" } else { "24" };
        let source = format!(
            "[1000]\n[morse]\nfrequency_hz=902\nspeed_wpm=32\nlevel_db=-12\n[morse 1000]\n{overrides}\n[speech]\nspeed_percent=152\nlevel_db=-14\n[speech 1000]\n{speech}\n[time 1000]\nformat={time}\n"
        );
        let document = ConfigDocument::parse(&source).unwrap();
        let morse = ResolvedMorseSettings::resolve(&document, &node).unwrap();
        assert_eq!(
            (
                morse.value.frequency_hz,
                morse.value.speed_wpm,
                morse.value.level_db
            ),
            (frequency, speed, level)
        );
        let speech = ResolvedSpeechSettings::resolve(&document, &node).unwrap();
        assert_eq!(
            (speech.value.speed_percent, speech.value.level_db),
            (speech_speed, speech_level)
        );
        assert_eq!(
            ResolvedTimeSettings::resolve(&document, &node)
                .unwrap()
                .value
                .format,
            format
        );
        assert_eq!(!morse.warnings.is_empty(), invalid);
    }
}

#[test]
fn invalid_macro_overrides_resolve_to_valid_inherited_values() {
    let node = NodeId::new("524950").unwrap();
    for option in [
        "action=invalid",
        "target_node=invalid",
        "target_node=1234567890123456789012345678901234567890123456789012345678901234",
    ] {
        let document = ConfigDocument::parse(&format!(
            "[524950]\n[macro connect]\naction=connect\ntarget_node=123\n[macro 524950 connect]\n{option}\n"
        )).unwrap();
        let resolved =
            super::ResolvedMacroSettings::resolve(&document, Some(&node), "connect").unwrap();
        assert_eq!(resolved.warnings.len(), 1);
        assert_eq!(
            resolved.value.action(),
            crate::schedule::ScheduledAction::Connect
        );
        assert_eq!(resolved.value.target_node, "123");
    }
}

#[test]
fn invalid_courtesy_remote_resolves_to_unassigned_input() {
    let node = NodeId::new("524950").unwrap();
    for input in ["receiver", "link"] {
        let document = ConfigDocument::parse(&format!(
            "[524950]\n[courtesy 524950 tail]\ninput={input}\nremote_node=invalid\n"
        ))
        .unwrap();
        let resolved = ResolvedCourtesySettings::resolve(&document, &node, "tail").unwrap();
        assert_eq!(resolved.warnings.len(), 1);
        assert_eq!(resolved.value.remote_node, "");
    }
}

#[test]
fn oversized_priority_resolves_to_inherited_priority() {
    let document = ConfigDocument::parse(
        "[524950]\n[identifier]\npriority=7\n[identifier 524950]\npriority=2147483648\n",
    )
    .unwrap();
    let resolved =
        ResolvedIdentifierSettings::resolve(&document, &NodeId::new("524950").unwrap(), None)
            .unwrap();
    assert_eq!(resolved.warnings.len(), 1);
    assert_eq!(resolved.value.priority, 7);
}

#[test]
fn unknown_fixed_command_options_cannot_disable_builtin_commands() {
    let document = ConfigDocument::parse("[524950]\nfixed_10=\nfixed_722=\n").unwrap();
    let resolved =
        ResolvedNodeSettings::resolve(&document, &NodeId::new("524950").unwrap()).unwrap();
    assert_eq!(resolved.warnings.len(), 2);
    assert_eq!(resolved.value.link_commands["fixed_10"], "10");
    assert_eq!(resolved.value.link_commands["fixed_722"], "722");
}

#[test]
fn parses_owned_sections_and_options_without_borrowing_input() {
    let source = String::from("[general]\nnode_enabled = yes\n\n[node]\nradio_channel = usb\n");
    let document = ConfigDocument::parse(&source).expect("valid document");
    drop(source);

    assert_eq!(document.entries().len(), 2);
    assert_eq!(document.entries()[0].section, "general");
    assert_eq!(document.entries()[0].key, "node_enabled");
    assert_eq!(document.entries()[0].value, "yes");
}

#[test]
fn parser_rejects_malformed_lines_and_options_before_sections() {
    let cases = [
        "node_enabled = yes\n",
        "[general\n",
        "[general] trailing\n",
        "[general]\n= yes\n",
    ];
    for source in cases {
        assert!(matches!(
            ConfigDocument::parse(source),
            Err(ConfigError::Syntax { .. })
        ));
    }
}

#[test]
fn schema_warns_unknown_options_and_retired_media_options_are_not_special() {
    let document = ConfigDocument::parse(
        "[node]\nnode_enabled = maybe\nsample_rate_hz = 8000\ncodec = ulaw\nunknown = yes\n",
    )
    .expect("syntax is valid");
    let result = Schema::validate(&document).expect("recoverable options use defaults");
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.key == "unknown")
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.key == "sample_rate_hz")
    );
    assert!(result.warnings.iter().any(|warning| warning.key == "codec"));
}

#[test]
fn node_resolution_uses_scoped_values_over_defaults_regardless_of_file_order() {
    let document = ConfigDocument::parse(
        "[node]\ntransmit_hang_ms = 77\nfull_duplex = no\n[general]\ntransmit_hang_ms = 11\nfull_duplex = yes\n",
    )
    .expect("valid document");
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap())
        .expect("valid settings");
    assert_eq!(resolved.value.hang_ms, 77);
    assert!(!resolved.value.full_duplex);
}

#[test]
fn unknown_or_invalid_values_warn_and_retain_defaults() {
    let document = ConfigDocument::parse(
        "[general]\nfull_duplex = maybe\ntransmit_hang_ms = invalid\nnode_enabled = invalid\n",
    )
    .expect("valid document");
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap())
        .expect("invalid values are recoverable");
    assert!(resolved.value.enabled);
    assert!(resolved.value.full_duplex);
    assert_eq!(resolved.value.hang_ms, 0);
    assert_eq!(resolved.warnings.len(), 3);
}

#[test]
fn structural_schema_errors_are_not_recovered() {
    let document =
        ConfigDocument::parse("[template greeting]\ntext=one\n[template greeting]\ntext=two\n")
            .expect("valid syntax");
    assert!(matches!(
        Schema::validate(&document),
        Err(ConfigError::Structure { .. })
    ));
}

#[test]
fn scoped_named_sections_accept_single_spaces_and_unknown_sections_warn() {
    let document = ConfigDocument::parse(
        "[usb]\n[identifier usb welcome]\ninterval_ms = 60000\n[future extension]\nvalue = yes\n",
    )
    .expect("valid syntax");
    let result = Schema::validate(&document).expect("unknown input is recoverable");
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.section == "future extension")
    );
}

#[test]
fn node_override_precedes_general_and_invalid_node_value_falls_back_to_general() {
    let document = ConfigDocument::parse(
        "[general]\ntransmit_hang_ms = 11\n[node]\ntransmit_hang_ms = invalid\nfull_duplex = no\n",
    )
    .expect("valid syntax");
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap())
        .expect("invalid value is recoverable");
    assert_eq!(resolved.value.hang_ms, 11);
    assert!(!resolved.value.full_duplex);
}

#[test]
fn squelch_delay_inherits_and_defaults_to_zero() {
    let node = NodeId::new("node").unwrap();
    let defaults =
        ResolvedNodeSettings::resolve(&ConfigDocument::parse("[node]\n").unwrap(), &node)
            .unwrap()
            .value;
    assert_eq!(defaults.squelch_delay_ms, 0);
    let document =
        ConfigDocument::parse("[general]\nsquelch_delay_ms=20\n[node]\nsquelch_delay_ms=40\n")
            .unwrap();
    assert_eq!(
        ResolvedNodeSettings::resolve(&document, &node)
            .unwrap()
            .value
            .squelch_delay_ms,
        40
    );
}

#[test]
fn activity_ctcss_defaults_off_and_bounds_its_hang_to_transmit_hang() {
    let node = NodeId::new("node").unwrap();
    let defaults =
        ResolvedNodeSettings::resolve(&ConfigDocument::parse("[node]\n").unwrap(), &node)
            .unwrap()
            .value;
    assert!(!defaults.ctcss_encode_on_input);
    assert_eq!(defaults.ctcss_hang_ms, 0);

    let document = ConfigDocument::parse(
        "[general]\ntransmit_hang_ms=500\nctcss_encode_on_input=yes\nctcss_hang_ms=600\n[node]\ntransmit_hang_ms=250\n",
    )
    .unwrap();
    let resolved = ResolvedNodeSettings::resolve(&document, &node).unwrap();
    assert!(resolved.value.ctcss_encode_on_input);
    assert_eq!(resolved.value.hang_ms, 250);
    assert_eq!(resolved.value.ctcss_hang_ms, 0);
    assert!(
        resolved
            .warnings
            .iter()
            .any(|warning| { warning.key == "ctcss_hang_ms" && warning.fallback == "0" })
    );
}

#[test]
fn activity_ctcss_hang_accepts_valid_global_value_and_ignores_invalid_value() {
    let node = NodeId::new("1000").unwrap();
    let valid =
        ConfigDocument::parse("[general]\ntransmit_hang_ms=250\nctcss_hang_ms=200\n[1000]\n")
            .unwrap();
    assert_eq!(
        ResolvedNodeSettings::resolve(&valid, &node)
            .unwrap()
            .value
            .ctcss_hang_ms,
        200
    );

    let invalid =
        ConfigDocument::parse("[general]\ntransmit_hang_ms=250\nctcss_hang_ms=invalid\n[1000]\n")
            .unwrap();
    assert_eq!(
        ResolvedNodeSettings::resolve(&invalid, &node)
            .unwrap()
            .value
            .ctcss_hang_ms,
        0
    );
}

#[test]
fn parrot_enabled_defaults_off_and_inherits_with_node_precedence() {
    let node = NodeId::new("1000").unwrap();
    let resolved = |text: &str| {
        ResolvedNodeSettings::resolve(&ConfigDocument::parse(text).unwrap(), &node)
            .unwrap()
            .value
            .parrot_enabled
    };
    assert!(!resolved("[1000]\n"));
    assert!(resolved("[general]\nparrot_enabled=yes\n[1000]\n"));
    assert!(!resolved(
        "[general]\nparrot_enabled=yes\n[1000]\nparrot_enabled=no\n"
    ));
    assert!(resolved(
        "[general]\nparrot_enabled=yes\n[1000]\nparrot_enabled=invalid\n"
    ));
}

#[test]
fn parrot_dtmf_commands_and_admin_credentials_inherit_per_node() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let hash = Argon2::default()
        .hash_password(b"test-code", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    let node = NodeId::new("1000").unwrap();
    let inherited = ResolvedNodeSettings::resolve(
        &ConfigDocument::parse(&format!(
            "[general]\ndtmf_admin_unlock_hash={hash}\ndtmf_admin_lock_hash={hash}\n[1000]\n"
        ))
        .unwrap(),
        &node,
    )
    .unwrap()
    .value;
    assert_eq!(inherited.dtmf_admin_unlock_hash, hash);
    assert_eq!(inherited.dtmf_admin_lock_hash, hash);
    assert_eq!(inherited.dtmf_admin_timeout_ms, 300_000);
    let mappings = inherited.command_map().unwrap();
    for (prefix, action) in [
        ("800", crate::command::LinkAction::AdminUnlock),
        ("801", crate::command::LinkAction::AdminLock),
        ("804", crate::command::LinkAction::ParrotEnable),
        ("805", crate::command::LinkAction::ParrotDisable),
    ] {
        assert!(
            mappings
                .mappings()
                .iter()
                .any(|mapping| { mapping.digits == prefix && mapping.action == action })
        );
    }
    let overridden = ResolvedNodeSettings::resolve(
        &ConfigDocument::parse(&format!(
            "[general]\ndtmf_admin_unlock_hash={hash}\ndtmf_admin_lock_hash={hash}\n[1000]\ndtmf_admin_timeout_ms=1234\nlink_command_parrot_enable=77\n"
        ))
        .unwrap(),
        &node,
    )
    .unwrap()
    .value;
    assert_eq!(overridden.dtmf_admin_timeout_ms, 1234);
    assert!(
        overridden
            .command_map()
            .unwrap()
            .mappings()
            .iter()
            .any(|mapping| mapping.digits == "77")
    );
}

#[test]
fn identifier_set_overrides_node_and_flat_defaults_including_empty_media() {
    let document = ConfigDocument::parse(
        "[node]\n[identifier]\ninterval_ms=600000\nsound_file=flat.wav\n[identifier node]\ninterval_ms=120000\nsound_file=node.wav\n[identifier node id]\ninterval_ms=60000\nsound_file=\n",
    )
    .expect("valid syntax");
    let resolved =
        ResolvedIdentifierSettings::resolve(&document, &NodeId::new("node").unwrap(), Some("id"))
            .expect("valid identifier");
    assert_eq!(resolved.value.interval_ms, 60_000);
    assert!(resolved.value.sound_file.is_empty());
}

#[test]
fn announcement_zero_default_and_named_empty_value_override() {
    let document = ConfigDocument::parse(
        "[node]\n[announcement]\nsound_file=flat.wav\n[announcement node release]\nsound_file=\n",
    )
    .expect("valid syntax");
    let resolved = ResolvedAnnouncementSettings::resolve(
        &document,
        &NodeId::new("node").unwrap(),
        Some("release"),
    )
    .expect("valid announcement");
    assert_eq!(resolved.value.interval_ms, 0);
    assert!(resolved.value.sound_file.is_empty());
}

#[test]
fn courtesy_requires_link_for_remote_and_resolves_its_common_level() {
    let document = ConfigDocument::parse(
        "[node]\n[courtesy]\nspeech_text=flat\n[speech node]\nvoice=node.onnx\nspeed_percent=140\n[morse node]\nfrequency_hz=950\n[courtesy node north]\ninput=link\nremote_node=123\nlevel_db=-12\ntone_sequence=1,2\n",
    )
    .expect("valid syntax");
    let resolved =
        ResolvedCourtesySettings::resolve(&document, &NodeId::new("node").unwrap(), "north")
            .expect("valid courtesy");
    assert_eq!(resolved.value.remote_node, "123");
    assert_eq!(resolved.value.level_db, -12);
    assert_eq!(resolved.value.speech_text, "flat");
    assert_eq!(resolved.value.speech_model, "node.onnx");
    assert_eq!(resolved.value.speech_speed_percent, 140);
    assert_eq!(resolved.value.speech_level_db, 0);
    assert_eq!(resolved.value.morse_frequency_hz, 950);
    assert_eq!(resolved.value.morse_level_db, -12);
    assert_eq!(resolved.value.tone_sequence, "1,2");
}

#[test]
fn morse_speech_and_time_use_node_over_flat_defaults() {
    let document = ConfigDocument::parse(
        "[node]\n[morse]\nfrequency_hz=800\n[morse node]\nfrequency_hz=1000\n[speech]\nvoice=flat.onnx\n[speech node]\nvoice=node.onnx\n[time]\nformat=12\n[time node]\nformat=24\n",
    )
    .expect("valid syntax");
    let node = NodeId::new("node").unwrap();
    assert_eq!(
        ResolvedMorseSettings::resolve(&document, &node)
            .unwrap()
            .value
            .frequency_hz,
        1000
    );
    assert_eq!(
        ResolvedSpeechSettings::resolve(&document, &node)
            .unwrap()
            .value
            .voice,
        "node.onnx"
    );
    assert_eq!(
        ResolvedTimeSettings::resolve(&document, &node)
            .unwrap()
            .value
            .format,
        24
    );
}

#[test]
fn settings_families_fall_back_after_invalid_node_values() {
    let document = ConfigDocument::parse(
        "[node]\n[identifier]\ninterval_ms=120000\npolite=yes\n[identifier node]\ninterval_ms=invalid\npolite=invalid\n[speech]\nspeed_percent=120\n[speech node]\nspeed_percent=invalid\n[morse]\nfrequency_hz=900\n[morse node]\nfrequency_hz=invalid\n[time]\nformat=24\n[time node]\nformat=invalid\n",
    )
    .expect("valid syntax");
    let node = NodeId::new("node").unwrap();
    let identifier = ResolvedIdentifierSettings::resolve(&document, &node, None).unwrap();
    let announcement = ResolvedAnnouncementSettings::resolve(&document, &node, None).unwrap();
    let morse = ResolvedMorseSettings::resolve(&document, &node).unwrap();
    let speech = ResolvedSpeechSettings::resolve(&document, &node).unwrap();
    let time = ResolvedTimeSettings::resolve(&document, &node).unwrap();

    assert_eq!(identifier.value.interval_ms, 120_000);
    assert!(identifier.value.polite);
    assert_eq!(identifier.value.speech_speed_percent, 120);
    assert_eq!(identifier.value.morse_frequency_hz, 900);
    assert_eq!(announcement.value.speech_speed_percent, 120);
    assert_eq!(announcement.value.morse_frequency_hz, 900);
    assert_eq!(morse.value.frequency_hz, 900);
    assert_eq!(speech.value.speed_percent, 120);
    assert_eq!(time.value.format, 24);
}

#[test]
fn identifier_and_announcement_resolve_all_media_defaults() {
    let document = ConfigDocument::parse(
        "[node]\n[identifier]\nfirst_key_only=yes\nregardless_of_activity=yes\npolite=yes\npolite_maximum_wait_ms=70000\n[speech]\nvoice=flat.onnx\nspeed_percent=125\nlevel_db=-4\n[morse]\nspeed_wpm=25\nfrequency_hz=850\nlevel_db=-8\n[identifier node set]\nspeech_model=set.onnx\n[announcement node set]\nspeech_model=announcement.onnx\nmorse_speed_wpm=30\n",
    )
    .expect("valid syntax");
    let node = NodeId::new("node").unwrap();
    let identifier = ResolvedIdentifierSettings::resolve(&document, &node, Some("set")).unwrap();
    let announcement =
        ResolvedAnnouncementSettings::resolve(&document, &node, Some("set")).unwrap();

    assert!(identifier.value.first_key_only);
    assert!(identifier.value.regardless_of_activity);
    assert!(identifier.value.polite);
    assert_eq!(identifier.value.polite_maximum_wait_ms, 70_000);
    assert_eq!(identifier.value.speech_model, "set.onnx");
    assert_eq!(identifier.value.speech_speed_percent, 125);
    assert_eq!(identifier.value.speech_level_db, -4);
    assert_eq!(identifier.value.morse_speed_wpm, 25);
    assert_eq!(identifier.value.morse_frequency_hz, 850);
    assert_eq!(identifier.value.morse_level_db, -8);
    assert_eq!(announcement.value.speech_model, "announcement.onnx");
    assert_eq!(announcement.value.morse_speed_wpm, 30);
    assert_eq!(announcement.value.morse_frequency_hz, 850);
}

#[test]
fn named_document_enumeration_preserves_source_order() {
    let document = ConfigDocument::parse(
        "[usb]\n[template first]\ntext=one\n[template usb second]\ntext=two\n[event usb morning]\nat=daily 08:00\nmessage=hello\n",
    )
    .unwrap();
    assert_eq!(document.nodes(), ["usb"]);
    assert_eq!(
        document.named_sections("template", None),
        ["template first"]
    );
    assert_eq!(
        document.named_sections("template", Some("usb")),
        ["template usb second"]
    );
    assert_eq!(
        document.named_sections("event", Some("usb")),
        ["event usb morning"]
    );
}

#[test]
fn event_permanent_and_schedule_resolve_required_declarations() {
    let document = ConfigDocument::parse("[node]\n[event node morning]\nat=daily 08:00\nmessage=hello\n[permanent node primary]\nremote_node=123\n[schedule node net]\nremote_node=456\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\n").unwrap();
    let node = NodeId::new("node").unwrap();
    assert_eq!(
        ResolvedEventSettings::resolve(&document, &node, "morning")
            .unwrap()
            .value
            .at,
        "daily 08:00"
    );
    assert_eq!(
        ResolvedPermanentLinkSettings::resolve(&document, &node, "primary")
            .unwrap()
            .value
            .remote_nodes[0],
        "123"
    );
    assert_eq!(
        ResolvedScheduleSettings::resolve(&document, &node, "net")
            .unwrap()
            .value
            .replace_permanent,
        "primary"
    );
}

#[test]
fn permanent_group_resolution_preserves_the_configured_priority_order() {
    let source = "[524950]\n[permanent 524950 blind-hams]\nremote_node=506315,506312,506310,506311,506313,506314\ngroup_name=The Blind Hams Network\n";
    let document = ConfigDocument::parse(source).unwrap();
    let resolved = ResolvedPermanentLinkSettings::resolve(
        &document,
        &NodeId::new("524950").unwrap(),
        "blind-hams",
    )
    .unwrap();
    assert!(resolved.warnings.is_empty());
    assert_eq!(
        resolved.value.remote_nodes,
        ["506315", "506312", "506310", "506311", "506313", "506314"]
    );
    assert_eq!(
        resolved.value.group_name.as_deref(),
        Some("The Blind Hams Network")
    );
}

#[test]
fn invalid_optional_permanent_group_name_resolves_as_unset() {
    let source = format!(
        "[1000]\n[permanent 1000 net]\nremote_node=2000\ngroup_name={}\n",
        "x".repeat(64)
    );
    let document = ConfigDocument::parse(&source).unwrap();
    let resolved =
        ResolvedPermanentLinkSettings::resolve(&document, &NodeId::new("1000").unwrap(), "net")
            .unwrap();
    assert_eq!(resolved.value.group_name, None);
}

#[test]
fn blank_optional_permanent_group_name_resolves_as_unset() {
    let document =
        ConfigDocument::parse("[1000]\n[permanent 1000 net]\nremote_node=2000\ngroup_name=   \n")
            .unwrap();
    let resolved =
        ResolvedPermanentLinkSettings::resolve(&document, &NodeId::new("1000").unwrap(), "net")
            .unwrap();
    assert_eq!(resolved.value.group_name, None);
}

#[test]
fn schedule_resolution_retains_selectors_and_inactivity_grace() {
    let document = ConfigDocument::parse(
        "[node]\n[permanent node primary]\nremote_node=123\n[schedule node net]\nremote_node=456\nreplace_permanent=primary\ndays=Monday-Friday\ndates=\nstart_time=11:00\nend_time=23:59\nend_inactivity_ms=300000\n",
    )
    .unwrap();
    let value = ResolvedScheduleSettings::resolve(&document, &NodeId::new("node").unwrap(), "net")
        .unwrap()
        .value;
    assert_eq!(value.days, "Monday-Friday");
    assert!(value.dates.is_empty());
    assert_eq!(value.end_time, "23:59");
    assert_eq!(value.end_inactivity_ms, 300_000);
    assert!(value.warning_before_start_ms.is_empty());
    assert!(value.warning_before_end_ms.is_empty());
    assert_eq!(value.warning_message_id, None);
}

#[test]
fn schedule_resolution_preserves_group_name_and_remote_priority() {
    let document = ConfigDocument::parse(
        "[node]\n[permanent node primary]\nremote_node=123\n[schedule node net]\nremote_node=456, 789\ngroup_name=Regional Net\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\n",
    )
    .unwrap();
    let value = ResolvedScheduleSettings::resolve(&document, &NodeId::new("node").unwrap(), "net")
        .unwrap()
        .value;
    assert_eq!(value.remote_nodes, ["456", "789"]);
    assert_eq!(value.group_name.as_deref(), Some("Regional Net"));

    for group_name in [String::new(), "x".repeat(64)] {
        let invalid_optional = ConfigDocument::parse(&format!(
            "[node]\n[permanent node primary]\nremote_node=123\n[schedule node net]\nremote_node=456\ngroup_name={group_name}\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\n"
        ))
        .unwrap();
        let resolved = ResolvedScheduleSettings::resolve(
            &invalid_optional,
            &NodeId::new("node").unwrap(),
            "net",
        )
        .unwrap();
        assert_eq!(resolved.value.group_name, None);
    }
}

#[test]
fn schedule_resolution_preserves_ordered_warning_leads_and_message_id() {
    let document = ConfigDocument::parse(
        "[node]\n[permanent node primary]\nremote_node=123\n[schedule node net]\nremote_node=456\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\nwarning_before_start_ms=3600000,60000\nwarning_before_end_ms=60000,30000\nwarning_message_id=scheduled-link-change\n",
    )
    .unwrap();
    let value = ResolvedScheduleSettings::resolve(&document, &NodeId::new("node").unwrap(), "net")
        .unwrap()
        .value;
    assert_eq!(value.warning_before_start_ms, [3_600_000, 60_000]);
    assert_eq!(value.warning_before_end_ms, [60_000, 30_000]);
    assert_eq!(
        value.warning_message_id.as_deref(),
        Some("scheduled-link-change")
    );
}

#[test]
fn schedule_resolution_uses_empty_defaults_for_invalid_optional_selectors() {
    let document = ConfigDocument::parse(
        "[node]\n[permanent node primary]\nremote_node=123\n[schedule node net]\nremote_node=456\nreplace_permanent=primary\ndays=Funday\ndates=not-a-date\nstart_time=11:00\nend_time=12:00\n",
    )
    .unwrap();
    let resolved =
        ResolvedScheduleSettings::resolve(&document, &NodeId::new("node").unwrap(), "net").unwrap();

    assert!(resolved.value.days.is_empty());
    assert!(resolved.value.dates.is_empty());
    assert_eq!(resolved.warnings.len(), 2);
}

#[test]
fn command_map_has_all_defaults_and_rejects_node_taking_prefix_collisions() {
    let document = ConfigDocument::parse("[node]\n").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap()).unwrap();
    assert_eq!(resolved.value.link_commands.len(), 20);
    assert_eq!(resolved.value.link_commands["fixed_10"], "10");
    assert_eq!(resolved.value.link_commands["fixed_722"], "722");
    let conflict = ConfigDocument::parse("[node]\nlink_command_disconnect=3\n").unwrap();
    assert!(ResolvedNodeSettings::resolve(&conflict, &NodeId::new("node").unwrap()).is_err());
}

#[test]
fn telemetry_duck_override_uses_bounded_signed_decibels() {
    for (raw, expected) in [("-60", -60), ("-17", -17), ("0", 0)] {
        let document =
            ConfigDocument::parse(&format!("[node]\ntelemetry_duck_db={raw}\n")).unwrap();
        let resolved =
            ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap()).unwrap();
        assert_eq!(resolved.value.telemetry_duck_db, expected);
        assert!(resolved.warnings.is_empty());
    }
}

#[test]
fn global_template_and_macro_can_be_resolved_without_a_node() {
    use super::{ResolvedMacroSettings, ResolvedTemplateSettings};
    let document = ConfigDocument::parse(
        "[template greeting]\ntext=hello\n[macro connect]\naction=connect\ntarget_node=2000\n",
    )
    .unwrap();
    assert_eq!(
        ResolvedTemplateSettings::resolve(&document, None, "greeting")
            .unwrap()
            .value
            .text,
        "hello"
    );
    assert_eq!(
        ResolvedMacroSettings::resolve(&document, None, "connect")
            .unwrap()
            .value
            .target_node,
        "2000"
    );
}

#[test]
fn command_prefix_overlap_depends_on_the_longer_mapping() {
    let cases = [
        ("link_command_status", "7", "fixed_722", "72", true),
        (
            "link_command_status",
            "7",
            "link_command_disconnect",
            "72",
            false,
        ),
        (
            "link_command_disconnect",
            "72",
            "link_command_status",
            "7",
            false,
        ),
        (
            "link_command_status",
            "7",
            "link_command_full_status",
            "7",
            false,
        ),
    ];
    for (left_key, left, right_key, right, valid) in cases {
        let commands = BTreeMap::from([
            (left_key.to_owned(), left.to_owned()),
            (right_key.to_owned(), right.to_owned()),
        ]);
        assert_eq!(validate_command_map(&commands).is_ok(), valid);
    }
}
#[test]
fn resolved_command_map_preserves_configured_and_fixed_actions() {
    let document = super::ConfigDocument::parse("[524950]\nlink_command_transceive=9\n").unwrap();
    let node = super::NodeId::new("524950").unwrap();
    let settings = super::ResolvedNodeSettings::resolve(&document, &node)
        .unwrap()
        .value;
    let map = settings.command_map().unwrap();
    assert_eq!(
        map.parse("92000").unwrap().action,
        crate::command::LinkAction::Transceive
    );
    assert_eq!(
        map.parse("722").unwrap().action,
        crate::command::LinkAction::Time
    );
    assert_eq!(
        map.parse("806").unwrap().action,
        crate::command::LinkAction::DisconnectPermanentAll
    );
    assert_eq!(
        map.parse("816").unwrap().action,
        crate::command::LinkAction::ReconnectPermanentAll
    );
}
