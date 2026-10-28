use super::*;
use rpt_advanced_core::link::{HumanMessage, TextAudience, TextOrigin};
use std::sync::Arc;

fn message(audience: TextAudience) -> HumanMessage {
    HumanMessage {
        origin: TextOrigin {
            station_generation: 1,
            connection_generation: 7,
            peer: "2000".into(),
            protocol: 1,
        },
        source: "4000".into(),
        audience,
        body: Arc::from("literal ${time} hello"),
    }
}
fn destinations() -> Vec<TextDestination> {
    ["2000", "3000", "4000", "0500", "6000", "7000"]
        .iter()
        .enumerate()
        .map(|(index, node)| TextDestination {
            connection_generation: 7 + index as u64,
            protocol: 1,
            node: (*node).into(),
            can_forward_text: *node != "7000",
        })
        .collect()
}
#[test]
fn text_forwarding_preserves_asl_flood_exclusions_and_independent_destination_admission() {
    let router = TextRouter::new(vec!["1000".into(), "1001".into()]);
    let peers = destinations();
    let mut seen = Vec::new();
    let disposition = router.route(&message(TextAudience::Broadcast), &peers, |peer, wire| {
        seen.push((peer.node.clone(), wire.to_str().unwrap().to_owned()));
        peer.node != "3000"
    });
    assert!(disposition.locally_eligible);
    assert_eq!(
        (
            disposition.destinations_admitted,
            disposition.destinations_rejected
        ),
        (1, 1)
    );
    assert_eq!(
        seen,
        [
            ("3000".into(), "M 4000 0 literal ${time} hello".into()),
            ("6000".into(), "M 4000 0 literal ${time} hello".into())
        ]
    );
    for (audience, local, forwarded) in [
        (TextAudience::Node("1001".into()), true, 0),
        (TextAudience::Node("8000".into()), false, 2),
    ] {
        let result = router.route(&message(audience), &peers, |_, _| true);
        assert_eq!(
            (result.locally_eligible, result.destinations_admitted),
            (local, forwarded)
        );
    }
    let mut local = message(TextAudience::Broadcast);
    local.source = "1000".into();
    let result = router.route(&local, &peers, |_, _| {
        panic!("local-origin text must not loop")
    });
    assert!(!result.locally_eligible);
    assert_eq!(result.destinations_admitted, 0);
}

#[test]
fn text_forwarding_large_body_is_not_limited_by_local_synthesis_quota_or_failure() {
    let router = TextRouter::new(vec!["1000".into()]);
    let mut message = message(TextAudience::Broadcast);
    message.body = Arc::from("é".repeat(3000));
    let mut forwarded = 0;
    let result = router.route(&message, &destinations(), |_, wire| {
        assert!(wire.to_bytes().ends_with(message.body.as_bytes()));
        forwarded += 1;
        true
    });
    assert!(result.locally_eligible);
    assert_eq!(forwarded, 2);
    message.body = Arc::from("invalid\0wire");
    let result = router.route(&message, &destinations(), |_, _| {
        panic!("NUL cannot cross the C text boundary")
    });
    assert_eq!(result.destinations_rejected, 2);
    message.body = Arc::from("normal text");
    let mut other = destinations().remove(1);
    other.protocol = 2;
    let result = router.route(&message, &[other], |_, _| {
        panic!("no serializer for this protocol")
    });
    assert_eq!(result.destinations_rejected, 1);
}
