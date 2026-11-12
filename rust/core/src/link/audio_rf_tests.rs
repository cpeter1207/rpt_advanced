//! Selected RF programme metadata is separate from capture and physical presentation.
use super::*;
use crate::controller::ControllerControl;
use crate::link::{PhoneAccountId, PhoneCallId, PhoneSource};

struct RfInput {
    signals: PeerSignals,
    call: Option<PeerReply>,
    age: Option<SourceTimeBound>,
    finite: Option<bool>,
    available: u64,
    real: Option<usize>,
    value: f32,
    finish_on_render: bool,
    retire_on_render: Option<PeerReply>,
    renders: usize,
    silent_samples: usize,
}
impl PeerInput for RfInput {
    fn phone_reply(&self) -> Option<&PeerReply> {
        self.call.as_ref()
    }
    fn source_rate(&self) -> u32 {
        48_000
    }
    fn receive_policy(&self) -> ReceivePolicy {
        if self.call.is_some() {
            ReceivePolicy::Phone
        } else {
            ReceivePolicy::Peer
        }
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        self.available
    }
    fn finite_playout(&self) -> Option<bool> {
        self.finite
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        self.renders += 1;
        output.fill(self.value);
        if self.finish_on_render {
            self.finite = Some(false);
        }
        if let Some(reply) = &self.retire_on_render {
            reply.retire();
        }
        self.real.map(|real| real.min(output.len()))
    }
    fn render_timed_block(&mut self, output: &mut [f32]) -> Option<RenderedPeerBlock> {
        self.render(output).map(|real| RenderedPeerBlock {
            real,
            age_bound: self.age,
        })
    }
    fn silence(&mut self, scratch: &mut [f32]) {
        self.silent_samples += scratch.len();
        scratch.fill(9.0); // Discarded processor history is not RF contribution.
    }
}
fn age(epoch: u64, received: u64) -> SourceTimeBound {
    SourceTimeBound {
        host_clock_epoch: epoch,
        not_after_ns: received,
    }
}
fn peer(index: u8, private: bool) -> (AudioPeer<RfInput>, LinkAudioConsumer) {
    let source = PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        7,
        PhoneCallId::new([index + 1; 16]).unwrap(),
        9,
        u64::from(index) + 13,
    )
    .unwrap();
    let call = private.then(|| {
        let signals = PeerSignals::for_phone(source.clone());
        assert!(signals.authenticate_phone());
        signals.reply_target()
    });
    let signals = PeerSignals::new();
    signals.publish_pcm();
    let input = RfInput {
        signals,
        call: call.clone(),
        age: Some(age(7, 30)),
        finite: Some(true),
        available: 16_000,
        real: Some(4),
        value: 0.0,
        finish_on_render: false,
        retire_on_render: None,
        renders: 0,
        silent_samples: 0,
    };
    let (writer, reader) = LinkAudioQueue::new(32, 8).unwrap().into_endpoints();
    let prepared = if let Some(call) = call {
        AudioPeer::for_phone(&source, &call, Mode::TRANSCEIVE, input, writer, 4).unwrap()
    } else {
        AudioPeer::new(
            PeerIdentity::from_operator(&format!("20{index}")).unwrap(),
            Mode::TRANSCEIVE,
            input,
            writer,
            4,
            0,
        )
        .unwrap()
    };
    (prepared, reader)
}
fn fixture(
    peers: Vec<AudioPeer<RfInput>>,
) -> (
    LinkAudio<RfInput>,
    LinkDispatcher,
    NodeController,
    ControllerControl,
    RetirementDomain,
) {
    let domain = RetirementDomain::default();
    let (links, dispatcher) = LinkAudio::new_in_domain(peers, 4, domain.clone()).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.bind_phone_retirement(&mut control, domain.clone());
    (links, dispatcher, node, control, domain)
}
fn tick(links: &mut LinkAudio<RfInput>, node: &mut NodeController) {
    links
        .process(node, false, &mut [0.0; 4], SourcePosition::default())
        .unwrap();
}
fn assert_programme(
    links: &LinkAudio<RfInput>,
    active: bool,
    private: bool,
    bound: Option<SourceTimeBound>,
) {
    let programme = links.rf_programme();
    assert_eq!(programme.active, active);
    assert_eq!(programme.private, private);
    assert_eq!(programme.age_bound, bound);
}
fn contributors(links: &LinkAudio<RfInput>) -> Vec<(usize, usize)> {
    links
        .rf_contributors()
        .map(|source| (source.media.identity(), source.call.identity()))
        .collect()
}

#[test]
fn rf_programme_requires_selected_rendering_not_key_registration_or_amplitude() {
    // The same real mixer handles zero PCM, key-only, rejected rendering, idle
    // graph advancement, PLC, and the last block of an explicitly finite source.
    for case in 0..7 {
        let (mut prepared, _reader) = peer(0, true);
        match case {
            1 => {
                prepared.input.finite = None;
                prepared.input.available = 0;
                prepared.input.signals.set_radio_keyed(true);
            }
            2 => prepared.input.real = None,
            3 => prepared.input.finite = Some(false),
            4 => {
                prepared.input.finite = None;
                prepared.input.real = Some(0);
                prepared.input.age = None;
            }
            5 => prepared.input.finish_on_render = true,
            6 => prepared.input.signals.end(),
            _ => {}
        }
        let (mut links, _dispatcher, mut node, _control, domain) = fixture(vec![prepared]);
        let original_revision = domain.revision();
        tick(&mut links, &mut node);
        let contributes = matches!(case, 0 | 4 | 5);
        assert_programme(
            &links,
            contributes,
            contributes,
            (contributes && case != 4).then_some(age(7, 30)),
        );
        assert_eq!(
            contributors(&links).len(),
            usize::from(contributes),
            "case {case}"
        );
        assert_eq!(links.rf_programme().retirement_revision, original_revision);
        assert_eq!(links.mix, [0.0; 4], "zero PCM is not missing provenance");
        assert_eq!(
            links.peers[0].input.renders,
            usize::from(matches!(case, 0 | 2 | 4 | 5)),
            "one source render, case {case}"
        );
        if case == 5 {
            tick(&mut links, &mut node);
            assert_programme(&links, false, false, None);
            assert!(contributors(&links).is_empty());
            assert_eq!(links.peers[0].input.renders, 1);
        }
    }
}

#[test]
fn rf_programme_folds_oldest_original_age_with_unknown_sticky_in_either_order() {
    for second_age in [Some(age(7, 10)), None, Some(age(8, 10)), Some(age(0, 10))] {
        for reverse in [false, true] {
            let (first, _first_reader) = peer(0, true);
            let (mut second, _second_reader) = peer(1, true);
            second.input.age = second_age;
            let mut peers = vec![first, second];
            if reverse {
                peers.reverse();
            }
            let (mut links, _dispatcher, mut node, _control, _domain) = fixture(peers);
            tick(&mut links, &mut node);
            let expected = second_age.filter(|bound| bound.host_clock_epoch == 7);
            assert_programme(&links, true, true, expected);
            assert_eq!(contributors(&links).len(), 2);
            for peer in &links.peers {
                assert_eq!(peer.input.renders, 1);
            }
        }
    }
}

#[test]
fn rf_programme_ordinary_unknown_cannot_be_relabelled_as_private_bound() {
    for reverse in [false, true] {
        let (phone, _phone_reader) = peer(0, true);
        let (public, _public_reader) = peer(1, false);
        let mut peers = vec![phone, public];
        if reverse {
            peers.reverse();
        }
        let (mut links, _dispatcher, mut node, _control, _domain) = fixture(peers);
        tick(&mut links, &mut node);
        assert_programme(&links, true, true, None);
        assert_eq!(contributors(&links).len(), 1);
        assert_eq!(
            links
                .peers
                .iter()
                .map(|peer| peer.input.renders)
                .sum::<usize>(),
            2
        );
    }
    let (public, _reader) = peer(0, false);
    let (mut links, _dispatcher, mut node, _control, _domain) = fixture(vec![public]);
    tick(&mut links, &mut node);
    assert_programme(&links, true, false, None);
    assert!(contributors(&links).is_empty());
}

#[test]
fn rf_programme_selection_preserves_idle_adoption_and_independent_capture() {
    let selection = GroupSelection::new();
    let first_slot = std::num::NonZeroUsize::new(1).unwrap();
    let second_slot = std::num::NonZeroUsize::new(2).unwrap();
    selection.publish_desired(Some(first_slot));
    let (mut first, _first_reader) = peer(0, true);
    let (mut second, _second_reader) = peer(1, true);
    first.input.finite = Some(false);
    second.input.finite = Some(false);
    second.input.age = None;
    let (mut links, _dispatcher, mut node, mut control, domain) = fixture(vec![
        first.with_group(selection.clone(), first_slot),
        second.with_group(selection.clone(), second_slot),
    ]);
    tick(&mut links, &mut node); // Only the existing all-inputs-idle turn adopts slot 1.
    assert_eq!(selection.active(), Some(first_slot));
    assert_programme(&links, false, false, None);
    for peer in &mut links.peers {
        peer.input.finite = Some(true);
    }
    selection.publish_desired(Some(second_slot));
    tick(&mut links, &mut node);
    assert_eq!(selection.active(), Some(first_slot));
    assert_programme(&links, true, true, Some(age(7, 30)));
    let first_call = links.peers[0].input.call.as_ref().unwrap().identity();
    assert_eq!(
        contributors(&links),
        vec![(links.peers[0].input.signals.reply_identity(), first_call)]
    );
    for peer in &mut links.peers {
        assert_eq!(peer.input.renders, 1);
        peer.input.finite = Some(false);
    }
    links
        .process(&mut node, true, &mut [0.0; 4], SourcePosition::default())
        .unwrap();
    assert_eq!(
        selection.active(),
        Some(first_slot),
        "local RF activity also defers adoption"
    );
    tick(&mut links, &mut node);
    assert_eq!(selection.active(), Some(second_slot));
    assert_programme(&links, false, false, None);
    let capture = control.take_parrot_capture().unwrap();
    let captured_phone = capture.phone_source().unwrap();
    assert_eq!(
        captured_phone.age_bound, None,
        "the unselected peer still taints capture"
    );
    assert_eq!(captured_phone.retirement_revision, domain.revision());
    for peer in &mut links.peers {
        peer.input.finite = Some(true);
    }
    tick(&mut links, &mut node);
    assert_programme(&links, true, true, None);
    assert_eq!(
        contributors(&links)[0].1,
        links.peers[1].input.call.as_ref().unwrap().identity()
    );
}

#[test]
fn rf_programme_retirement_during_render_does_not_report_an_unmixed_source() {
    for private_survivor in [false, true] {
        for retire_call in [false, true] {
            let (first, _first_reader) = peer(0, true);
            let retired = if retire_call {
                first.input.call.as_ref().unwrap().clone()
            } else {
                first.input.signals.reply_target()
            };
            let (mut second, _second_reader) = peer(1, private_survivor);
            second.input.age = Some(age(7, 10));
            let (mut links, _dispatcher, mut node, _control, domain) = fixture(vec![first, second]);
            tick(&mut links, &mut node);
            assert_programme(&links, true, true, private_survivor.then_some(age(7, 10)));
            links.peers[1].input.retire_on_render = Some(retired);
            let original_revision = domain.revision();
            tick(&mut links, &mut node);
            assert_ne!(domain.revision(), original_revision);
            assert_eq!(
                links.rf_programme().retirement_revision,
                original_revision,
                "never renew the snapshot after a race"
            );
            assert_programme(&links, true, private_survivor, None);
            let remaining = contributors(&links);
            assert_eq!(remaining.len(), usize::from(private_survivor));
            if let Some((media, call)) = remaining.first() {
                let survivor = &links.peers[1].input;
                assert_eq!(*media, survivor.signals.reply_identity());
                assert_eq!(*call, survivor.call.as_ref().unwrap().identity());
            }
        }
    }
}

#[test]
fn rf_programme_borrows_exact_fences_and_retains_the_completed_block_snapshot() {
    let (prepared, _reader) = peer(0, true);
    let media = prepared.input.signals.reply_target();
    let call = prepared.input.call.as_ref().unwrap().clone();
    let (mut links, _dispatcher, mut node, _control, domain) = fixture(vec![prepared]);
    tick(&mut links, &mut node);
    let original = links.rf_programme();
    let source = links.rf_contributors().next().unwrap();
    assert_eq!(source.media, &media);
    assert_eq!(source.call, &call);
    media.retire();
    assert!(source.media.ended());
    assert!(!source.call.ended());
    assert_ne!(domain.revision(), original.retirement_revision);
    assert_eq!(
        links.rf_programme(),
        original,
        "a getter does not restamp rendered history"
    );
    assert_eq!(
        contributors(&links),
        vec![(media.identity(), call.identity())]
    );
    tick(&mut links, &mut node);
    assert_programme(&links, false, false, None);
    assert!(contributors(&links).is_empty());
}

#[test]
fn rf_programme_empty_or_rejected_callback_cannot_reuse_prior_contributors() {
    for frames in [0, 5] {
        let (prepared, _reader) = peer(0, true);
        let (mut links, _dispatcher, mut node, _control, _domain) = fixture(vec![prepared]);
        tick(&mut links, &mut node);
        assert_programme(&links, true, true, Some(age(7, 30)));
        let mut output = [0.0; 5];
        let result = links.process(
            &mut node,
            false,
            &mut output[..frames],
            SourcePosition::default(),
        );
        assert_eq!(result.is_err(), frames == 5);
        assert_programme(&links, false, false, None);
        assert!(contributors(&links).is_empty());
    }
}

#[test]
fn rf_programme_metadata_does_not_delay_network_mix_minus_or_replace_egress_clock() {
    let (mut first, mut first_reader) = peer(0, true);
    let (mut second, mut second_reader) = peer(1, true);
    first.input.value = 0.25;
    second.input.value = 0.5;
    let (mut links, mut dispatcher, mut node, _control, _domain) = fixture(vec![first, second]);
    let source = SourcePosition {
        generation: 11,
        sample: 48,
        stream: 23,
        born_ns: Some(900_000),
    };
    links
        .process(&mut node, true, &mut [0.125; 4], source)
        .unwrap();
    assert_programme(&links, true, true, Some(age(7, 30)));
    assert_eq!(links.mix, [0.75; 4]);
    assert_eq!(dispatcher.dispatch(1), 1);
    for (reader, expected) in [(&mut first_reader, 0.625), (&mut second_reader, 0.375)] {
        let mut output = [0.0; 4];
        let read = reader.read_span(&mut output).unwrap();
        assert_eq!(read.frames, 4);
        assert_eq!(output, [expected; 4]);
        assert_eq!(read.span.generation, source.generation);
        assert_eq!(read.span.source_start, source.sample);
        assert_eq!(read.span.stream, source.stream);
        assert_eq!(read.span.born_ns, source.born_ns);
    }
}
