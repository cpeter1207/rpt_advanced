use super::{
    ConfigDocument, ConfigError, NodeId, ResolvedAnnouncementSettings, ResolvedCourtesySettings,
    ResolvedEventSettings, ResolvedIdentifierSettings, ResolvedMorseSettings, ResolvedNodeSettings,
    ResolvedPermanentLinkSettings, ResolvedScheduleSettings, ResolvedSpeechSettings,
    ResolvedTimeSettings, Schema,
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
}
