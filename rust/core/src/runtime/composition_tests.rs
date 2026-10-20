use super::*;
use crate::{
    composition::Composition,
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
        let Some(MessageArgument::Forms(forms)) = values.binding(&source, &catalog).unwrap() else {
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
        assert!(
            values
                .binding(&BindingSource::Sensor("meter".into()), &catalog)
                .unwrap()
                .is_none()
        );
    }
}

fn recipe(text: &str) -> Composition {
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
    Composition::compile(definitions["root"].clone(), Arc::new(definitions)).unwrap()
}

fn snapshot() -> MessageValueSnapshot {
    MessageValueSnapshot {
        civil: CivilTime::new(2026, 10, 18, Weekday::Sunday, 13, 25).unwrap(),
        time_format: TimeFormat::TwelveHour,
        node: "524950".into(),
        callsign: "KG0BP".into(),
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
