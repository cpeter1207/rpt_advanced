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

#[test]
fn private_prompt_queue_requires_the_exact_live_token_and_never_uses_radio_media() {
    struct Prompt;
    impl rpt_advanced_core::audio::PcmStreamReader for Prompt {
        fn render(&mut self, output: &mut [f32]) -> rpt_advanced_core::audio::PcmRead {
            let count = output.len().min(8);
            output[..count].fill(0.25);
            rpt_advanced_core::audio::PcmRead::FinalSamples(count)
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let foreign = rpt_advanced_core::link::PeerSignals::for_phone(source.clone()).reply_target();
    assert!(matches!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(foreign.clone())),
            clock()
        ),
        Err(RuntimeError::Rejected)
    ));
    assert!(
        host.queue_phone_reply(&source, &foreign, Box::new(Prompt))
            .is_err()
    );
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    for _ in 0..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .unwrap();
    }
    assert!(
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .is_err()
    );
    let mut prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    let mut output = [0.0; 16];
    assert_eq!(prepared.render(false, &mut output), 8);
    assert_eq!(&output[..8], &[0.25; 8]);
    assert_eq!(&output[8..], &[0.0; 8]);
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(host.retire_phone(&source));
    assert!(!prepared.current());
    assert!(host.take_phone_reply(&source, &reply).is_err());
    assert!(
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn removed_node_reload_retires_its_pending_private_call_before_reclaiming_the_node() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.reload(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        clock(),
    )
    .unwrap();
    assert!(host.runtime.node("1000").is_none());
    assert!(reply.ended());
    assert!(host.phone_source(call).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(state.lock().unwrap().bound_radio.is_none());
    pump_until(&mut host, |host| host.runtime.status(20).is_empty());
    assert!(host.stop(20));
}
