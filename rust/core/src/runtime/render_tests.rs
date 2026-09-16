use super::*;
use crate::{link::Mode, schedule::Weekday};

#[test]
fn direct_status_describes_every_mode_and_excludes_retiring_or_retrying_peers() {
    for (mode, name) in [
        (Mode::TRANSCEIVE, "TRANSCEIVE"),
        (Mode::MONITOR, "MONITOR"),
        (Mode::LOCAL_MONITOR, "LOCAL"),
    ] {
        let mut links = LinkManager::new("1000").unwrap();
        links.attach("2000", mode, false).unwrap();
        assert_eq!(
            direct_status(&links),
            (format!("LINK 2000 {name}"), "2000".into())
        );
        links.attach("3000", Mode::TRANSCEIVE, false).unwrap();
        assert_eq!(
            direct_status(&links),
            (format!("2 LINKS 2000 {name}"), "2000".into())
        );
        links.end("3000");
        links.retain_retry("4000", Mode::TRANSCEIVE, 0).unwrap();
        assert_eq!(
            direct_status(&links),
            (format!("LINK 2000 {name}"), "2000".into())
        );
    }
}

#[test]
fn direct_status_excludes_topology_blocked_automatic_intent() {
    let mut links = LinkManager::new("1000").unwrap();
    links.attach("2000", Mode::TRANSCEIVE, false).unwrap();
    links.update_topology("2000", b"L T3000").unwrap();
    links.retain_topology_blocked("3000", Mode::TRANSCEIVE);
    assert_eq!(
        direct_status(&links),
        ("LINK 2000 TRANSCEIVE".into(), "2000".into())
    );
}

#[test]
fn scheduled_greetings_use_captured_civil_time_and_reject_unknown_clock_format() {
    let template = MessageTemplate::parse("${greeting}").unwrap();
    let links = LinkManager::new("1000").unwrap();
    for (hour, greeting) in [
        (0, "Good Morning"),
        (12, "Good Afternoon"),
        (17, "Good Evening"),
    ] {
        let civil = CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
        assert_eq!(
            message(&template, civil, 24, "1000", "W1AW", &links),
            Ok(greeting.into())
        );
        assert_eq!(
            message(&template, civil, 13, "1000", "W1AW", &links),
            Err(RuntimeError::Preparation)
        );
    }
}
