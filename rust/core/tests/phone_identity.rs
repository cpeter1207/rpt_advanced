//! Opaque calls never acquire numeric node aliases or authority from login metadata.
use rpt_advanced_core::link::{
    MailboxId, PeerAddress, PeerIdentity, PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource,
    ProtocolId,
};

#[test]
fn phone_identity_has_no_public_node_or_operator_alias() {
    let wire = "aabbccddeeff00112233445566778899";
    let call: PhoneCallId = wire.parse().unwrap();
    let phone = PeerIdentity::parse(ProtocolId::Phone, wire).unwrap();
    assert_eq!(phone.address(), &PeerAddress::PhoneCall(call));
    assert_eq!(phone.node(), None);
    assert_eq!(phone.operator_alias(), None);
    assert_eq!(phone.wire_identity(), wire);
    assert_eq!(call.to_string(), wire);
    assert_eq!(
        PeerIdentity::parse(ProtocolId::AllStarLink, "2000")
            .unwrap()
            .call_id(),
        None
    );
    assert!(PeerIdentity::from_operator(wire).is_err());
    for invalid in [
        "",
        "12345",
        "00000000000000000000000000000000",
        "AABBCCDDEEFF00112233445566778899",
        "aabbccddeeff0011223344556677889z",
    ] {
        assert!(
            invalid.parse::<PhoneCallId>().is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn phone_source_keeps_provider_call_and_actual_ingress_generations_distinct() {
    let account = PhoneAccountId::new("provider-main").unwrap();
    let call = "aabbccddeeff00112233445566778899".parse().unwrap();
    let source = PhoneSource::new(account.clone(), 3, call, 4, 5).unwrap();
    assert_eq!(source.account(), &account);
    assert_eq!(source.account_generation(), 3);
    assert_eq!(source.call_id(), call);
    assert_eq!(source.call_generation(), 4);
    assert_eq!(source.ingress(), 5);
    for (account_generation, call_generation, ingress) in [(0, 4, 5), (3, 0, 5), (3, 4, 0)] {
        assert!(
            PhoneSource::new(
                account.clone(),
                account_generation,
                call,
                call_generation,
                ingress
            )
            .is_err()
        );
    }
    for invalid in ["", "provider secret", "provider/other", &"a".repeat(65)] {
        assert!(PhoneAccountId::new(invalid).is_err());
    }
    assert_eq!(MailboxId::new("00123").unwrap().as_str(), "00123");
    for invalid in ["", "W1XYZ", &"1".repeat(33)] {
        assert!(MailboxId::new(invalid).is_err());
    }
    let signals = PeerSignals::for_phone(source.clone());
    let reply = signals.reply_target();
    assert_eq!(reply.phone_source(), Some(&source));
    assert!(!reply.phone_authenticated());
    assert!(signals.authenticate_phone());
    assert!(reply.phone_authenticated());
    let ordinary = PeerSignals::new();
    assert!(!ordinary.authenticate_phone());
    assert_eq!(ordinary.reply_target().phone_source(), None);
    signals.end();
    assert!(reply.ended());
    assert!(!reply.phone_authenticated());
    assert!(!signals.authenticate_phone());
}
