//! Cross-protocol routing preserves human identity, audience and literal content.
use rpt_advanced_core::link::{HumanMessage, PeerIdentity, ProtocolId, TextAudience, TextOrigin};
use rptadv_product::link::text::{TextDestination, TextPayload, TextRouter};
use std::sync::Arc;

fn message() -> HumanMessage {
    HumanMessage {
        origin: TextOrigin {
            station_generation: 1,
            connection_generation: 7,
            peer: "3654321".into(),
            protocol: 2,
        },
        source: "W1XYZ-R".into(),
        represented_source: Some(PeerIdentity::parse(ProtocolId::EchoLink, "123456").unwrap()),
        audience: TextAudience::Broadcast,
        body: Arc::from("literal ${time} !NEWKEY!"),
    }
}
fn peers() -> Vec<TextDestination> {
    [("2000", 1), ("3654321", 2), ("3123456", 2), ("3777777", 2)]
        .into_iter()
        .enumerate()
        .map(|(i, (node, protocol))| TextDestination {
            connection_generation: 6 + i as u64,
            protocol,
            node: node.into(),
            can_forward_text: true,
        })
        .collect()
}
#[test]
fn relayed_echo_sender_keeps_its_alias_and_never_echoes_to_ingress_or_source() {
    let router = TextRouter::new(vec!["524950".into()]);
    let mut sent = Vec::new();
    let result = router.route(&message(), &peers(), |peer, payload| {
        let value = match payload {
            TextPayload::AllStarLink(wire) => wire.to_str().unwrap().to_owned(),
            TextPayload::EchoLink { sender, body } => format!("{sender}>{body}"),
        };
        sent.push((peer.node.clone(), value));
        true
    });
    assert!(result.locally_eligible);
    assert_eq!(result.destinations_admitted, 2);
    assert_eq!(result.destinations_rejected, 0);
    assert_eq!(
        sent,
        [
            ("2000".into(), "M 3123456 0 literal ${time} !NEWKEY!".into()),
            ("3777777".into(), "W1XYZ-R>literal ${time} !NEWKEY!".into()),
        ]
    );
}
#[test]
fn unknown_echo_sender_stays_local_and_echo_capable_without_fabricating_an_asl_identity() {
    let router = TextRouter::new(vec!["524950".into()]);
    let mut message = message();
    message.represented_source = None;
    let mut echo = 0;
    let result = router.route(&message, &peers(), |_, payload| {
        assert!(matches!(
            payload,
            TextPayload::EchoLink {
                sender: "W1XYZ-R",
                ..
            }
        ));
        echo += 1;
        true
    });
    assert!(result.locally_eligible);
    assert_eq!((echo, result.destinations_rejected), (2, 1));
}
#[test]
fn directed_asl_text_is_not_rewritten_as_echo_broadcast() {
    let router = TextRouter::new(vec!["524950".into()]);
    let mut message = message();
    message.origin.protocol = 1;
    message.origin.peer = "9000".into();
    message.source = "4000".into();
    message.represented_source = PeerIdentity::from_operator("4000").ok();
    message.audience = TextAudience::Node("8000".into());
    let result = router.route(&message, &peers(), |_, payload| {
        let TextPayload::AllStarLink(wire) = payload else {
            panic!("directed broadcast")
        };
        assert_eq!(wire.to_bytes(), b"M 4000 8000 literal ${time} !NEWKEY!");
        true
    });
    assert!(!result.locally_eligible);
    assert_eq!(
        (result.destinations_admitted, result.destinations_rejected),
        (1, 3)
    );
    message.audience = TextAudience::Broadcast;
    let mut echo = 0;
    let result = router.route(&message, &peers(), |_, payload| {
        if let TextPayload::EchoLink { sender, body } = payload {
            assert_eq!(sender, "4000");
            assert_eq!(body, "literal ${time} !NEWKEY!");
            echo += 1;
        }
        true
    });
    assert_eq!(
        (
            echo,
            result.destinations_admitted,
            result.destinations_rejected
        ),
        (3, 4, 0)
    );
}
