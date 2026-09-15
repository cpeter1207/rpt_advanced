use super::{
    ConfigDocument, ConfigError, NodeId, ResolvedAnnouncementSettings, ResolvedCourtesySettings,
    ResolvedEventSettings, ResolvedIdentifierSettings, ResolvedMorseSettings, ResolvedNodeSettings,
    ResolvedPermanentLinkSettings, ResolvedScheduleSettings, ResolvedSpeechSettings,
    ResolvedTimeSettings, Schema, validate_command_map,
};
use std::collections::BTreeMap;

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
            .remote_node,
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
    assert_eq!(resolved.value.link_commands.len(), 16);
    assert_eq!(resolved.value.link_commands["fixed_10"], "10");
    assert_eq!(resolved.value.link_commands["fixed_722"], "722");
    let conflict = ConfigDocument::parse("[node]\nlink_command_disconnect=3\n").unwrap();
    assert!(ResolvedNodeSettings::resolve(&conflict, &NodeId::new("node").unwrap()).is_err());
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
