use super::*;
use crate::link::{PeerIdentity, PeerReply, PhoneAccountId, PhoneCallId, PhoneSource, ProtocolId};

struct PhoneInput {
    signals: PeerSignals,
    reply: PeerReply,
    policy: ReceivePolicy,
    age: Option<SourceTimeBound>,
    public: bool,
    quiet: bool,
}
impl PeerInput for PhoneInput {
    fn phone_reply(&self) -> Option<&PeerReply> {
        (!self.public).then_some(&self.reply)
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
    fn render_timed_block(&mut self, output: &mut [f32]) -> Option<RenderedPeerBlock> {
        self.render(output).map(|real| RenderedPeerBlock {
            real,
            age_bound: self.age,
        })
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        if self.quiet {
            output.fill(0.0);
            return None;
        }
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
        age: None,
        public: false,
        quiet: false,
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
    for retire_at in 0..4 {
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
            let hook = if retire_at == 2 {
                &super::BEFORE_PUBLISH
            } else {
                &super::AFTER_VALIDATION
            };
            hook.with(|slot| {
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
        let destination = usize::from(retire_at < 2);
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

#[test]
fn phone_fanout_keeps_original_deadline_excludes_self_and_never_advances_it_on_partial_read() {
    for second_epoch in [Some(7), Some(8), None] {
        let mut peers = Vec::new();
        let mut readers = Vec::new();
        for index in 0..3 {
            let source = source(7, 9 + index, 13);
            let reply = reply(source.clone(), true);
            let mut input = input(reply.clone());
            input.age = if index == 0 {
                Some(SourceTimeBound {
                    host_clock_epoch: 7,
                    not_after_ns: 100_000_000,
                })
            } else {
                second_epoch.map(|host_clock_epoch| SourceTimeBound {
                    host_clock_epoch,
                    not_after_ns: 200_000_000,
                })
            };
            input.public = index == 2;
            let (writer, reader) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
            readers.push(reader);
            peers.push(if index == 2 {
                AudioPeer::new(
                    PeerIdentity::from_operator("2000").unwrap(),
                    Mode::TRANSCEIVE,
                    input,
                    writer,
                    4,
                    0,
                )
                .unwrap()
            } else {
                AudioPeer::for_phone(&source, &reply, Mode::TRANSCEIVE, input, writer, 4).unwrap()
            });
        }
        let domain = RetirementDomain::default();
        let (links, mut dispatcher) = LinkAudio::new_in_domain(peers, 4, domain.clone()).unwrap();
        let mut links = links.with_phone_clock(7);
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        controller.bind_phone_retirement(&mut control, domain);
        links
            .process(
                &mut controller,
                false,
                &mut [0.0; 4],
                SourcePosition {
                    generation: 1,
                    sample: 0,
                    stream: 1,
                    born_ns: Some(400_000_000),
                },
            )
            .unwrap();
        dispatcher.dispatch(3);
        let second_cap = if second_epoch == Some(7) {
            700_000_000
        } else {
            0
        };
        for (reader, expected) in
            readers
                .iter_mut()
                .zip([second_cap, 600_000_000, second_cap.min(600_000_000)])
        {
            let first = reader.read_span(&mut [0.0; 2]).unwrap();
            let tail = reader.read_span(&mut [0.0; 2]).unwrap();
            assert_eq!(first.span.phone_expires_ns, expected);
            assert_eq!(tail.span.phone_expires_ns, expected);
            assert_eq!(
                first.span.born_ns,
                Some(400_000_000),
                "native egress allowance is unchanged"
            );
            assert!(tail.span.born_ns > first.span.born_ns);
        }
    }
}

#[test]
fn outgoing_phone_role_uses_existing_exact_mixer_fence_without_control_authority() {
    let source = source(7, 9, 13);
    let outgoing = PeerSignals::for_outgoing_phone(source.clone());
    let current = outgoing.reply_target();
    let (writer, mut output) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
    let peer = AudioPeer::for_phone(
        &source,
        &current,
        Mode::TRANSCEIVE,
        input(current.clone()),
        writer,
        4,
    )
    .expect("outgoing media-only role must use the existing typed phone slot");
    assert!(!current.phone_authenticated());
    assert!(!peer.ended());
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
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
    dispatcher.dispatch(1);
    let mut sent = [0.0; 4];
    assert_eq!(
        output.read(&mut sent),
        0,
        "outgoing media must reach the real dispatcher"
    );
    assert_eq!(
        sent, [0.5; 4],
        "return is local radio without phone self-echo"
    );
    links
        .process(
            &mut controller,
            true,
            &mut [0.5; 4],
            SourcePosition::default(),
        )
        .unwrap();
    outgoing.end();
    dispatcher.dispatch(1);
    assert_eq!(
        output.read(&mut sent),
        4,
        "retirement fences queued return media"
    );
    assert!(links.peers[0].ended());
    assert!(!current.phone_authenticated());
}

#[test]
fn outgoing_idle_pcm_uses_existing_route_without_rf_telemetry_or_retired_recipient() {
    use crate::controller::{MorseSettings, PlaybackDestination, PlaybackPolicy, PreparedMedia};
    for (is_outgoing, mode, idle_expected, retire_source) in [
        (true, Mode::TRANSCEIVE, true, false),
        (true, Mode::TRANSCEIVE, true, true),
        (true, Mode::MONITOR, false, false),
        (false, Mode::TRANSCEIVE, false, false),
    ] {
        let source = source(7, 9, 13);
        let owner = if is_outgoing {
            PeerSignals::for_outgoing_phone(source.clone())
        } else {
            let owner = PeerSignals::for_phone(source.clone());
            assert!(owner.authenticate_phone());
            owner
        };
        let current = owner.reply_target();
        let mut receive = input(current.clone());
        receive.quiet = true;
        receive.signals.set_radio_keyed(false);
        let (writer, mut output) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
        let peer = AudioPeer::for_phone(&source, &current, mode, receive, writer, 4).unwrap();
        let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        // A callback gap keeps its source coordinate; no catch-up PCM is invented.
        for tick in [0, 1, 7] {
            let stamp = SourcePosition {
                generation: 7,
                sample: tick * 4,
                stream: 9,
                born_ns: Some(1_000_000 + tick * 83_333),
            };
            let mut rf = [0.0; 4];
            assert!(
                !links
                    .process(&mut controller, false, &mut rf, stamp)
                    .unwrap(),
                "idle transport must not key RF"
            );
            assert_eq!(rf, [0.0; 4]);
            assert!(!links.peer_source_active());
            assert!(!links.routing[0].telemetry);
            dispatcher.dispatch(1);
            let mut sent = [1.0; 4];
            let read = output.read_span(&mut sent);
            assert_eq!(
                read.is_some(),
                idle_expected,
                "outgoing silence must use the actual dispatcher"
            );
            if let Some(read) = read {
                assert_eq!(sent, [0.0; 4]);
                assert_eq!(
                    (
                        read.frames,
                        read.span.source_start,
                        read.span.stream,
                        read.span.born_ns
                    ),
                    (4, stamp.sample, stamp.stream, stamp.born_ns)
                );
            }
        }
        // LOCAL-and-links telemetry remains excluded from every Phone destination.
        controller.process_audio(false, false, &[], &mut [0.0; 12_000]);
        assert!(
            control.queue_prepared_status(
                PreparedMedia::new(Some(vec![0.25; 4]), "E", MorseSettings::default())
                    .unwrap()
                    .with_playback_policy(PlaybackPolicy {
                        destination: PlaybackDestination::LocalAndLinks,
                        ..Default::default()
                    })
            )
        );
        let mut rf = [0.0; 4];
        links
            .process(&mut controller, false, &mut rf, SourcePosition::default())
            .unwrap();
        assert_eq!(rf, [0.25; 4]);
        dispatcher.dispatch(1);
        let mut sent = [1.0; 4];
        assert_eq!(output.read_span(&mut sent).is_some(), idle_expected);
        if idle_expected {
            assert_eq!(
                sent, [0.0; 4],
                "continuous idle route must not leak public telemetry"
            );
        }
        links
            .process(
                &mut controller,
                true,
                &mut [0.5; 4],
                SourcePosition::default(),
            )
            .unwrap();
        dispatcher.dispatch(1);
        let read = output.read_span(&mut sent);
        assert_eq!(read.is_some(), mode.transmits());
        if mode.transmits() {
            assert_eq!(sent, [0.5; 4]);
        }
        links
            .process(
                &mut controller,
                false,
                &mut [0.0; 4],
                SourcePosition::default(),
            )
            .unwrap();
        if retire_source {
            links.peers[0].input.signals.end();
            assert!(!current.ended(), "source cut does not end the call");
        } else {
            owner.end();
        }
        dispatcher.dispatch(1);
        assert!(
            output.read_span(&mut sent).is_none(),
            "exact recipient or source retirement cancels queued silence"
        );
    }
}

#[test]
fn outgoing_isolation_suppresses_only_ordinary_media_in_both_directions() {
    for (phone_mode, forwards) in [(Mode::TRANSCEIVE, true), (Mode::LOCAL_TRANSCEIVE, false)] {
        for isolated in [false, true] {
            for (ordinary, callee, local) in [
                (true, false, false),
                (true, false, true),
                (false, true, false),
            ] {
                let source = source(7, 9, 13);
                let owner = PeerSignals::for_outgoing_phone(source.clone());
                let reply = owner.reply_target();
                let mut peers = Vec::new();
                let mut outputs = Vec::new();
                for phone in [false, true] {
                    let mut input = input(reply.clone());
                    input.public = !phone;
                    input.policy = if phone {
                        ReceivePolicy::Phone
                    } else {
                        ReceivePolicy::Peer
                    };
                    input.quiet = !(if phone { callee } else { ordinary });
                    input.signals.set_radio_keyed(!input.quiet);
                    let (writer, reader) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
                    outputs.push(reader);
                    peers.push(if phone {
                        AudioPeer::for_phone(&source, &reply, phone_mode, input, writer, 4).unwrap()
                    } else {
                        AudioPeer::new(
                            PeerIdentity::from_operator("2000").unwrap(),
                            Mode::TRANSCEIVE,
                            input,
                            writer,
                            4,
                            0,
                        )
                        .unwrap()
                    });
                }
                let (links, mut dispatcher) = LinkAudio::new(peers, 4).unwrap();
                let mut links = links.with_peer_isolation(isolated);
                let (mut controller, _) = NodeController::new(
                    ControllerSettings::default(),
                    vec![],
                    vec![],
                    CourtesySettings::default(),
                )
                .unwrap();
                let mut rf = [if local { 0.5 } else { 0.0 }; 4];
                let keyed = links
                    .process(&mut controller, local, &mut rf, SourcePosition::default())
                    .unwrap();
                if ordinary && !callee && !local && isolated {
                    assert!(!keyed, "isolated peer must not key RF");
                    assert_eq!(rf, [0.0; 4], "ordinary ingress cannot reach RF");
                }
                if callee {
                    assert!(keyed);
                    assert_eq!(rf, [0.25; 4], "callee retains local RF playout");
                }
                dispatcher.dispatch(1);
                let mut sent = [9.0; 4];
                let ordinary_read = outputs[0].read_span(&mut sent);
                if isolated {
                    assert!(ordinary_read.is_none(), "ordinary egress is isolated");
                } else if local || (callee && forwards) {
                    assert!(ordinary_read.is_some(), "normal mix-minus is unchanged");
                    assert_eq!(sent, [if local { 0.5 } else { 0.25 }; 4]);
                } else if callee {
                    assert!(
                        ordinary_read.is_none(),
                        "local-only telephone playout cannot reach ordinary peers"
                    );
                }
                assert!(
                    outputs[1].read_span(&mut sent).is_some(),
                    "outgoing call keeps its clocked route"
                );
                assert_eq!(
                    sent,
                    [(if local { 0.5 } else { 0.0 })
                        + (if ordinary && !isolated { 0.25 } else { 0.0 }); 4],
                    "phone hears local input but no isolated peer or self echo"
                );
                assert!(
                    !links.peers[0].input.signals.ended(),
                    "control/transport stays live"
                );
                assert_eq!(
                    links.peers[0].input.signals.radio_keyed(),
                    ordinary,
                    "media policy does not falsify the transport control signal"
                );
                assert!(!reply.ended());
            }
        }
    }
}
