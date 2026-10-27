//! Fixtures follow ASL b6cc1cf app_rpt.c1948-1978 and rpt_cli.c903,1030,
//! not the obsolete wiki's reversed TO/FROM labels. No app_rpt code is copied.
use super::*;

fn origin() -> TextOrigin {
    TextOrigin {
        station_generation: 7,
        connection_generation: 11,
        peer: "2000".into(),
        protocol: 1,
    }
}

#[test]
fn text_classification_human_origin_audience_and_unicode() {
    for (wire, audience, body) in [
        (
            &b"M 3000 0 hello world"[..],
            TextAudience::Broadcast,
            "hello world",
        ),
        (
            &b"M 3000 1000 hello\0"[..],
            TextAudience::Node("1000".into()),
            "hello",
        ),
        (
            "M 3000 0 café 🌍".as_bytes(),
            TextAudience::Broadcast,
            "café 🌍",
        ),
        (
            &b"M  3000   0  spaces"[..],
            TextAudience::Broadcast,
            "spaces",
        ),
        (
            &b"M 3000 0 !NEWKEY1!"[..],
            TextAudience::Broadcast,
            "!NEWKEY1!",
        ),
    ] {
        let ClassifiedText::Human(message) = classify_asl(origin(), wire) else {
            panic!("human fixture rejected")
        };
        assert_eq!(message.origin, origin());
        assert_eq!(message.source, "3000"); // A relayed sender need not be the ingress peer.
        assert_eq!(message.audience, audience);
        assert_eq!(&*message.body, body);
    }
}

#[test]
fn text_classification_never_speaks_control_unknown_or_malformed() {
    for control in [
        &b"L T2000"[..],
        b"!!DISCONNECT!!",
        b"!NEWKEY!",
        b"!NEWKEY1!",
        b"!IAXKEY!",
        b"K? * 2000 0 0",
        b"K 1000 2000 1 0",
        b"D 1000 2000 17 5",
    ] {
        assert!(matches!(
            classify_asl(origin(), control),
            ClassifiedText::Control(_)
        ));
    }
    for unknown in [
        &b"T 2000 STATUS,2000,0"[..],
        b"ordinary unknown text",
        &[0xff],
        b"M 2000 0",
        b"M 2000 0 ",
        b"M 2000 0 a",
        b"M bad! 0 hello",
        b"M 2000 bad! hello",
        b"M 2000 0 hello\0hidden",
        b"M 2000 0 hello\0\0",
        b"M 2000 0 hello\ncontrol",
    ] {
        assert_eq!(classify_asl(origin(), unknown), ClassifiedText::Unknown);
    }
}
