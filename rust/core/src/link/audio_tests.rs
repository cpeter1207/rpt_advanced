use super::*;
use crate::audio::{LinkAudioConsumer, LinkAudioQueue};
use crate::controller::{ControllerSettings, CourtesySettings};

struct FiniteInput {
    signals: PeerSignals,
    samples: std::collections::VecDeque<f32>,
}
impl PeerInput for FiniteInput {
    fn source_rate(&self) -> u32 {
        48000
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        self.samples.len() as u64
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        let count = output.len().min(self.samples.len());
        for sample in &mut output[..count] {
            *sample = self.samples.pop_front().unwrap();
        }
        output[count..].fill(0.0);
        Some(count)
    }
}

#[test]
fn regeneration_replaces_rf_not_peer_program_or_parrot_capture() {
    let input = |samples: &[f32]| FiniteInput {
        signals: PeerSignals::new(),
        samples: samples.iter().copied().collect(),
    };
    let (writer, mut reader) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new("200", Mode::TRANSCEIVE, input(&[]), writer, 4, 0).unwrap();
    let (audio, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
    let mut audio = audio.with_regeneration(input(&[0.25, 0.0]));
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut rf = [0.5; 4];
    audio
        .process(&mut node, true, &mut rf, SourcePosition::default())
        .unwrap();
    assert_eq!(rf, [0.25, 0.0, 0.5, 0.5]);
    dispatcher.dispatch(1);
    let mut remote = [0.0; 4];
    assert_eq!(reader.read(&mut remote), 0);
    assert_eq!(remote, [0.5; 4]);
    audio
        .process(&mut node, false, &mut [0.0], SourcePosition::default())
        .unwrap();
    assert_eq!(control.take_parrot_capture().unwrap().samples(), &[0.5; 4]);
    assert!(
        audio
            .process(&mut node, false, &mut [0.0; 5], SourcePosition::default())
            .is_err()
    );
}

#[test]
fn egress_dispatch_notifies_receive_only_activity_without_echoing_pcm() {
    let (mut peer, signals, mut output) = peer_with_kerchunk("200", 960, 0);
    peer.mode = Mode::MONITOR;
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    signals.set_radio_keyed(true);
    let mut notified = Vec::new();
    links
        .process(
            &mut controller,
            false,
            &mut [0.0; 960],
            SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!(
        notified,
        [0],
        "inbound activity must wake its idle serialized owner"
    );
    assert!(
        output.read_span(&mut [0.0; 960]).is_none(),
        "an activity wake must not echo input"
    );
    links
        .process(
            &mut controller,
            false,
            &mut [0.0; 960],
            SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!(
        notified,
        [0],
        "unchanged activity must not poll an idle owner"
    );
}

#[test]
fn egress_batched_mix_minus_preserves_routes() {
    use crate::audio::AudioSpan;
    let (mut full, _, _full_output) = peer_with_kerchunk("200", 4, 0);
    let (healthy, _, mut output) = peer_with_kerchunk("300", 4, 0);
    full.outbound
        .as_mut()
        .unwrap()
        .write(AudioSpan::default(), &[0.9; 4]);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![full, healthy], 4).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let source = SourcePosition {
        generation: 8,
        sample: 500,
        ..SourcePosition::default()
    };
    links
        .process(&mut controller, true, &mut [0.25; 2], source)
        .unwrap();
    assert_eq!(dispatcher.dispatch_batch(0, |_| {}).destinations, 0);
    let mut notified = Vec::new();
    let first = dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!(
        (first.destinations, first.completed_blocks, first.pending),
        (1, 0, true)
    );
    assert!(
        notified.is_empty(),
        "a full peer does not claim accepted audio"
    );
    links
        .process(
            &mut controller,
            true,
            &mut [0.5; 2],
            SourcePosition {
                sample: 502,
                ..source
            },
        )
        .unwrap();
    links
        .process(
            &mut controller,
            true,
            &mut [0.75; 2],
            SourcePosition {
                sample: 504,
                ..source
            },
        )
        .unwrap();
    assert_eq!(
        links.status().dropped_blocks(),
        1,
        "partial dispatch cannot recycle its block early"
    );
    let second = dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!((second.destinations, second.completed_blocks), (1, 1));
    assert_eq!(notified, [1]);
    let mut received = [0.0; 4];
    let span = output.read_span(&mut received).unwrap();
    assert_eq!(
        (span.frames, span.span.generation, span.span.source_start),
        (2, 8, 500)
    );
    assert_eq!(&received[..2], &[0.25; 2]);
    assert_eq!(dispatcher.dispatch_batch(2, |_| {}).completed_blocks, 1);
    assert_eq!(
        output.read_span(&mut received).unwrap().span.source_start,
        502
    );
    assert_eq!(&received[..2], &[0.5; 2]);
    assert!(!dispatcher.dispatch_batch(1, |_| {}).pending);
    links
        .process(
            &mut controller,
            true,
            &mut [0.6; 2],
            SourcePosition {
                sample: 506,
                ..source
            },
        )
        .unwrap();
    dispatcher.dispatch_batch(2, |_| {});
    let after_drop = output.read_span(&mut received).unwrap();
    assert_eq!(
        (after_drop.span.source_start, after_drop.span.burst),
        (506, 1)
    );
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            SourcePosition {
                sample: 508,
                ..source
            },
        )
        .unwrap();
    assert_eq!(
        dispatcher.dispatch_batch(1, |_| {}).destinations,
        1,
        "disabled peers consume the batch budget too"
    );
    dispatcher.dispatch_batch(1, |_| {});
    assert!(output.read_span(&mut received).is_none());
    links
        .process(
            &mut controller,
            true,
            &mut [0.7],
            SourcePosition {
                generation: 9,
                sample: 509,
                ..source
            },
        )
        .unwrap();
    dispatcher.dispatch_batch(2, |_| {});
    let next_burst = output.read_span(&mut received).unwrap();
    assert_eq!(
        (
            next_burst.span.generation,
            next_burst.span.source_start,
            next_burst.span.burst
        ),
        (9, 509, 2)
    );
}

struct Input {
    signals: Arc<PeerSignals>,
}

impl PeerInput for Input {
    fn source_rate(&self) -> u32 {
        48_000
    }

    fn signals(&self) -> &PeerSignals {
        &self.signals
    }

    fn available(&self) -> u64 {
        0
    }

    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        output.fill(0.0);
        None
    }
}

fn peer(name: &str, maximum: usize) -> (AudioPeer<Input>, Arc<PeerSignals>) {
    let (peer, signals, _) = peer_with_kerchunk(name, maximum, 0);
    (peer, signals)
}

fn peer_with_kerchunk(
    name: &str,
    maximum: usize,
    kerchunk_ms: u32,
) -> (AudioPeer<Input>, Arc<PeerSignals>, LinkAudioConsumer) {
    let (outbound, inbound) = LinkAudioQueue::new(maximum.max(1), 64)
        .unwrap()
        .into_endpoints();
    let signals = Arc::new(PeerSignals::new());
    (
        AudioPeer::new(
            name,
            Mode::TRANSCEIVE,
            Input {
                signals: Arc::clone(&signals),
            },
            outbound,
            maximum,
            kerchunk_ms,
        )
        .unwrap(),
        signals,
        inbound,
    )
}

#[test]
fn peer_replies_wait_for_origin_and_route_each_sample_without_local_ptt() {
    use crate::controller::{MorseSettings, PreparedMedia};
    let (mut first, first_signal, mut first_output) = peer_with_kerchunk("200", 8, 0);
    first.mode = Mode::MONITOR;
    let (second, second_signal, mut second_output) = peer_with_kerchunk("300", 8, 0);
    let selection = GroupSelection::new();
    let second = second.with_group(selection, std::num::NonZeroUsize::new(1).unwrap());
    let (mut links, mut dispatcher) = LinkAudio::new(vec![first, second], 8).unwrap();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for (signal, value) in [(&first_signal, 0.2), (&second_signal, 0.4)] {
        let media = PreparedMedia::new(Some(vec![value; 2]), "", MorseSettings::default())
            .unwrap()
            .reply_to(Some(signal.reply_target()));
        assert!(control.queue_prepared_status(media));
    }
    first_signal.set_radio_keyed(true);
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    assert_eq!(first_output.read(&mut [0.0]), 1);
    assert_eq!(second_output.read(&mut [0.0]), 1);
    first_signal.set_radio_keyed(false);
    let mut radio = [0.0; 8];
    assert!(
        !links
            .process(
                &mut controller,
                false,
                &mut radio,
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    dispatcher.dispatch(1);
    assert_eq!(radio, [0.0; 8]);
    let mut first_pcm = [0.0; 8];
    let mut second_pcm = [0.0; 8];
    assert_eq!(first_output.read(&mut first_pcm), 0);
    assert_eq!(second_output.read(&mut second_pcm), 0);
    assert_eq!(first_pcm, [0.2, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(second_pcm, [0.0, 0.0, 0.4, 0.4, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(control.reclaim().count(), 2);
}

#[test]
fn ended_peer_reply_never_reaches_same_number_replacement() {
    use crate::controller::{MorseSettings, PreparedMedia};
    let old = PeerSignals::new();
    let media = PreparedMedia::new(Some(vec![0.4]), "", MorseSettings::default())
        .unwrap()
        .reply_to(Some(old.reply_target()));
    old.end();
    let (replacement, _, mut output) = peer_with_kerchunk("200", 1, 0);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![replacement], 1).unwrap();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(control.queue_prepared_status(media));
    assert!(
        !links
            .process(
                &mut controller,
                false,
                &mut [0.0],
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    dispatcher.dispatch(1);
    assert_eq!(output.read(&mut [0.0]), 1);
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn addressed_morse_reply_does_not_enable_ordinary_audio_for_monitor_or_fallback() {
    use crate::controller::{MorseSettings, PreparedMedia};
    for fallback in [false, true] {
        let (mut peer, signal, mut output) = peer_with_kerchunk("200", 4, 0);
        if fallback {
            peer = peer.with_group(
                GroupSelection::new(),
                std::num::NonZeroUsize::new(1).unwrap(),
            );
        } else {
            peer.mode = Mode::MONITOR;
        }
        let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: false,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        let media = PreparedMedia::new(Some(vec![0.4; 2]), "E", MorseSettings::default())
            .unwrap()
            .reply_to(Some(signal.reply_target()));
        assert!(control.queue_prepared_status(media));
        let mut radio = [0.7; 4];
        assert!(
            !links
                .process(
                    &mut controller,
                    true,
                    &mut radio,
                    crate::link::SourcePosition::default()
                )
                .unwrap()
        );
        dispatcher.dispatch(1);
        let mut sent = [0.0; 4];
        assert_eq!(output.read(&mut sent), 0);
        let mut expected = [0.0; 4];
        let mut morse = crate::audio::MorseRenderer::new("E", 20, 800.0, -6).unwrap();
        assert_eq!(
            crate::audio::AudioSource::render(&mut morse, &mut expected),
            4
        );
        assert_eq!(sent, expected);
        assert_eq!(radio, [0.0; 4]);
        assert_eq!(control.reclaim().count(), 0, "Morse reply is still playing");
    }
}

#[test]
fn preparation_and_empty_graph_reject_invalid_shapes_without_dispatch() {
    for (name, maximum) in [("bad", 1), ("200", 0)] {
        let (outbound, _) = LinkAudioQueue::new(1, 64).unwrap().into_endpoints();
        assert!(
            AudioPeer::new(
                name,
                Mode::TRANSCEIVE,
                Input {
                    signals: Arc::new(PeerSignals::new()),
                },
                outbound,
                maximum,
                0,
            )
            .is_err()
        );
    }

    assert!(LinkAudio::<Input>::new(vec![], 0).is_err());
    let (undersized, _) = peer("200", 1);
    assert!(LinkAudio::new(vec![undersized], 2).is_err());

    let (mut empty, mut dispatcher) = LinkAudio::<Input>::new(vec![], 1).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let status = empty.status();
    assert_eq!(status.last_keyed(), None);
    assert!(
        !empty
            .process(
                &mut controller,
                false,
                &mut [],
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    assert!(
        !empty
            .process(
                &mut controller,
                false,
                &mut [0.0],
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    assert_eq!(dispatcher.dispatch(1), 0);

    let (prepared, _) = peer("200", 1);
    let (mut links, _) = LinkAudio::new(vec![prepared], 1).unwrap();
    assert!(
        links
            .process(
                &mut controller,
                false,
                &mut [0.0, 0.0],
                crate::link::SourcePosition::default()
            )
            .is_err()
    );
}

#[test]
fn grouped_member_selection_and_last_keyed_status_follow_callback_edges() {
    let selection = GroupSelection::new();
    let slot = std::num::NonZeroUsize::new(1).unwrap();
    selection.publish_desired(Some(slot));
    let (prepared, signals) = peer("200", 1);
    let member = GroupMemberSelection::new("primary", slot, selection);
    assert_eq!(member.label(), "primary");
    assert_eq!(member.slot(), slot);
    let prepared = prepared.with_group_member(member);
    let (mut links, _) = LinkAudio::new(vec![prepared], 1).unwrap();
    let status = links.status();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert_eq!(status.last_keyed(), None);
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    signals.set_radio_keyed(true);
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(status.last_keyed(), Some("200"));
}

#[test]
fn short_and_long_link_bursts_apply_kerchunk_limit() {
    for (frames, expect_courtesy) in [(1, false), (4_801, true)] {
        let (prepared, signals, mut outbound) = peer_with_kerchunk("200", 4_802, 100);
        let (mut links, _) = LinkAudio::new(vec![prepared], 4_802).unwrap();
        let courtesy = CourtesySettings {
            link: Some(
                crate::controller::PreparedMedia::new(
                    Some(vec![0.5]),
                    "",
                    crate::controller::MorseSettings::default(),
                )
                .unwrap(),
            ),
            ..CourtesySettings::default()
        };
        let (mut controller, _) = NodeController::new(
            ControllerSettings {
                courtesy_delay_ms: 0,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            courtesy,
        )
        .unwrap();

        signals.set_radio_keyed(true);
        links
            .process(
                &mut controller,
                false,
                &mut vec![0.0; frames],
                crate::link::SourcePosition::default(),
            )
            .unwrap();
        signals.set_radio_keyed(false);
        let mut output = [0.0];
        links
            .process(
                &mut controller,
                false,
                &mut output,
                crate::link::SourcePosition::default(),
            )
            .unwrap();
        assert_eq!(outbound.read(&mut [0.0]), 1);
        assert_eq!(output[0] == 0.5, expect_courtesy);
    }
}

#[test]
fn ended_peer_is_not_enabled_as_a_dispatch_destination() {
    let (prepared, signals, mut outbound) = peer_with_kerchunk("201", 1, 0);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![prepared], 1).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    signals.set_radio_keyed(true);
    signals.end();
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(dispatcher.dispatch(1), 1);
    let mut output = [1.0];
    assert_eq!(outbound.read(&mut output), 1);
    assert_eq!(output, [0.0]);
}
