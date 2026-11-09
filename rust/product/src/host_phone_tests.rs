//! Pending phone calls have private control ownership, never a radio-peer slot.
use super::*;
use rpt_advanced_core::{command::LinkAction, link::MailboxId};

#[test]
fn pending_phone_has_no_handshake_topology_or_radio_attachment_and_queues_only_private_status() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    assert!(host.peers.is_empty());
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .links()
            .manager()
            .snapshot()
            .is_empty()
    );
    let source = host.phone_source(call).unwrap().clone();
    assert_eq!(source.account_generation(), 1);
    assert_eq!(source.call_generation(), 2);
    assert_ne!(source.ingress(), 0);
    let reply = host.phone_reply(&source).unwrap();
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
            clock()
        )
        .is_err()
    );
    host.authenticate_phone(&source, &reply, MailboxId::new("100").unwrap())
        .unwrap();
    host.immediate(
        "1000",
        LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
        clock(),
    )
    .unwrap();
    let prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(prepared.source(), &source);
    assert!(prepared.current());
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Message {
                selector: "1".into(),
                reply: Some(reply.clone())
            },
            clock()
        )
        .is_err(),
        "unimplemented private message preparation must fail explicitly"
    );
    let (other, rejected) = crate::link::session::tests::phone_peer();
    assert!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, other, clock())
            .is_err()
    );
    assert!(rejected.lock().unwrap().bound_radio.is_none());
    assert!(host.retire_phone(&source));
    assert!(reply.ended());
    assert!(!prepared.current());
    assert!(host.phone_source(call).is_none());
    assert!(!host.retire_phone(&source));
    let state = state.lock().unwrap();
    assert!(
        state.texts.is_empty(),
        "no ASL protocol text may reach a phone"
    );
    assert!(state.audio.is_empty());
    assert!(state.bound_radio.is_none());
    assert_eq!(state.drops, 1);
    drop(state);
    assert!(host.stop(20));
}

#[test]
fn phone_reload_and_unload_retire_the_actual_call_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.reload(document("usb", "ctcss_encode=no\n"), clock())
        .unwrap();
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    let (phone, _) = crate::link::session::tests::phone_peer();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(fresh.ingress(), source.ingress());
    let reply = host.phone_reply(&fresh).unwrap();
    assert!(host.stop(20));
    assert!(reply.ended());
}
