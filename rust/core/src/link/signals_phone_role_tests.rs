//! Outgoing transport ownership grants media, never mailbox/control authority.
use super::*;
use crate::link::{PhoneAccountId, PhoneCallId, PhoneSource};

fn source(ingress: u64) -> PhoneSource {
    PhoneSource::new(
        PhoneAccountId::new("office").unwrap(),
        7,
        PhoneCallId::new([1; 16]).unwrap(),
        9,
        ingress,
    )
    .unwrap()
}

#[test]
fn outgoing_phone_role_has_media_only_and_exact_retirement() {
    let source = source(1);
    let outgoing = PeerSignals::for_outgoing_phone(source.clone());
    let reply = outgoing.reply_target();
    assert_eq!(reply.phone_source(), Some(&source));
    assert!(
        reply.phone_media_authorized(),
        "owned outgoing call must receive a media-only grant"
    );
    assert!(reply.phone_outgoing());
    assert!(
        !reply.phone_authenticated(),
        "callee is not an authenticated reverse caller"
    );
    assert!(
        !outgoing.authenticate_phone(),
        "incoming authentication cannot change an outgoing role"
    );
    let domain = RetirementDomain::default();
    reply.bind_retirement_domain(&domain);
    let other = PeerSignals::for_outgoing_phone(self::source(2));
    outgoing.end();
    assert!(!reply.phone_media_authorized());
    assert!(!reply.phone_authenticated());
    assert!(!outgoing.authenticate_phone());
    assert!(reply.ended());
    assert_eq!(domain.revision(), Some(1));
    assert!(
        other.reply_target().phone_media_authorized(),
        "retiring one exact call must not end another"
    );
}

#[test]
fn incoming_phone_role_still_requires_mailbox_and_cannot_become_outgoing() {
    let incoming = PeerSignals::for_phone(source(1));
    let reply = incoming.reply_target();
    assert!(!reply.phone_media_authorized());
    assert!(!reply.phone_authenticated());
    assert!(!reply.phone_outgoing());
    assert!(incoming.authenticate_phone());
    assert!(reply.phone_media_authorized());
    assert!(reply.phone_authenticated());
    assert!(!reply.phone_outgoing());
    incoming.end();
    assert!(!reply.phone_media_authorized());
    assert!(!reply.phone_authenticated());
    let ordinary = PeerSignals::new();
    assert!(!ordinary.authenticate_phone());
    assert!(!ordinary.reply_target().phone_media_authorized());
}
