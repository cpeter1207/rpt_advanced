use crate::composition::{
    Composition, CompositionError, MorseRequirement, PublicMessageSource, Segment,
};
use crate::config::{BindingSource, resources::candidate::ResourceKind};
use crate::messages::patterns::NativePattern;
use crate::station_fixture::{PORT, SITE, configuration, projected_telemetry};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn optional_morse_admits_real_media_and_is_inherited_by_nested_macros() {
    let (_, m, _) = projected_telemetry(&[]).unwrap();
    for (file, morse, allowed) in [
        (false, None, false),
        (true, None, true),
        (false, Some("MSG"), true),
        (true, Some("MSG"), true),
    ] {
        let mut settings = m.renderer_defaults().clone();
        if file {
            settings.media.file = Some("message.wav".into());
        }
        if let Some(text) = morse {
            settings.media.morse = Some(crate::media::MorseSource {
                text: text.into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            });
        }
        let defs = Arc::new(BTreeMap::from([("recording".into(), settings)]));
        let sources = BTreeMap::from([(
            "recording".into(),
            PublicMessageSource::Media {
                speech: None,
                morse: morse.map(|t| NativePattern::parse(t).unwrap()),
            },
        )]);
        for (requirement, want) in [
            (MorseRequirement::Optional, allowed),
            (MorseRequirement::Required, morse.is_some()),
        ] {
            assert_eq!(
                Composition::compile_public(
                    m.renderer_defaults().clone(),
                    &PublicMessageSource::Content(
                        NativePattern::parse("Before. { recording }").unwrap()
                    ),
                    defs.clone(),
                    &sources,
                    &BTreeMap::new(),
                    &m.catalog,
                    requirement
                )
                .is_ok(),
                want
            );
        }
    }
}

fn compile(
    content: &str,
    parts: &BTreeMap<String, String>,
) -> Result<Composition, CompositionError> {
    let (_, m, _) = projected_telemetry(&[]).unwrap();
    let lower = |s: &str| {
        NativePattern::parse(s)
            .map(PublicMessageSource::Content)
            .map_err(|_| CompositionError::Syntax)
    };
    let defs = Arc::new(
        parts
            .keys()
            .map(|k| (k.clone(), m.renderer_defaults().clone()))
            .collect(),
    );
    let sources = parts
        .iter()
        .map(|(k, v)| Ok((k.clone(), lower(v)?)))
        .collect::<Result<BTreeMap<_, _>, CompositionError>>()?;
    Composition::compile_public(
        m.renderer_defaults().clone(),
        &lower(content)?,
        defs,
        &sources,
        &BTreeMap::new(),
        &m.catalog,
        MorseRequirement::Required,
    )
}

#[test]
fn composition_cycle_or_overflow_is_rejected_before_expansion() {
    let mut parts = BTreeMap::from([
        ("first".into(), "{ second }".into()),
        ("second".into(), "{ first }".into()),
    ]);
    assert_eq!(compile("{ first }", &parts), Err(CompositionError::Cycle));
    parts.clear();
    for i in (0..=usize::BITS).rev() {
        let content = if i == usize::BITS {
            "x".into()
        } else {
            format!("{{ n{0} }}{{ n{0} }}", i + 1)
        };
        parts.insert(format!("n{i}"), content);
    }
    assert_eq!(compile("{ n0 }", &parts), Err(CompositionError::TooLarge));
}

#[test]
fn composition_literal_multiple_references_preserve_owners_and_bounds() {
    let parts = BTreeMap::from([
        ("first".into(), "First".into()),
        ("second".into(), "Second".into()),
    ]);
    let value = compile("Station. { first } { second }", &parts).unwrap();
    let owners = value
        .segments()
        .iter()
        .map(|s| match s {
            Segment::Fluent { owner, .. } => owner.as_deref(),
            _ => panic!("unexpected segment"),
        })
        .collect::<Vec<_>>();
    assert_eq!(owners, [None, Some("first"), Some("second")]);
    assert!(compile("{ missing }", &parts).is_err());
    assert!(compile("{ malformed", &parts).is_err());
    assert!(compile(&"x".repeat(128), &parts).is_err());
}

#[test]
fn composition_media_and_attention_preserve_sequence_and_require_fallback() {
    let rows = [
        (
            ResourceKind::Message,
            "attention",
            json!({"tone":{"sequence":"440/20"}}),
        ),
        (
            ResourceKind::Message,
            "recorded",
            json!({"file":"id.wav","morse":{"text":"ID"}}),
        ),
    ];
    let (_, m, _) = projected_telemetry(&rows).unwrap();
    let recipe = m
        .compile_inline(
            "{ attention } { recorded } { attention }",
            MorseRequirement::Required,
        )
        .unwrap();
    assert_eq!(
        recipe
            .segments()
            .iter()
            .map(|s| match s {
                Segment::FluentMedia { owner, .. } => owner.as_deref(),
                _ => panic!("unexpected segment"),
            })
            .collect::<Vec<_>>(),
        [Some("attention"), Some("recorded"), Some("attention")]
    );
    for text in ["", " ", "你好"] {
        assert!(
            projected_telemetry(&[(
                ResourceKind::Message,
                "recorded",
                json!({"file":"id.wav","morse":{"text":text}})
            )])
            .is_err()
        );
    }
}

#[test]
fn composition_configuration_retains_inherited_media_defaults_and_registered_bindings() {
    let cfg = configuration(
        &[(PORT, "radio")],
        &[
            (
                SITE,
                ResourceKind::TelemetrySettings,
                "",
                json!({"speech":{"level_db":-6},"morse":{"frequency_hz":500}}),
            ),
            (
                SITE,
                ResourceKind::Message,
                "report",
                json!({"content":"Temperature { $reading }","bindings":{"reading":"sensor:temperature"},"dtmf_id":"1"}),
            ),
            (
                PORT,
                ResourceKind::Message,
                "report",
                json!({"speech":{"timing":{"speed_percent":125}}}),
            ),
        ],
    );
    let scope = crate::config::resources::candidate::ResourceScope::Port(PORT.parse().unwrap());
    let recipe = &cfg.scopes[&scope].messages.recipes["report"];
    let settings = recipe.settings_for(None);
    assert_eq!(settings.speech_defaults.speed_percent, 125);
    assert_eq!(settings.speech_defaults.level_db, -6);
    assert_eq!(settings.morse_defaults.frequency_hz, 500);
    assert_eq!(
        settings.bindings["reading"],
        BindingSource::Sensor("temperature".into())
    );
    assert_eq!(settings.dtmf_id.as_deref(), Some("1"));
    for s in ["sensor:", "input:a:b", "sensor:two names", "unknown:name"] {
        assert!(BindingSource::from_public_path(s).is_none());
    }
    assert!(!BindingSource::TimeRemaining.available_in_context(false));
    assert!(BindingSource::TimeRemaining.available_in_context(true));
}
