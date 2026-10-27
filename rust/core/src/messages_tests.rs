use super::messages::{CatalogError, Message, MessageCatalog, MessageForms};

const ENGLISH: &str = include_str!("../../../messages/en-US.ftl");

#[test]
fn timeout_notice_formats_the_configured_duration() {
    use super::messages::MessageArgument;
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let args = std::collections::BTreeMap::from([(
        "duration".into(),
        MessageArgument::Forms(MessageForms {
            text: "3 minutes".into(),
            tts: "3 minutes".into(),
            morse: "3M".into(),
        }),
    )]);
    let forms = catalog.format_named("repeater-timed-out", &args).unwrap();
    assert_eq!(forms.text, "Repeater timed out after 3 minutes.");
    assert_eq!(forms.tts, "Repeater timed out after 3 minutes.");
    assert_eq!(forms.morse, "REPEATER TIMED OUT AFTER 3M");
}

#[test]
fn timeout_notices_use_typed_catalog_messages() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    assert_eq!(
        catalog
            .format(&Message::RepeaterTimedOut {
                duration: "1 second",
            })
            .unwrap(),
        MessageForms {
            text: "Repeater timed out after 1 second.".into(),
            tts: "Repeater timed out after 1 second.".into(),
            morse: "REPEATER TIMED OUT AFTER 1 second".into(),
        }
    );
    assert_eq!(
        catalog.format(&Message::RepeaterTimeoutCleared).unwrap(),
        MessageForms {
            text: "Repeater timeout cleared.".into(),
            tts: "Repeater timeout cleared.".into(),
            morse: "REPEATER TIMEOUT CLEARED".into(),
        }
    );
}

#[test]
fn timeout_notice_preserves_duration_units_and_plurals() {
    use crate::schedule::{CivilTime, Weekday};
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    for (seconds, expected, morse) in [
        (0, "0 seconds", "0S"),
        (1, "1 second", "1S"),
        (2, "2 seconds", "2S"),
        (60, "1 minute", "1M"),
        (90, "1 minute, and 30 seconds", "1M 30S"),
        (180, "3 minutes", "3M"),
        (3600, "1 hour", "1H"),
        (
            3373381,
            "1 month, 1 week, 1 day, 1 hour, 3 minutes, and 1 second",
            "1M 1W 1D 1H 3M 1S",
        ),
    ] {
        let forms = catalog.format_timeout(seconds, local).unwrap();
        assert_eq!(forms.text, format!("Repeater timed out after {expected}."));
        assert_eq!(forms.tts, format!("Repeater timed out after {expected}."));
        assert_eq!(forms.morse, format!("REPEATER TIMED OUT AFTER {morse}"));
    }
}

#[test]
fn timeout_notice_uses_independent_localized_duration_forms_and_fallback() {
    use crate::schedule::{CivilTime, Weekday};
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let translation = "duration-unit =\n    .text = display { $count } { $unit }\n    .tts = voice { $count } { $unit }\n    .morse = cw { $count } { $unit }\nduration-join =\n    .text = { $head } et { $tail }\n    .tts = { $head } puis { $tail }\n    .morse = { $head } { $tail }\nrepeater-timed-out =\n    .text = display { $duration }\n    .tts = voice { $duration }\n    .morse = cw { $duration }\nrepeater-timeout-cleared =\n    .text = Termine\n    .tts = { $missing }\n";
    let catalog = MessageCatalog::from_sources("fr-CA", ENGLISH, Some(translation)).unwrap();
    assert_eq!(
        catalog.format_timeout(90, local).unwrap(),
        MessageForms {
            text: "display display 1 minute et display 30 second".into(),
            tts: "voice voice 1 minute puis voice 30 second".into(),
            morse: "cw cw 1 minute cw 30 second".into(),
        }
    );
    assert_eq!(
        catalog.format(&Message::RepeaterTimeoutCleared).unwrap(),
        MessageForms {
            text: "Termine".into(),
            tts: "Repeater timeout cleared.".into(),
            morse: "REPEATER TIMEOUT CLEARED".into(),
        }
    );
}

#[test]
fn timeout_notice_accepts_a_bounded_duration_beyond_the_fragment_limit() {
    use crate::schedule::{CivilTime, Weekday};
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let fragment = "x".repeat(127);
    let translation = format!(
        "duration-unit =\n    .text = {fragment}\n    .tts = {fragment}\n    .morse = {fragment}\n"
    );
    let catalog = MessageCatalog::from_sources("de", ENGLISH, Some(&translation)).unwrap();
    let forms = catalog.format_timeout(1, local).unwrap();
    assert_eq!(forms.text, format!("Repeater timed out after {fragment}."));
    assert_eq!(forms.tts, format!("Repeater timed out after {fragment}."));
    assert_eq!(forms.morse, format!("REPEATER TIMED OUT AFTER {fragment}"));
    assert!(forms.tts.len() > 127);
}

#[test]
fn composition_shipped_site_messages_keep_independent_forms() {
    use super::messages::{MessageArgument, MessageForms};
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    for (id, name) in [
        ("site-time", "time"),
        ("site-temperature", "temperature"),
        ("site-link-status", "links"),
        ("site-value-unavailable", "label"),
    ] {
        let args = std::collections::BTreeMap::from([(
            name.into(),
            MessageArgument::Forms(MessageForms {
                text: "DISPLAY".into(),
                tts: "SPEECH".into(),
                morse: "MORSE".into(),
            }),
        )]);
        let forms = catalog.format_named(id, &args).unwrap();
        assert!(forms.text.contains("DISPLAY"));
        assert!(forms.tts.contains("SPEECH"));
        assert!(forms.morse.contains("MORSE"));
    }
}

#[test]
fn composition_named_forms_keep_numbers_and_audience_values_separate() {
    use super::messages::{MessageArgument, MessageForms};
    let source = format!(
        "{ENGLISH}\nsite-test =\n    .text = {{ $node }} {{ $value }}\n    .tts = {{ $node }} {{ $value }}\n    .morse = {{ $node }} {{ $value }}\n"
    );
    let catalog = MessageCatalog::from_sources("en-US", &source, None).unwrap();
    let args = std::collections::BTreeMap::from([
        ("value".into(), MessageArgument::Number(12.5)),
        (
            "node".into(),
            MessageArgument::Forms(MessageForms {
                text: "524950".into(),
                tts: "node,5,2,4,9,5,0".into(),
                morse: "524950".into(),
            }),
        ),
    ]);
    let forms = catalog.format_named("site-test", &args).unwrap();
    assert_eq!(forms.tts, "node,5,2,4,9,5,0 12.5");
    assert_eq!(forms.morse, "524950 12.5");
    assert!(catalog.format_named("unknown", &args).is_err());
    assert!(
        catalog
            .format_named("site-test", &Default::default())
            .is_err()
    );
}

#[test]
fn composition_named_forms_enforce_independent_byte_bounds_and_fallback() {
    for size in [100, 127, 128] {
        let value = "x".repeat(size);
        let source = format!(
            "{ENGLISH}\nsite-test =\n    .text = {value}\n    .tts = {value}\n    .morse = {value}\n"
        );
        let catalog = MessageCatalog::from_sources(
            "fr",
            &source,
            Some("site-test =\n    .text = traduit\n    .tts = { $wrong }\n"),
        )
        .unwrap();
        let forms = catalog.format_named("site-test", &Default::default());
        if size <= 127 {
            let forms = forms.unwrap();
            assert_eq!(forms.text, "traduit");
            assert_eq!(forms.tts, value);
            assert_eq!(forms.morse, value);
        } else {
            assert_eq!(forms, Err(CatalogError::OutputTooLong));
        }
    }
}

#[test]
fn pad_test_forms_are_localized_and_bounded() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let forms = catalog.format_pad_test("*#ABCD0123456789", false).unwrap();
    assert_eq!(forms.text, "* # A B C D 0 1 2 3 4 5 6 7 8 9");
    assert_eq!(forms.tts, "star pound A B C D 0 1 2 3 4 5 6 7 8 9");
    assert_eq!(forms.morse, "STAR POUND A B C D 0 1 2 3 4 5 6 7 8 9");
    assert_eq!(catalog.format_pad_test("", false).unwrap().tts, "no digits");
    assert_eq!(
        catalog.format_pad_test("11", true).unwrap().tts,
        "too many digits 1 1"
    );
    for invalid in ["x".to_owned(), "1".repeat(128)] {
        assert!(catalog.format_pad_test(&invalid, false).is_err());
    }
    let fragment = "x".repeat(127);
    let overlay = format!(
        "dtmf-symbol =\n    .text = {fragment}\n    .tts = {fragment}\n    .morse = {fragment}\n"
    );
    let translated = MessageCatalog::from_sources("de", ENGLISH, Some(&overlay)).unwrap();
    let forms = translated.format_pad_test(&"1".repeat(127), true).unwrap();
    assert!(forms.tts.len() > 127);
    assert!(forms.tts.len() <= 128 * 128);
    assert_eq!(forms.tts.matches(&fragment).count(), 127);
    let missing =
        MessageCatalog::from_sources("de", ENGLISH, Some("dtmf-symbol =\n    .text = taste\n"))
            .unwrap();
    assert_eq!(missing.format_pad_test("*", false).unwrap().tts, "star");
}

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
fn message_catalog_formats_parrot_level_report_in_all_required_forms() {
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let forms = catalog
        .format(&Message::ParrotLevels {
            peak_dbfs: -3,
            rms_dbfs: -18,
        })
        .unwrap();
    assert_eq!(forms.text, "Peak level -3 dBFS. RMS level -18 dBFS.");
    assert_eq!(forms.tts, "Peak level -3 dBFS. RMS level -18 dBFS.");
    assert_eq!(forms.morse, "PEAK -3 DBFS RMS -18 DBFS");
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
            .unwrap(),
        MessageForms {
            text: "Blind Hams Network: NODE 506315 CONNECTED".into(),
            tts: "Blind Hams Network. 506315 connected".into(),
            morse: "Blind Hams Network NODE 506315 CONNECTED".into(),
        }
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
                target: "Handiham Net",
                connecting: true,
                time_remaining: "1 minute",
            })
            .unwrap()
            .tts,
        "Connecting to Handiham Net in 1 minute"
    );
}

#[test]
fn schedule_warning_preserves_remaining_minutes_and_seconds() {
    use crate::schedule::{CivilTime, Weekday};
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let target = MessageForms {
        text: "Handiham Net".into(),
        tts: "Handiham Net".into(),
        morse: "Handiham Net".into(),
    };
    assert_eq!(
        catalog
            .format_schedule_warning(90, local, &target, true)
            .unwrap()
            .tts,
        "Connecting to Handiham Net in 1 minute, and 30 seconds"
    );
}

#[test]
fn duration_preserves_all_nonzero_units_and_localizes_plurals() {
    use crate::schedule::{CivilTime, Weekday};
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    for (seconds, expected, morse) in [
        (0, "0 seconds", "0S"),
        (1, "1 second", "1S"),
        (2, "2 seconds", "2S"),
        (59, "59 seconds", "59S"),
        (60, "1 minute", "1M"),
        (61, "1 minute, and 1 second", "1M 1S"),
        (90, "1 minute, and 30 seconds", "1M 30S"),
        (120, "2 minutes", "2M"),
        (3599, "59 minutes, and 59 seconds", "59M 59S"),
        (3600, "1 hour", "1H"),
        (5400, "1 hour, and 30 minutes", "1H 30M"),
        (7200, "2 hours", "2H"),
        (86399, "23 hours, 59 minutes, and 59 seconds", "23H 59M 59S"),
        (86400, "1 day", "1D"),
        (91800, "1 day, 1 hour, and 30 minutes", "1D 1H 30M"),
        (172800, "2 days", "2D"),
        (518400, "6 days", "6D"),
        (604800, "1 week", "1W"),
        (1209600, "2 weeks", "2W"),
        (2678400, "1 month", "1M"),
        (5356800, "2 months", "2M"),
        (
            3373381,
            "1 month, 1 week, 1 day, 1 hour, 3 minutes, and 1 second",
            "1M 1W 1D 1H 3M 1S",
        ),
    ] {
        let forms = catalog.format_duration(seconds, local).unwrap();
        assert_eq!(forms.text, expected, "{seconds}");
        assert_eq!(forms.tts, expected, "{seconds}");
        assert_eq!(forms.morse, morse, "{seconds}");
    }
}

#[test]
fn duration_month_length_uses_the_current_local_month_and_gregorian_leap_year() {
    use crate::schedule::{CivilTime, Weekday};
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    for (year, month, days) in [
        (2026, 1, 31),
        (2026, 2, 28),
        (2024, 2, 29),
        (2000, 2, 29),
        (2100, 2, 28),
        (2026, 4, 30),
    ] {
        let local = CivilTime::new(year, month, 1, Weekday::Monday, 0, 0).unwrap();
        assert_eq!(
            catalog.format_duration(days * 86400, local).unwrap().tts,
            "1 month",
            "{year}-{month}"
        );
        assert!(
            !catalog
                .format_duration(days * 86400 - 1, local)
                .unwrap()
                .tts
                .contains("month"),
            "{year}-{month}"
        );
        assert_eq!(
            catalog
                .format_duration(days * 86400 + 1, local)
                .unwrap()
                .tts,
            "1 month, and 1 second",
            "{year}-{month}"
        );
    }
}

#[test]
fn schedule_warning_uses_the_matching_localized_duration_form() {
    use crate::schedule::{CivilTime, Weekday};
    let french = "duration-unit =\n    .text = texte { $count } { $unit }\n    .tts = voix { $count } { $unit }\n    .morse = morse { $count } { $unit }\nduration-join =\n    .text = { $head } et { $tail }\n    .tts = { $head } puis { $tail }\n    .morse = { $head } { $tail }\nscheduled-link-change =\n    .text = texte { $target } { $time_remaining }\n    .tts = voix { $target } { $time_remaining }\n    .morse = morse { $target } { $time_remaining }\n";
    let catalog = MessageCatalog::from_sources("fr-CA", ENGLISH, Some(french)).unwrap();
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let target = MessageForms {
        text: "texte Net".into(),
        tts: "voix Net".into(),
        morse: "morse Net".into(),
    };
    assert_eq!(
        catalog
            .format_schedule_warning(90, local, &target, false)
            .unwrap(),
        super::messages::MessageForms {
            text: "texte texte Net texte 1 minute et texte 30 second".into(),
            tts: "voix voix Net voix 1 minute puis voix 30 second".into(),
            morse: "morse morse Net morse 1 minute morse 30 second".into(),
        }
    );
}

#[test]
fn schedule_warning_formats_both_directions_and_preserves_message_bounds() {
    use crate::schedule::{CivilTime, Weekday};
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let target = MessageForms {
        text: "Net".into(),
        tts: "node,1".into(),
        morse: "Net".into(),
    };
    for (connecting, expected, morse) in [
        (
            true,
            "Connecting to Net in 1 hour",
            "CONNECTING TO Net IN 1H",
        ),
        (
            false,
            "Disconnecting from Net in 1 hour",
            "DISCONNECTING FROM Net IN 1H",
        ),
    ] {
        let forms = catalog
            .format_schedule_warning(3600, local, &target, connecting)
            .unwrap();
        assert_eq!(forms.text, expected);
        assert_eq!(forms.tts, expected.replace("Net", "node,1"));
        assert_eq!(forms.morse, morse);
    }
    let long = MessageForms {
        text: "x".repeat(128),
        tts: "Net".into(),
        morse: "Net".into(),
    };
    assert_eq!(
        catalog.format_schedule_warning(60, local, &long, true),
        Err(CatalogError::OutputTooLong)
    );
}

#[test]
fn schedule_warning_keeps_valid_long_names_and_compound_durations() {
    use crate::schedule::{CivilTime, Weekday};
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    let local = CivilTime::new(2026, 10, 21, Weekday::Wednesday, 12, 0).unwrap();
    let target = MessageForms {
        text: "N1".repeat(31) + "A",
        tts: "N,1,".repeat(31) + "A",
        morse: "N1".repeat(31) + "A",
    };
    let forms = catalog
        .format_schedule_warning(60, local, &target, true)
        .unwrap();
    assert_eq!(
        forms.tts,
        format!("Connecting to {} in 1 minute", target.tts)
    );
    assert!(forms.tts.len() > 127);

    let target = MessageForms {
        text: "R".repeat(63),
        tts: "R".repeat(63),
        morse: "R".repeat(63),
    };
    let forms = catalog
        .format_schedule_warning(3373381, local, &target, false)
        .unwrap();
    assert_eq!(
        forms.tts,
        format!(
            "Disconnecting from {} in 1 month, 1 week, 1 day, 1 hour, 3 minutes, and 1 second",
            target.tts
        )
    );
    assert!(forms.tts.len() > 127);
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

#[test]
fn catalog_debug_and_optional_translation_failures_are_readable() {
    let catalog = MessageCatalog::from_sources("fr-CA", ENGLISH, None).unwrap();
    assert_eq!(format!("{catalog:?}"), "MessageCatalog");
    assert_eq!(catalog.translation_warnings(), &["catalog unavailable"]);

    let malformed = MessageCatalog::from_sources("fr-CA", ENGLISH, Some("!!!")).unwrap();
    assert_eq!(malformed.translation_warnings(), &["catalog is malformed"]);
}

#[test]
fn weekday_message_uses_each_localized_weekday_key() {
    use crate::schedule::Weekday;

    let weekdays = [
        Weekday::Sunday,
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
    ];
    let catalog = MessageCatalog::from_sources("en-US", ENGLISH, None).unwrap();
    for (day, name) in weekdays.into_iter().zip([
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ]) {
        assert_eq!(
            catalog.format(&Message::DayOfWeek { day }).unwrap().text,
            name
        );
    }
}

#[test]
fn english_catalog_rejects_oversized_builtin_output() {
    let oversized = ENGLISH.replace(
        "    .text = NO LINKS\n",
        &format!("    .text = {}\n", "x".repeat(128)),
    );
    assert_eq!(
        MessageCatalog::from_sources("en-US", &oversized, None).unwrap_err(),
        CatalogError::InvalidEnglish
    );
}

#[test]
fn english_catalog_rejects_fluent_format_errors() {
    let invalid = ENGLISH.replace("    .text = NO LINKS\n", "    .text = { $missing }\n");
    assert_eq!(
        MessageCatalog::from_sources("en-US", &invalid, None).unwrap_err(),
        CatalogError::InvalidEnglish
    );
}

#[test]
fn catalog_load_uses_missing_locale_fallback() {
    let catalog = MessageCatalog::load("fr-CA").unwrap();
    assert!(catalog.format(&Message::NoLinks).is_ok());
}
