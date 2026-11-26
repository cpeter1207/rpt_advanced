use super::*;
use crate::link::Mode;

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
fn speech_pronunciation_preserves_preformatted_identities() {
    for (source, expected) in [
        ("node,2,0,0,0", "node,2,0,0,0"),
        ("AB1CD", "A,B,1,C,D"),
        ("A,B,1,C,D.", "A,B,1,C,D."),
        ("Temperature 75 degrees", "Temperature 75 degrees"),
        (
            "AB1CD linked node,2,0,0,0.",
            "A,B,1,C,D linked node,2,0,0,0.",
        ),
    ] {
        assert_eq!(telemetry_speech(source, &["1000"]), expected);
    }
}
