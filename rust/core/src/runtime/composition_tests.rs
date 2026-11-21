use super::*;
use crate::{
    composition::{Composition, MorseRequirement},
    config::{ConfigDocument, NodeId, ResolvedMessageSettings},
    messages::{MessageArgument, MessageForms},
    schedule::Weekday,
    site::{Observation, ReadingQuality},
};
use std::sync::Arc;

#[test]
fn composition_site_bindings_warn_only_for_missing_source_kind() {
    for kind in ["sensor", "input"] {
        for declaration in ["", "sensor", "input"] {
            let section = if declaration.is_empty() {
                String::new()
            } else {
                // A disabled definition without a DTMF selector is still configured.
                format!("[{declaration} 524950 reading]\nenabled=no\n")
            };
            let document = ConfigDocument::parse(&format!(
                "[524950]\n[message root]\ncontent=Repeater. {{message_macro_value}}\n[message value]\nmessage_id=site-temperature\narg_temperature={kind}:reading\n{section}"
            )).unwrap();
            let recipes = MessageRecipes::new(
                &document,
                &NodeId::new("524950").unwrap(),
                &MessageCatalog::load("en-US").unwrap(),
            )
            .unwrap();
            assert!(recipes.names.contains_key("root"));
            assert_eq!(
                recipes
                    .warnings
                    .iter()
                    .any(|warning| warning.value == "reading"),
                kind != declaration,
                "binding={kind} declaration={declaration}"
            );
        }
    }
}

#[test]
fn composition_catalog_contract_is_checked_before_admission() {
    for invalid in [
        "message_id=not-a-shipped-or-admin-message\n",
        "message_id=site-time\n",
        "content=你好\n",
    ] {
        let document = ConfigDocument::parse(&format!(
            "[524950]\n[message bad]\n{invalid}[message good]\ncontent=Good\ndtmf_id=2\n"
        ))
        .unwrap();
        let recipes = MessageRecipes::new(
            &document,
            &NodeId::new("524950").unwrap(),
            &MessageCatalog::load("en-US").unwrap(),
        )
        .unwrap();
        assert_eq!(
            recipes.names.keys().collect::<Vec<_>>(),
            ["good"],
            "{invalid}"
        );
        assert!(!recipes.warnings.is_empty());
        assert!(recipes.selectors.contains_key("2"));
    }
}

#[test]
fn literal_preparation_rejects_speech_beyond_the_telemetry_bound() {
    // A valid, bounded literal expands when speech spells an alphanumeric
    // identity, so admission succeeds but preparation must retain its bound.
    let recipe = recipe(&format!("[message root]\ncontent={}\n", "A1".repeat(33)));
    assert_eq!(
        recipe
            .prepare(&MessageCatalog::load("en-US").unwrap(), &snapshot())
            .err(),
        Some(CompositionError::TooLarge)
    );
}

#[test]
fn composition_catalog_contract_requires_a_usable_complete_morse_form() {
    let recipe = recipe("[message root]\ncontent=ID\n");
    for invalid in [String::new(), "你好".into(), "E".repeat(128), "E\0".into()] {
        let forms = MessageForms {
            text: "Valid".into(),
            tts: "Valid".into(),
            morse: invalid,
        };
        assert!(spoken_source(forms, recipe.settings_for(None)).is_err());
    }
}

#[test]
fn composition_admission_disables_missing_fallback_and_unreachable_selector() {
    for invalid in [
        "content={media}\nsound_file=id.wav\n".to_owned(),
        "content={media}\nspeech_text=ID\n".to_owned(),
        format!("content=Valid\ndtmf_id={}\n", "1".repeat(126)),
        "message_id=site-time\narg_time=time_remaining\n".to_owned(),
        "content={message_macro_missing}\n".to_owned(),
    ] {
        let document = ConfigDocument::parse(&format!(
            "[524950]\n[message bad]\n{invalid}[message good]\ncontent=Good\ndtmf_id=2\n"
        ))
        .unwrap();
        let recipes = MessageRecipes::new(
            &document,
            &NodeId::new("524950").unwrap(),
            &MessageCatalog::load("en-US").unwrap(),
        )
        .unwrap();
        assert!(!recipes.warnings.is_empty(), "{invalid}");
        assert_eq!(recipes.selectors.len(), 1);
        assert!(recipes.selectors.contains_key("2"));
    }
    let document = ConfigDocument::parse(
        "[524950]\n[message a]\ncontent=A\ndtmf_id=1\n[message b]\ncontent=B\ndtmf_id=1\n",
    )
    .unwrap();
    assert!(
        MessageRecipes::new(
            &document,
            &NodeId::new("524950").unwrap(),
            &MessageCatalog::load("en-US").unwrap()
        )
        .is_err()
    );
}

#[test]
fn composition_value_forms_and_invalid_observations() {
    let catalog = MessageCatalog::load("en-US").unwrap();
    let mut values = snapshot();
    for (source, speech, morse) in [
        (BindingSource::Node, "node,5,2,4,9,5,0", "524950"),
        (BindingSource::Callsign, "K,G,0,B,P", "KG0BP"),
        (BindingSource::Date, "2026-10-18", "2026-10-18"),
        (BindingSource::Time, "1:25 PM", "1:25 PM"),
    ] {
        let BoundValue::Available(MessageArgument::Forms(forms)) =
            values.binding(&source, &catalog).unwrap()
        else {
            panic!("forms")
        };
        assert_eq!(forms.tts, speech);
        assert_eq!(forms.morse, morse);
    }
    for (quality, observed_at_ms, value) in [
        (
            ReadingQuality::Fault,
            Some(1),
            Some(MessageArgument::Number(75.0)),
        ),
        (
            ReadingQuality::Valid,
            None,
            Some(MessageArgument::Number(75.0)),
        ),
        (
            ReadingQuality::Valid,
            Some(1),
            Some(MessageArgument::Number(f64::NAN)),
        ),
        (ReadingQuality::Valid, Some(1), None),
    ] {
        values.sensors.insert(
            "meter".into(),
            Observation {
                quality,
                observed_at_ms,
                value,
            },
        );
        assert_eq!(
            values
                .binding(&BindingSource::Sensor("meter".into()), &catalog)
                .unwrap(),
            BoundValue::Unavailable("meter")
        );
    }
}

fn recipe(text: &str) -> Composition {
    recipe_with_morse(text, MorseRequirement::Required)
}

fn recipe_with_morse(text: &str, requirement: MorseRequirement) -> Composition {
    let document = ConfigDocument::parse(&format!("[524950]\n{text}")).unwrap();
    let node = NodeId::new("524950").unwrap();
    let definitions = document
        .named_sections("message", None)
        .into_iter()
        .map(|section| {
            let name = section.rsplit(' ').next().unwrap();
            (
                name.to_owned(),
                ResolvedMessageSettings::resolve(&document, &node, name)
                    .unwrap()
                    .value,
            )
        })
        .collect::<BTreeMap<_, _>>();
    Composition::compile(
        definitions["root"].clone(),
        Arc::new(definitions),
        requirement,
    )
    .unwrap()
}

#[test]
fn optional_compositions_never_emit_a_partial_morse_substitute() {
    let catalog = MessageCatalog::load("en-US").unwrap();
    for (content, source) in [
        ("Before. {media} After.", "sound_file=message.wav"),
        (
            "Before. {message_macro_recorded} After.",
            "sound_file=message.wav",
        ),
        ("{message_macro_recorded} Before.", "sound_file=message.wav"),
        ("Before. {media} After.", "speech_text=Report"),
        (
            "Before. {message_macro_recorded} After.",
            "speech_text=Report",
        ),
    ] {
        let recipe = recipe_with_morse(
            &format!(
                "[message root]\ncontent={content}\n{source}\n\
             [message recorded]\ncontent={{media}}\n{source}\n"
            ),
            MorseRequirement::Optional,
        );
        let program = recipe.prepare(&catalog, &snapshot()).unwrap();
        assert!(program.primary.len() >= 2);
        assert!(
            program
                .primary
                .iter()
                .any(|source| source.file.is_some() || source.speech.is_some())
        );
        assert!(program.morse.is_empty(), "{content} {source}");
    }
    for content in ["Before. {tone_sequence} After.", "Before. {media} After."] {
        let recipe = recipe_with_morse(
            &format!("[message root]\ncontent={content}\ntone_sequence=500/10\n"),
            MorseRequirement::Optional,
        );
        let program = recipe.prepare(&catalog, &snapshot()).unwrap();
        assert_eq!(program.primary.len(), 3);
        assert!(program.primary[1].tone.is_some());
        assert_eq!(
            program
                .morse
                .iter()
                .map(|source| source.text.as_str())
                .collect::<Vec<_>>(),
            ["Before.", "After."]
        );
    }
}

fn snapshot() -> MessageValueSnapshot {
    MessageValueSnapshot {
        civil: CivilTime::new(2026, 10, 18, Weekday::Sunday, 13, 25).unwrap(),
        time_format: TimeFormat::TwelveHour,
        node: "524950".into(),
        callsign: "KG0BP".into(),
        port: Default::default(),
        event: None,
        builtin: Default::default(),
        links: MessageForms {
            text: "No links".into(),
            tts: "No links".into(),
            morse: "NO LINKS".into(),
        },
        sensors: BTreeMap::new(),
        inputs: BTreeMap::new(),
        time_remaining: None,
    }
}

#[test]
fn composition_values_are_data_and_stale_reading_replaces_only_its_segment() {
    let recipe = recipe(
        "[message root]\ncontent=Before. {message_macro_temperature} After.\n[message temperature]\nmessage_id=site-temperature\narg_temperature=sensor:outdoor\n",
    );
    let catalog = MessageCatalog::load("en-US").unwrap();
    let mut values = snapshot();
    values.sensors.insert(
        "outdoor".into(),
        Observation {
            value: Some(MessageArgument::Forms(MessageForms {
                text: "{message_macro_x}".into(),
                tts: "{message_macro_x}".into(),
                morse: "literal value".into(),
            })),
            observed_at_ms: Some(100),
            quality: ReadingQuality::Valid,
        },
    );
    let program = recipe.prepare(&catalog, &values).unwrap();
    assert_eq!(program.primary.len(), 3);
    assert_eq!(
        program.primary[1].speech.as_ref().unwrap().text,
        "Temperature is {message_macro_x}."
    );
    values.sensors.get_mut("outdoor").unwrap().quality = ReadingQuality::Stale;
    let stale = recipe.prepare(&catalog, &values).unwrap();
    assert_eq!(stale.primary[0], program.primary[0]);
    assert_eq!(stale.primary[2], program.primary[2]);
    assert_eq!(
        stale.primary[1].speech.as_ref().unwrap().text,
        "outdoor is unavailable."
    );
}

#[test]
fn composition_prepares_new_values_without_mutating_prior_attempt() {
    let recipe = recipe(
        "[message root]\ncontent={message_macro_time}\n[message time]\nmessage_id=site-time\narg_time=time\n",
    );
    let catalog = MessageCatalog::load("en-US").unwrap();
    let mut values = snapshot();
    let first = recipe.prepare(&catalog, &values).unwrap();
    values.civil = CivilTime::new(2026, 10, 18, Weekday::Sunday, 13, 26).unwrap();
    let second = recipe.prepare(&catalog, &values).unwrap();
    assert_eq!(
        first.primary[0].speech.as_ref().unwrap().text,
        "The time is 1:25 PM."
    );
    assert_eq!(
        second.primary[0].speech.as_ref().unwrap().text,
        "The time is 1:26 PM."
    );
    assert_eq!(first.morse[0].text, "1:25 PM");
}

#[test]
fn composition_literal_inherits_its_defining_macro_not_the_invoker() {
    let recipe = recipe(
        "[message root]\ncontent=Root {message_macro_quiet}\nspeech_level_db=-6\nmorse_level_db=-10\n[message quiet]\ncontent=Quiet\nspeech_level_db=-20\nmorse_level_db=-30\n",
    );
    let program = recipe
        .prepare(&MessageCatalog::load("en-US").unwrap(), &snapshot())
        .unwrap();
    assert_eq!(program.primary[0].speech.as_ref().unwrap().level_db, -6);
    assert_eq!(program.primary[1].speech.as_ref().unwrap().level_db, -20);
    assert_eq!(program.morse[0].level_db, -10);
    assert_eq!(program.morse[1].level_db, -30);
}

#[test]
fn composition_numeric_values_do_not_become_node_numbers() {
    let recipe = recipe(
        "[message root]\ncontent={message_macro_temperature}\n[message temperature]\nmessage_id=site-temperature\narg_temperature=sensor:outdoor\n",
    );
    let mut values = snapshot();
    values.sensors.insert(
        "outdoor".into(),
        Observation {
            value: Some(MessageArgument::Number(75.0)),
            observed_at_ms: Some(100),
            quality: ReadingQuality::Valid,
        },
    );
    let program = recipe
        .prepare(&MessageCatalog::load("en-US").unwrap(), &values)
        .unwrap();
    assert_eq!(
        program.primary[0].speech.as_ref().unwrap().text,
        "Temperature is 75."
    );
    assert_eq!(program.morse[0].text, "TEMP 75");
}

#[test]
fn composition_civil_and_deadline_bindings_keep_their_own_context() {
    let catalog = MessageCatalog::load("en-US").unwrap();
    let mut values = snapshot();
    for (source, expected) in [
        (BindingSource::DayOfWeek, "Sunday"),
        (BindingSource::LinkStatus, "No links"),
    ] {
        let BoundValue::Available(MessageArgument::Forms(forms)) =
            values.binding(&source, &catalog).unwrap()
        else {
            panic!("expected localized forms");
        };
        assert_eq!(forms.tts, expected);
    }
    for (hour, expected) in [
        (0, "Good Morning"),
        (12, "Good Afternoon"),
        (17, "Good Evening"),
    ] {
        values.civil = CivilTime::new(2026, 10, 18, Weekday::Sunday, hour, 0).unwrap();
        let BoundValue::Available(MessageArgument::Forms(forms)) =
            values.binding(&BindingSource::Greeting, &catalog).unwrap()
        else {
            panic!("expected greeting");
        };
        assert_eq!(forms.tts, expected);
    }
    assert_eq!(
        values.binding(&BindingSource::TimeRemaining, &catalog),
        Err(CompositionError::Preparation)
    );
    values.time_remaining = Some(MessageArgument::Number(60.0));
    assert_eq!(
        values.binding(&BindingSource::TimeRemaining, &catalog),
        Ok(BoundValue::Available(MessageArgument::Number(60.0)))
    );
    values.inputs.insert(
        "door".into(),
        Observation {
            value: Some(MessageArgument::Number(1.0)),
            observed_at_ms: Some(1),
            quality: ReadingQuality::Valid,
        },
    );
    assert_eq!(
        values.binding(&BindingSource::Input("door".into()), &catalog),
        Ok(BoundValue::Available(MessageArgument::Number(1.0)))
    );
}

#[test]
fn composition_media_and_attention_preserve_order_and_fallback() {
    let recipe = recipe(
        "[message root]\ncontent={tone_sequence} {media} {tone_sequence}\n\
         tone_sequence=1000Hz / 200ms / -20dB\nsound_file=id.wav\nmorse_text=E\n",
    );
    let program = recipe
        .prepare(&MessageCatalog::load("en-US").unwrap(), &snapshot())
        .unwrap();
    assert!(
        recipe
            .validate_catalog(&MessageCatalog::load("en-US").unwrap())
            .is_ok()
    );
    assert_eq!(program.primary.len(), 3);
    assert!(program.primary[0].tone.is_some());
    assert_eq!(
        program.primary[1].file.as_deref(),
        Some(std::path::Path::new("id.wav"))
    );
    assert_eq!(program.primary[2], program.primary[0]);
    assert_eq!(program.morse.len(), 1);
    assert_eq!(program.morse[0].text, "E");
    let mut settings = recipe.settings_for(None).clone();
    settings.speech_defaults.voice.clear();
    let source = spoken_source(
        MessageForms {
            text: "ID".into(),
            tts: "ID".into(),
            morse: "ID".into(),
        },
        &settings,
    )
    .unwrap();
    assert!(source.speech.is_none());
    assert_eq!(source.morse.unwrap().text, "ID");
}

#[test]
fn composition_site_selectors_respect_command_size_and_report_active_input() {
    let catalog = MessageCatalog::load("en-US").unwrap();
    let document = ConfigDocument::parse(&format!(
        "[524950]\n[input 524950 door]\ndtmf_id=1\n\
         [sensor 524950 long]\ndtmf_id={}\n",
        "2".repeat(126)
    ))
    .unwrap();
    let recipes =
        MessageRecipes::new(&document, &NodeId::new("524950").unwrap(), &catalog).unwrap();
    assert!(recipes.site_selectors.contains_key("1"));
    assert!(!recipes.site_selectors.contains_key(&"2".repeat(126)));
    assert!(
        recipes
            .warnings
            .iter()
            .any(|warning| warning.message.contains("command length"))
    );
    let mut values = snapshot();
    values.inputs.insert(
        "door".into(),
        Observation {
            value: Some(MessageArgument::Number(1.0)),
            observed_at_ms: Some(1),
            quality: ReadingQuality::Valid,
        },
    );
    let program = recipes.items[recipes.site_selectors["1"]]
        .prepare(&catalog, &values)
        .unwrap();
    assert_eq!(
        program.primary[0].speech.as_ref().unwrap().text,
        "door is active."
    );
}

#[test]
fn flat_identity_arguments_use_the_immutable_peer_and_explicit_port() {
    use crate::config::IdentitySource::*;
    let catalog = MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap();
    let mut observed = snapshot();
    observed.port.allstarlink_number = Some("987".into());
    observed.port.name = "Home".into();
    assert!(
        observed
            .binding(&BindingSource::Identity(PeerNumber), &catalog)
            .is_err()
    );
    observed.event = Some(MessageEventIdentity {
        kind: "connected".into(),
        peer_protocol: Some("echolink".into()),
        peer_number: Some("001".into()),
        peer_callsign: None,
    });
    for (source, expected, spoken) in [
        (NodeName, "Home", "Home"),
        (AllstarlinkNumber, "987", "9,8,7"),
        (PeerNumber, "001", "0,0,1"),
        (PeerProtocol, "echolink", "echolink"),
        (EventKind, "connected", "connected"),
    ] {
        assert_eq!(
            observed
                .binding(&BindingSource::Identity(source), &catalog)
                .unwrap(),
            BoundValue::Available(MessageArgument::Forms(MessageForms {
                text: expected.into(),
                tts: spoken.into(),
                morse: expected.into()
            }))
        );
    }
    assert!(
        observed
            .binding(&BindingSource::Identity(PeerCallsign), &catalog)
            .is_err()
    );
}

#[test]
fn public_fluent_media_parts_use_one_attempt_and_never_dispatch_formatted_input() {
    use crate::composition::PublicMessageSource;
    use crate::messages::patterns::NativePattern;
    let base = recipe("[message root]\ncontent=Literal\n");
    let mut settings = base.settings_for(None).clone();
    settings
        .bindings
        .insert("observed".into(), BindingSource::Sensor("a/b".into()));
    let (catalog, _) = MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap()
    .project_locales(&"en-US".parse().unwrap(), &BTreeMap::new())
    .unwrap();
    let pattern = NativePattern::parse("Reading { $observed }").unwrap();
    let compiled = Composition::compile_public(
        settings.clone(),
        &PublicMessageSource::Content(pattern),
        Arc::new(BTreeMap::new()),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &catalog,
        MorseRequirement::Required,
    )
    .unwrap();
    let mut values = snapshot();
    values.sensors.insert(
        "a/b".into(),
        Observation {
            value: Some(uniform("{ file }")),
            observed_at_ms: Some(23),
            quality: ReadingQuality::Valid,
        },
    );
    let rendered = compiled.prepare_text(&catalog, &values).unwrap();
    assert_eq!(rendered[0].text, "Reading { file }");
    let program = compiled.prepare(&catalog, &values).unwrap();
    assert_eq!(program.primary.len(), 1);
    assert!(program.primary[0].file.is_none());
    values.sensors.get_mut("a/b").unwrap().quality = ReadingQuality::Stale;
    assert!(
        compiled.prepare_text(&catalog, &values).unwrap()[0]
            .text
            .contains("unavailable")
    );
    values.sensors.get_mut("a/b").unwrap().quality = ReadingQuality::Valid;
    values.sensors.get_mut("a/b").unwrap().value = Some(uniform(&"é".repeat(64)));
    assert!(compiled.prepare_text(&catalog, &values).is_err());
}

#[test]
fn review35_explicit_media_requires_valid_static_morse() {
    use crate::{
        composition::PublicMessageSource, media::MorseSource, messages::patterns::NativePattern,
    };
    let (catalog, _) = MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap()
    .project_locales(&"en-US".parse().unwrap(), &Default::default())
    .unwrap();
    let mut settings = recipe("[message root]\ncontent=Literal\n")
        .settings_for(None)
        .clone();
    settings.media.file = Some("recording.wav".into());
    settings.media.morse = Some(MorseSource {
        text: "你好".into(),
        speed_wpm: 20,
        frequency_hz: 800.0,
        level_db: -6,
    });
    let source = PublicMessageSource::Media {
        speech: None,
        morse: Some(NativePattern::parse("你好").unwrap()),
    };
    assert!(
        Composition::compile_public(
            settings,
            &source,
            Arc::new(BTreeMap::new()),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &catalog,
            MorseRequirement::Required
        )
        .is_err()
    );
}
#[test]
fn review35_captured_typed_and_ordinary_values_share_the_actual_format_call() {
    use crate::messages::{patterns::NativePattern, projection::LocalizedMessage};
    let mut projected = LocalizedMessage::default();
    projected
        .locales
        .entry("en-US".parse().unwrap())
        .or_default()
        .insert(
            "text".into(),
            NativePattern::parse("{ $greeting_text } / { $node-name }").unwrap(),
        );
    let (catalog, bindings) = MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap()
    .project_locales(
        &"en-US".parse().unwrap(),
        &BTreeMap::from([("time-announcement".into(), projected)]),
    )
    .unwrap();
    let mut settings = recipe("[message root]\ncontent=Literal\n")
        .settings_for(None)
        .clone();
    settings.message_id = Some("time-announcement".into());
    settings.bindings = bindings["time-announcement"].clone();
    let mut values = snapshot();
    values.port.name = "Station".into();
    values.capture_builtin(&Message::TimeAnnouncement {
        greeting_text: "Good day",
        greeting_tts: "Good day",
        time: "12:34",
    });
    assert_eq!(
        message_forms(&settings, &values, &catalog).unwrap().text,
        "Good day / Station"
    );
}

#[test]
fn real_argument_fallback_resolves_the_installed_forms_own_normal_binding() {
    use crate::messages::{patterns::NativePattern, projection::LocalizedMessage};
    let en = include_str!("../../../../messages/en-US.ftl");
    let mut def = LocalizedMessage::default();
    def.locales
        .entry("fr".parse().unwrap())
        .or_default()
        .insert(
            "text".into(),
            NativePattern::parse("long { $node-name }").unwrap(),
        );
    let (catalog, bindings) =
        MessageCatalog::from_sources("fr", en, Some("no-links =\n    .text = PORT { $node-key }"))
            .unwrap()
            .project_locales(
                &"fr".parse().unwrap(),
                &BTreeMap::from([("no-links".into(), def)]),
            )
            .unwrap();
    let mut settings = recipe("[message root]\ncontent=Literal\n")
        .settings_for(None)
        .clone();
    settings.bindings = bindings["no-links"].clone();
    settings.message_id = Some("no-links".into());
    let mut at_playback = snapshot();
    at_playback.port.name = "z".repeat(126);
    let formatted = message_forms(&settings, &at_playback, &catalog).unwrap();
    assert_eq!(formatted.text, format!("PORT {}", at_playback.node));
}
#[test]
fn sparse_builtin_presentation_retains_its_captured_arguments() {
    let (catalog, bindings) = MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap()
    .project_locales(
        &"en-US".parse().unwrap(),
        &BTreeMap::from([("time-announcement".into(), Default::default())]),
    )
    .unwrap();
    let mut settings = recipe("[message root]\ncontent=Literal\n")
        .settings_for(None)
        .clone();
    settings.message_id = Some("time-announcement".into());
    settings.bindings = bindings["time-announcement"].clone();
    let mut values = snapshot();
    values.capture_builtin(&Message::TimeAnnouncement {
        greeting_text: "Good day",
        greeting_tts: "Hello",
        time: "12:34",
    });
    assert_eq!(
        message_forms(&settings, &values, &catalog).unwrap().tts,
        "Hello. The time is 12:34."
    );
}

#[test]
fn round_two_media_native_reference_validates_english_and_uses_optional_morse_fallback() {
    use crate::{
        composition::PublicMessageSource,
        media::MorseSource,
        messages::{patterns::NativePattern, projection::LocalizedMessage},
    };
    for (en, fr, valid) in [("GO", "你好", true), ("你好", "GO", false)] {
        let mut label = LocalizedMessage::default();
        for (lang, text) in [("en-US", en), ("fr", fr)] {
            let forms = label.locales.entry(lang.parse().unwrap()).or_default();
            for (attr, source) in [("text", text), ("tts", "Okay"), ("morse", "OK")] {
                forms.insert(attr.into(), NativePattern::parse(source).unwrap());
            }
        }
        let localized = BTreeMap::from([("label".into(), label)]);
        let (catalog, _) = MessageCatalog::from_sources(
            "en-US",
            include_str!("../../../../messages/en-US.ftl"),
            None,
        )
        .unwrap()
        .project_locales(&"fr".parse().unwrap(), &localized)
        .unwrap();
        let mut settings = recipe("[message root]\ncontent=Literal\n")
            .settings_for(None)
            .clone();
        settings.media.file = Some("recording.wav".into());
        settings.media.morse = Some(MorseSource {
            text: "{ label.text }".into(),
            speed_wpm: 20,
            frequency_hz: 800.0,
            level_db: -6,
        });
        let pattern = NativePattern::parse("{ label.text }").unwrap();
        for payload in [
            PublicMessageSource::Media {
                speech: None,
                morse: Some(pattern.clone()),
            },
            PublicMessageSource::Content(pattern),
        ] {
            let result = Composition::compile_public(
                settings.clone(),
                &payload,
                Arc::new(BTreeMap::new()),
                &BTreeMap::new(),
                &localized,
                &catalog,
                MorseRequirement::Required,
            );
            assert_eq!(result.is_ok(), valid);
            if let Ok(recipe) = result {
                assert_eq!(
                    recipe.prepare(&catalog, &snapshot()).unwrap().morse[0].text,
                    "GO"
                );
            }
        }
    }
}
#[test]
fn round_two_site_fallback_keeps_its_alias_binding() {
    use crate::messages::{patterns::NativePattern, projection::LocalizedMessage};
    let mut site = LocalizedMessage::default();
    for (lang, attr, val) in [
        ("en-US", "text", "EN"),
        ("en-US", "tts", "SAY"),
        ("en-US", "morse", "EN"),
        ("fr", "text", "SITE { $alias }"),
    ] {
        site.locales
            .entry(lang.parse().unwrap())
            .or_default()
            .insert(attr.into(), NativePattern::parse(val).unwrap());
    }
    site.bindings.insert(
        "alias".into(),
        BindingSource::from_public_path("node.key").unwrap(),
    );
    let mut port = site.clone();
    port.bindings.insert(
        "alias".into(),
        BindingSource::from_public_path("node.name").unwrap(),
    );
    port.locales
        .get_mut(&"fr".parse().unwrap())
        .unwrap()
        .insert(
            "text".into(),
            NativePattern::parse("LONG { $alias }").unwrap(),
        );
    let previous = BTreeMap::from([("notice".into(), site)]);
    let current = BTreeMap::from([("notice".into(), port)]);
    let (catalog, bindings, _) = MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap()
    .project_message_sources(
        &"fr".parse().unwrap(),
        &current,
        &Default::default(),
        &previous,
    )
    .unwrap();
    let mut settings = recipe("[message root]\ncontent=Literal\n")
        .settings_for(None)
        .clone();
    settings.message_id = Some("notice".into());
    settings.bindings = bindings["notice"].clone();
    let mut values = snapshot();
    values.port.name = "Z".repeat(126);
    assert_eq!(
        message_forms(&settings, &values, &catalog).unwrap().text,
        format!("SITE {}", values.node)
    );
}

#[test]
fn round_three_site_candidate_localizes_unavailable_readings_only_when_selected() {
    use crate::messages::{patterns::NativePattern, projection::LocalizedMessage};
    for (path, input) in [("sensor:outdoor", false), ("input:outdoor", true)] {
        let mut site = LocalizedMessage::default();
        for (lang, attr, value) in [
            ("en-US", "text", "Ready"),
            ("en-US", "tts", "SAY"),
            ("en-US", "morse", "EN"),
            ("fr", "text", "TEMP { $reading }"),
        ] {
            site.locales
                .entry(lang.parse().unwrap())
                .or_default()
                .insert(attr.into(), NativePattern::parse(value).unwrap());
        }
        site.bindings.insert(
            "reading".into(),
            BindingSource::from_public_path(path).unwrap(),
        );
        let mut port = site.clone();
        port.locales
            .get_mut(&"fr".parse().unwrap())
            .unwrap()
            .insert(
                "text".into(),
                NativePattern::parse("LONG { $node-name }").unwrap(),
            );
        let previous = BTreeMap::from([("notice".into(), site)]);
        let current = BTreeMap::from([("notice".into(), port)]);
        let selected = "fr".parse().unwrap();
        let (catalog,bindings,_)=MessageCatalog::from_sources("fr",include_str!("../../../../messages/en-US.ftl"),Some("site-value-unavailable =\n    .text = { $label } indisponible\n    .tts = { $label } absent\n    .morse = { $label } ABSENT\n")).unwrap().project_message_sources(&selected,&current,&Default::default(),&previous).unwrap();
        let mut settings = recipe("[message root]\ncontent=Literal\n")
            .settings_for(None)
            .clone();
        settings.message_id = Some("notice".into());
        settings.bindings = bindings["notice"].clone();
        let mut values = snapshot();
        let readings = if input {
            &mut values.inputs
        } else {
            &mut values.sensors
        };
        readings.insert(
            "outdoor".into(),
            Observation {
                value: Some(MessageArgument::Number(7.0)),
                observed_at_ms: Some(15),
                quality: ReadingQuality::Stale,
            },
        );
        values.port.name = "AB".into();
        assert_eq!(
            message_forms(&settings, &values, &catalog).unwrap().text,
            "LONG AB"
        );
        values.port.name = "Z".repeat(126);
        let unavailable = message_forms(&settings, &values, &catalog).unwrap();
        assert_eq!(unavailable.text, "outdoor indisponible");
        assert_eq!(unavailable.tts, "outdoor absent");
        let readings = if input {
            &mut values.inputs
        } else {
            &mut values.sensors
        };
        readings.get_mut("outdoor").unwrap().quality = ReadingQuality::Valid;
        assert_eq!(
            message_forms(&settings, &values, &catalog).unwrap().text,
            "TEMP 7"
        );
    }
}
