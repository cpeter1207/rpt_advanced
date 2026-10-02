use super::*;
use crate::{link::Mode, schedule::Weekday};

fn catalog() -> MessageCatalog {
    MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap()
}

#[test]
fn direct_status_describes_every_mode_and_excludes_retiring_or_retrying_peers() {
    let catalog = catalog();
    for (mode, name) in [
        (Mode::TRANSCEIVE, "TRANSCEIVE"),
        (Mode::MONITOR, "MONITOR"),
        (Mode::LOCAL_MONITOR, "LOCAL"),
    ] {
        let mut links = LinkManager::new("1000").unwrap();
        links.attach("2000", mode, false).unwrap();
        let (forms, peer) = direct_status(&links, &catalog).unwrap();
        assert_eq!(forms.text, format!("LINK 2000 {name}"));
        assert_eq!(peer, "2000");
        links.attach("3000", Mode::TRANSCEIVE, false).unwrap();
        assert_eq!(
            direct_status(&links, &catalog).unwrap().0.text,
            format!("2 LINKS 2000 {name}")
        );
        links.end("3000");
        links.retain_retry("4000", Mode::TRANSCEIVE, 0).unwrap();
        assert_eq!(
            direct_status(&links, &catalog).unwrap().0.text,
            format!("LINK 2000 {name}")
        );
    }
}

#[test]
fn direct_status_excludes_topology_blocked_automatic_intent() {
    let catalog = catalog();
    let mut links = LinkManager::new("1000").unwrap();
    links.attach("2000", Mode::TRANSCEIVE, false).unwrap();
    links.update_topology("2000", b"L T3000").unwrap();
    links.retain_topology_blocked_group("3000", Mode::TRANSCEIVE, None, None);
    links.retain_topology_blocked_group("3000", Mode::MONITOR, None, None);
    let retry = links
        .snapshot()
        .into_iter()
        .find(|peer| peer.name == "3000")
        .unwrap();
    assert_eq!(retry.mode, Mode::MONITOR);
    assert!(retry.topology_blocked && retry.permanent && !retry.paused);
    assert_eq!(
        direct_status(&links, &catalog).unwrap().0.text,
        "LINK 2000 TRANSCEIVE"
    );
}

#[test]
fn scheduled_greetings_use_captured_civil_time_and_reject_unknown_clock_format() {
    let catalog = catalog();
    let template = MessageTemplate::parse("${greeting}").unwrap();
    let links = LinkManager::new("1000").unwrap();
    for (hour, greeting) in [
        (0, "Good Morning"),
        (12, "Good Afternoon"),
        (17, "Good Evening"),
    ] {
        let civil = CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
        assert_eq!(
            message(&template, civil, 24, "1000", "W1AW", &links, &catalog),
            Ok(greeting.into())
        );
        assert_eq!(
            message(&template, civil, 13, "1000", "W1AW", &links, &catalog),
            Err(RuntimeError::Preparation)
        );
    }
}

#[test]
fn scheduled_day_of_week_substitution_uses_the_node_catalog() {
    let catalog = MessageCatalog::from_sources(
        "fr-CA",
        include_str!("../../../../messages/en-US.ftl"),
        Some("day-of-week =\n    .text = Vendredi\n    .tts = Vendredi\n    .morse = VENDREDI\n"),
    )
    .unwrap();
    let template = MessageTemplate::parse("${day_of_week}").unwrap();
    let links = LinkManager::new("1000").unwrap();
    let civil = CivilTime::new(2026, 9, 18, Weekday::Friday, 9, 0).unwrap();
    assert_eq!(
        message(&template, civil, 24, "1000", "W1AW", &links, &catalog),
        Ok("Vendredi".into())
    );
}
