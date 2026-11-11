use super::*;
use crate::link::{PeerIdentity, PeerReply, PhoneAccountId, PhoneCallId, PhoneSource, ProtocolId};

struct PhoneInput {
    signals: PeerSignals,
    reply: PeerReply,
    policy: ReceivePolicy,
}
impl PeerInput for PhoneInput {
    fn phone_reply(&self) -> Option<&PeerReply> {
        Some(&self.reply)
    }
    fn source_rate(&self) -> u32 {
        48000
    }
    fn receive_policy(&self) -> ReceivePolicy {
        self.policy
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        16000
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        output.fill(0.25);
        Some(output.len())
    }
}
fn source(account: u64, call: u64, ingress: u64) -> PhoneSource {
    PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        account,
        PhoneCallId::new([1; 16]).unwrap(),
        call,
        ingress,
    )
    .unwrap()
}
fn reply(source: PhoneSource, authenticate: bool) -> PeerReply {
    let signals = PeerSignals::for_phone(source);
    if authenticate {
        assert!(signals.authenticate_phone());
    }
    signals.reply_target()
}
fn input(reply: PeerReply) -> PhoneInput {
    let signals = PeerSignals::new();
    signals.publish_pcm();
    signals.set_radio_keyed(true);
    PhoneInput {
        signals,
        reply,
        policy: ReceivePolicy::Phone,
    }
}

#[test]
fn typed_phone_mixer_requires_the_exact_source_and_reply_not_only_call_id() {
    let original = source(7, 9, 13);
    for case in 0..10 {
        let admitted = reply(original.clone(), case != 0);
        let expected = if case == 1 {
            reply(original.clone(), true)
        } else {
            admitted.clone()
        };
        let candidate = match case {
            2 => source(8, 9, 13),
            3 => source(7, 10, 13),
            4 => source(7, 9, 14),
            5 => PhoneSource::new(
                PhoneAccountId::new("other").unwrap(),
                7,
                original.call_id(),
                9,
                13,
            )
            .unwrap(),
            6 => PhoneSource::new(
                PhoneAccountId::new("provider").unwrap(),
                7,
                PhoneCallId::new([2; 16]).unwrap(),
                9,
                13,
            )
            .unwrap(),
            _ => original.clone(),
        };
        if case == 7 {
            admitted.retire();
        }
        let mut receive = input(admitted);
        if case == 9 {
            receive.policy = ReceivePolicy::Peer;
        }
        let (writer, _) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
        let result =
            AudioPeer::for_phone(&candidate, &expected, Mode::TRANSCEIVE, receive, writer, 4);
        assert_eq!(result.is_ok(), case == 8, "case {case}");
    }
    for identity in [
        PeerIdentity::for_phone(original.call_id()),
        PeerIdentity::parse(ProtocolId::AllStarLink, "1000").unwrap(),
    ] {
        let current = reply(original.clone(), true);
        let (writer, _) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
        assert!(
            AudioPeer::new(identity, Mode::TRANSCEIVE, input(current), writer, 4, 0).is_err(),
            "public construction cannot bypass exact private admission"
        );
    }
    let current = reply(original.clone(), true);
    let (writer, _) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
    assert!(
        AudioPeer::for_phone(
            &original,
            &current,
            Mode::TRANSCEIVE,
            Input {
                signals: Arc::new(PeerSignals::new())
            },
            writer,
            4,
        )
        .is_err(),
        "ordinary input is not the admitted private call"
    );
}

#[test]
fn typed_phone_mixer_omits_public_identity_and_revokes_queued_egress() {
    for retire_call in [false, true] {
        let source = source(7, 9, 13);
        let current = reply(source.clone(), true);
        let (writer, mut output) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
        let peer = AudioPeer::for_phone(
            &source,
            &current,
            Mode::TRANSCEIVE,
            input(current.clone()),
            writer,
            4,
        )
        .unwrap();
        let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
        let status = links.status();
        let (mut controller, _) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        links
            .process(
                &mut controller,
                true,
                &mut [0.5; 4],
                SourcePosition::default(),
            )
            .unwrap();
        assert!(links.peers[0].active);
        assert_eq!(status.last_keyed(), None);
        assert_eq!(links.routing[0].target, current.identity());
        assert!(!links.routing[0].telemetry);
        dispatcher.dispatch(1);
        let mut sent = [0.0; 4];
        assert_eq!(output.read(&mut sent), 0);
        assert!(
            sent.iter().any(|sample| *sample != 0.0),
            "use the real dispatcher"
        );
        links
            .process(
                &mut controller,
                true,
                &mut [0.5; 4],
                SourcePosition::default(),
            )
            .unwrap();
        if retire_call {
            current.retire();
        } else {
            links.peers[0].input.signals.end();
        }
        dispatcher.dispatch(1);
        assert_eq!(
            output.read(&mut sent),
            4,
            "queued output is fenced at actual dispatch"
        );
        links
            .process(
                &mut controller,
                false,
                &mut [0.0; 4],
                SourcePosition::default(),
            )
            .unwrap();
        assert!(!links.peers[0].active);
        assert_eq!(status.last_keyed(), None);
        assert_eq!(current.ended(), retire_call);
    }
}

#[test]
fn typed_public_mixer_preserves_protocol_qualified_public_names() {
    for (protocol, expected) in [
        (ProtocolId::AllStarLink, "123"),
        (ProtocolId::EchoLink, "3000123"),
    ] {
        let signals = Arc::new(PeerSignals::new());
        signals.set_radio_keyed(true);
        let (writer, _) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
        let peer = AudioPeer::new(
            PeerIdentity::parse(protocol, "123").unwrap(),
            Mode::TRANSCEIVE,
            Input { signals },
            writer,
            4,
            0,
        )
        .unwrap();
        let (mut links, _) = LinkAudio::new(vec![peer], 4).unwrap();
        let status = links.status();
        let (mut controller, _) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        links
            .process(
                &mut controller,
                false,
                &mut [0.0; 4],
                SourcePosition::default(),
            )
            .unwrap();
        assert_eq!(status.last_keyed(), Some(expected));
    }
}

#[test]
fn typed_phone_mixer_revokes_contributions_and_bounds_later_retirement_churn() {
    for retire_at in 0..3 {
        let mut peers = Vec::new();
        let mut readers = Vec::new();
        let mut calls = Vec::new();
        for index in 0..2 {
            let source = PhoneSource::new(
                PhoneAccountId::new("provider").unwrap(),
                7,
                PhoneCallId::new([index + 1; 16]).unwrap(),
                9,
                u64::from(index) + 13,
            )
            .unwrap();
            let current = reply(source.clone(), true);
            let (writer, reader) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
            peers.push(
                AudioPeer::for_phone(
                    &source,
                    &current,
                    Mode::TRANSCEIVE,
                    input(current.clone()),
                    writer,
                    4,
                )
                .unwrap(),
            );
            readers.push(reader);
            calls.push(current);
        }
        let (mut links, mut dispatcher) = LinkAudio::new(peers, 4).unwrap();
        let (mut controller, _) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        links
            .process(
                &mut controller,
                false,
                &mut [0.0; 4],
                SourcePosition::default(),
            )
            .unwrap();
        if retire_at == 0 {
            calls[0].retire();
        } else if retire_at == 1 {
            let first = dispatcher.dispatch_batch(1, |_| {});
            assert_eq!(
                (first.destinations, first.completed_blocks, first.pending),
                (1, 0, true)
            );
            // A contributing source retires after the first destination.
            // Cancel the remainder without repeatedly scanning contributors.
            calls[0].retire();
        } else {
            let retiring = calls[1].clone();
            super::BEFORE_PUBLISH.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || retiring.retire()));
            });
        }
        let cancelled = dispatcher.dispatch_batch(64, |_| {});
        assert_eq!(cancelled.completed_blocks, 1);
        assert!(cancelled.destinations <= 1);
        if retire_at == 2 {
            assert_eq!(
                cancelled.destinations, 0,
                "retirement during composition cancels publication"
            );
        }
        let mut received = [0.0; 4];
        let destination = usize::from(retire_at != 2);
        assert_eq!(
            readers[destination].read(&mut received),
            4,
            "no retired-source contribution reaches a live destination"
        );
        assert_eq!(received, [0.0; 4]);
    }
}

#[test]
fn typed_phone_mixer_ignores_other_node_retirement_between_destinations() {
    let domain = super::RetirementDomain::default();
    let other = super::RetirementDomain::default();
    let unrelated = PeerSignals::new().reply_target();
    unrelated.bind_retirement_domain(&other);
    let mut peers = Vec::new();
    let mut readers = Vec::new();
    for index in 0..2 {
        let source = PhoneSource::new(
            PhoneAccountId::new("provider").unwrap(),
            7,
            PhoneCallId::new([index + 1; 16]).unwrap(),
            9,
            u64::from(index) + 13,
        )
        .unwrap();
        let current = reply(source.clone(), true);
        let (writer, reader) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
        peers.push(
            AudioPeer::for_phone(
                &source,
                &current,
                Mode::TRANSCEIVE,
                input(current.clone()),
                writer,
                4,
            )
            .unwrap(),
        );
        readers.push(reader);
    }
    let (mut links, mut dispatcher) = LinkAudio::new_in_domain(peers, 4, domain.clone()).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    links
        .process(
            &mut controller,
            false,
            &mut [0.0; 4],
            SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(dispatcher.dispatch_batch(1, |_| {}).destinations, 1);
    unrelated.retire();
    assert_eq!(domain.revision(), Some(0));
    assert_eq!(other.revision(), Some(1));
    assert_eq!(dispatcher.dispatch_batch(1, |_| {}).completed_blocks, 1);
    let mut received = [0.0; 4];
    assert_eq!(readers[1].read(&mut received), 0);
    assert_eq!(received, [0.25; 4]);
}
