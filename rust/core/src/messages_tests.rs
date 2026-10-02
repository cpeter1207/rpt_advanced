use super::messages::{CatalogError, Message, MessageCatalog};

const ENGLISH: &str = include_str!("../../../messages/en-US.ftl");

#[test]
fn message_catalog_formats_all_variants_with_typed_arguments() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let forms = catalog
        .format(&Message::PeerConnectedLocal { peer: "506316" })
        .unwrap();
    assert_eq!(forms.text, "506316 connected");
    assert_eq!(forms.tts, "506316 connected");
    assert_eq!(forms.morse, "506316 CONNECTED");
}

#[test]
fn message_catalog_formats_third_party_link_wording_and_link_status_arguments() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let connected = catalog
        .format(&Message::PeerConnectedThirdParty {
            first: "1000",
            second: "2000",
        })
        .unwrap();
    assert_eq!(connected.text, "1000 CONNECTED TO 2000");
    assert_eq!(connected.tts, "1000 connected to 2000");
    let status = catalog
        .format(&Message::LinkStatus {
            count: 2,
            peer: "2000",
            mode: super::messages::LinkDisplayMode::Transceive,
        })
        .unwrap();
    assert_eq!(status.text, "2 LINKS 2000 TRANSCEIVE");
}

#[test]
fn message_catalog_localizes_time_greeting_and_keeps_morse_time_only() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let morning = catalog
        .format(&Message::Greeting {
            daypart: super::messages::Daypart::Morning,
        })
        .unwrap();
    let time = catalog
        .format(&Message::TimeAnnouncement {
            greeting_text: &morning.text,
            greeting_tts: &morning.tts,
            time: "12:05 AM",
        })
        .unwrap();
    assert_eq!(time.tts, "Good Morning. The time is 12:05 AM.");
    assert_eq!(time.morse, "12:05 AM");
}

#[test]
fn message_catalog_localizes_weekday_values_used_by_custom_templates() {
    let translated =
        "day-of-week =\n    .text = Vendredi\n    .tts = Vendredi\n    .morse = VENDREDI\n";
    let catalog = MessageCatalog::from_sources("fr-CA", ENGLISH, Some(translated)).unwrap();
    let forms = catalog
        .format(&Message::DayOfWeek {
            day: crate::schedule::Weekday::Friday,
        })
        .unwrap();
    assert_eq!(forms.text, "Vendredi");
    assert_eq!(forms.tts, "Vendredi");
    assert_eq!(forms.morse, "VENDREDI");
}

#[test]
fn message_catalog_formats_status_rejection_groups_and_schedule_warnings() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    assert_eq!(catalog.format(&Message::NoLinks).unwrap().text, "NO LINKS");
    assert_eq!(
        catalog.format(&Message::LastKeyedNone).unwrap().tts,
        "No last keyed station"
    );
    assert_eq!(
        catalog
            .format(&Message::LoopRejected { node: "2000" })
            .unwrap()
            .text,
        "LINK TO NODE 2000 REJECTED: WOULD CREATE LOOP"
    );
    assert_eq!(
        catalog
            .format(&Message::PriorityGroupSelected {
                group: "Blind Hams Network",
                peer: "506315",
            })
            .unwrap()
            .tts,
        "Blind Hams Network. 506315 selected"
    );
    assert_eq!(
        catalog
            .format(&Message::PriorityGroupUnavailable {
                group: "Blind Hams Network",
            })
            .unwrap()
            .morse,
        "Blind Hams Network UNAVAILABLE"
    );
    assert_eq!(
        catalog
            .format(&Message::ScheduledLinkChange {
                time_remaining: "60 seconds",
            })
            .unwrap()
            .tts,
        "This connection will change in 60 seconds"
    );
}

#[test]
fn message_catalog_falls_back_per_attribute_and_ignores_bad_translation_entries() {
    let french =
        "peer-connected-local =\n    .text = { $wrong }\n    .tts = Noeud { $peer } connecte\n";
    let catalog = MessageCatalog::from_sources("fr-CA", ENGLISH, Some(french)).unwrap();
    let forms = catalog
        .format(&Message::PeerConnectedLocal { peer: "2000" })
        .unwrap();
    assert_eq!(forms.text, "2000 connected");
    assert_eq!(forms.tts, "Noeud 2000 connecte");
    assert_eq!(forms.morse, "2000 CONNECTED");
    assert!(
        catalog
            .translation_warnings()
            .contains(&"peer-connected-local.text falls back to English".to_owned())
    );
}

#[test]
fn message_catalog_rejects_incomplete_english_but_accepts_missing_optional_locale_entries() {
    let incomplete = "peer-connected-local =\n    .text = x\n";
    assert_eq!(
        MessageCatalog::from_sources("en-US", incomplete, None).unwrap_err(),
        CatalogError::InvalidEnglish
    );
    let catalog =
        MessageCatalog::from_sources("fr-CA", ENGLISH, Some("no-links =\n    .text = x\n"))
            .unwrap();
    let forms = catalog.format(&Message::NoLinks).unwrap();
    assert_eq!(forms.text, "x");
    assert_eq!(forms.tts, "No links");
    assert_eq!(forms.morse, "NO LINKS");
}

#[test]
fn message_catalog_rejects_formatted_output_over_127_bytes() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let peer = "x".repeat(128);
    assert_eq!(
        catalog.format(&Message::PeerConnectedLocal { peer: &peer }),
        Err(CatalogError::OutputTooLong)
    );
}

#[test]
fn message_catalog_requires_every_builtin_id_and_attribute() {
    let missing = ENGLISH.replace("    .morse = NO LINKS\n", "");
    assert_eq!(
        MessageCatalog::from_sources("en-US", &missing, None).unwrap_err(),
        CatalogError::InvalidEnglish
    );
}
