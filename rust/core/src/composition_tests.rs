use crate::composition::{Composition, CompositionError, Segment};
use crate::config::{BindingSource, ConfigDocument, NodeId, ResolvedMessageSettings};
use std::collections::BTreeMap;

#[test]
fn composition_identifier_content_preserves_media_inheritance() {
    let config = ConfigDocument::parse("[524950]\n[identifier]\ncontent={message}\nmessage_id=site-time\narg_time=time\nspeech_level_db=-3\n[identifier 524950]\narg_time=\n[speech]\nlevel_db=-8\n[identifier 524950 regular]\ncontent={message_macro_time}\nmorse_text=ID\n").unwrap();
    let resolved = crate::config::ResolvedIdentifierSettings::resolve(
        &config,
        &NodeId::new("524950").unwrap(),
        Some("regular"),
    )
    .unwrap();
    assert!(resolved.warnings.is_empty());
    assert_eq!(resolved.value.content, "{message_macro_time}");
    assert_eq!(resolved.value.message_id.as_deref(), Some("site-time"));
    assert!(resolved.value.bindings.is_empty());
    assert_eq!(resolved.value.speech_level_db, -8);
    assert_eq!(resolved.value.morse_text, "ID");
}

fn definitions(text: &str, names: &[&str]) -> BTreeMap<String, ResolvedMessageSettings> {
    let document = ConfigDocument::parse(&format!("[524950]\n{text}")).unwrap();
    names
        .iter()
        .map(|name| {
            (
                name.to_string(),
                ResolvedMessageSettings::resolve(&document, &NodeId::new("524950").unwrap(), name)
                    .unwrap()
                    .value,
            )
        })
        .collect()
}

fn compile(
    content: &str,
    definitions: &BTreeMap<String, ResolvedMessageSettings>,
) -> Result<Composition, CompositionError> {
    let document =
        ConfigDocument::parse("[524950]\n[message root]\nmessage_id=site-time\nmorse_text=ID\n")
            .unwrap();
    let mut root =
        ResolvedMessageSettings::resolve(&document, &NodeId::new("524950").unwrap(), "root")
            .unwrap()
            .value;
    root.content = content.into();
    Composition::compile(root, std::sync::Arc::new(definitions.clone()))
}

#[test]
fn composition_literal_multiple_macros() {
    let defs = definitions(
        "[message time]\nmessage_id=site-time\n[message temperature]\nmessage_id=site-temperature\n",
        &["time", "temperature"],
    );
    let composition = compile(
        "Station {{KG0BP}}. {message_macro_time} {message_macro_temperature}",
        &defs,
    )
    .unwrap();
    assert_eq!(
        composition.segments(),
        &[
            Segment::Literal {
                text: "Station {KG0BP}.".into(),
                owner: None
            },
            Segment::Message(Some("time".into())),
            Segment::Message(Some("temperature".into())),
        ]
    );
}

#[test]
fn composition_file_then_macros() {
    let defs = definitions("[message time]\nmessage_id=site-time\n", &["time"]);
    assert_eq!(
        compile("{media} {message_macro_time}", &defs)
            .unwrap()
            .segments(),
        &[Segment::Media(None), Segment::Message(Some("time".into())),]
    );
}

#[test]
fn composition_attention_positions() {
    let defs = definitions(
        "[message attention]\ncontent={tone_sequence}\ntone_sequence=1000Hz / 200ms / -20dB\n[message time]\nmessage_id=site-time\n[message temperature]\nmessage_id=site-temperature\n",
        &["attention", "time", "temperature"],
    );
    let composition = compile("{message_macro_attention} {message_macro_time} {message_macro_attention} {message_macro_temperature} {message_macro_attention}", &defs).unwrap();
    assert_eq!(
        composition.segments(),
        &[
            Segment::Tone(Some("attention".into())),
            Segment::Message(Some("time".into())),
            Segment::Tone(Some("attention".into())),
            Segment::Message(Some("temperature".into())),
            Segment::Tone(Some("attention".into())),
        ]
    );
}

#[test]
fn composition_cycle_or_overflow_is_rejected_before_expansion() {
    let mut defs = definitions(
        "[message a]\ncontent={message_macro_b}\n[message b]\ncontent={message_macro_a}\n",
        &["a", "b"],
    );
    assert_eq!(
        compile("{message_macro_a}", &defs),
        Err(CompositionError::Cycle)
    );
    let base = defs["a"].clone();
    defs.clear();
    for index in (0..usize::BITS + 1).rev() {
        let mut definition = base.clone();
        definition.content = if index == usize::BITS {
            "x".into()
        } else {
            format!("{{message_macro_n{0}}}{{message_macro_n{0}}}", index + 1)
        };
        defs.insert(format!("n{index}"), definition);
    }
    assert_eq!(
        compile("{message_macro_n0}", &defs),
        Err(CompositionError::TooLarge)
    );
}

#[test]
fn composition_segment_bounds_and_bad_tokens() {
    let defs = definitions(
        &format!("[message a]\ncontent={}\n", "x".repeat(100)),
        &["a"],
    );
    let composition = compile("{message_macro_a}{message_macro_a}", &defs).unwrap();
    assert_eq!(composition.segments().len(), 2);
    for bad in [
        "{unknown}",
        "{message_macro_unknown}",
        "{broken",
        "broken}",
        &"x".repeat(128),
    ] {
        assert!(compile(bad, &defs).is_err(), "{bad}");
    }
    assert_eq!(
        compile("{{message_macro_a}}", &defs).unwrap().segments(),
        &[Segment::Literal {
            text: "{message_macro_a}".into(),
            owner: None
        }]
    );
}

#[test]
fn composition_configuration_inherits_values_and_warns_invalid_bindings() {
    let text = "[524950]\n[speech]\nlevel_db=-6\n[message status]\nmessage_id=site-test\narg_node=node\narg_value=sensor:temperature\ndtmf_id=1\n[message 524950 status]\narg_value=invalid\nspeech_speed_percent=125\ndtmf_id=01\n";
    let doc = ConfigDocument::parse(text).unwrap();
    let resolution =
        ResolvedMessageSettings::resolve(&doc, &NodeId::new("524950").unwrap(), "status").unwrap();
    assert_eq!(resolution.value.content, "{message}");
    assert_eq!(
        resolution.value.bindings["value"],
        BindingSource::Sensor("temperature".into())
    );
    assert_eq!(resolution.value.dtmf_id.as_deref(), Some("1"));
    assert_eq!(resolution.warnings.len(), 2);
    let mut args = resolution.value.bindings;
    args.insert("remaining".into(), BindingSource::TimeRemaining);
    assert!(!args["remaining"].available_in_context(false));
    assert!(args["remaining"].available_in_context(true));
}

#[test]
fn composition_configuration_invalid_content_falls_back_and_preserves_media_defaults() {
    let doc = ConfigDocument::parse("[524950]\n[speech]\nlevel_db=-6\n[morse]\nfrequency_hz=500\n[message time]\ncontent={message}\nmessage_id=site-time\nspeech_speed_percent=125\n[message 524950 time]\ncontent={broken\nspeech_speed_percent=0\n").unwrap();
    let resolved =
        ResolvedMessageSettings::resolve(&doc, &NodeId::new("524950").unwrap(), "time").unwrap();
    assert_eq!(resolved.value.content, "{message}");
    assert_eq!(resolved.warnings.len(), 2);
    // Catalog segments need effective voice/Morse settings even without literal text.
    assert_eq!(resolved.value.speech_defaults.speed_percent, 125);
    assert_eq!(resolved.value.speech_defaults.level_db, -6);
    assert_eq!(resolved.value.morse_defaults.frequency_hz, 500);
}

#[test]
fn composition_configuration_clears_bindings_and_validates_shapes() {
    let doc = ConfigDocument::parse("[524950]\n[message a]\ncontent=Hello\narg_value=time\ndtmf_id=1\n[message 524950 a]\narg_value=\ndtmf_id=\n").unwrap();
    let resolved =
        ResolvedMessageSettings::resolve(&doc, &NodeId::new("524950").unwrap(), "a").unwrap();
    assert!(resolved.value.bindings.is_empty());
    assert!(resolved.value.dtmf_id.is_none());
    assert!(resolved.warnings.is_empty());
    for invalid in [
        "[message]\n",
        "[message 524950 a extra]\n",
        "[message unknown a]\n",
        "[message a]\n[message a]\n",
    ] {
        assert!(crate::config::Schema::validate(&ConfigDocument::parse(invalid).unwrap()).is_err());
    }
    for value in [
        "callsign",
        "node",
        "date",
        "time",
        "day_of_week",
        "greeting",
        "link_status",
        "input:door",
        "sensor:temperature",
    ] {
        assert!(
            value
                .parse::<BindingSource>()
                .unwrap()
                .available_in_context(false)
        );
    }
    for value in ["sensor:", "input:a:b", "sensor:two names", "unknown:name"] {
        assert!(value.parse::<BindingSource>().is_err());
    }
}
