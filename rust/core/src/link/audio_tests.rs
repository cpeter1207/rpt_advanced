use super::*;
use crate::audio::{LinkAudioConsumer, LinkAudioQueue};
use crate::controller::{ControllerSettings, CourtesySettings};

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

    fn render(&mut self, output: &mut [f32]) -> bool {
        output.fill(0.0);
        false
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
    let (outbound, inbound) = LinkAudioQueue::new(maximum.max(1))
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
    links.process(&mut controller, false, &mut [0.0]).unwrap();
    dispatcher.dispatch(1);
    assert_eq!(first_output.read(&mut [0.0]), 1);
    assert_eq!(second_output.read(&mut [0.0]), 1);
    first_signal.set_radio_keyed(false);
    let mut radio = [0.0; 8];
    assert!(!links.process(&mut controller, false, &mut radio).unwrap());
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
    assert!(!links.process(&mut controller, false, &mut [0.0]).unwrap());
    dispatcher.dispatch(1);
    assert_eq!(output.read(&mut [0.0]), 1);
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn addressed_reply_does_not_enable_ordinary_audio_for_monitor_or_fallback() {
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
        let media = PreparedMedia::new(Some(vec![0.4; 2]), "", MorseSettings::default())
            .unwrap()
            .reply_to(Some(signal.reply_target()));
        assert!(control.queue_prepared_status(media));
        let mut radio = [0.7; 4];
        assert!(!links.process(&mut controller, true, &mut radio).unwrap());
        dispatcher.dispatch(1);
        let mut sent = [0.0; 4];
        assert_eq!(output.read(&mut sent), 0);
        assert_eq!(sent, [0.4, 0.4, 0.0, 0.0]);
        assert_eq!(radio, [0.0; 4]);
        assert_eq!(control.reclaim().count(), 1);
    }
}

#[test]
fn preparation_and_empty_graph_reject_invalid_shapes_without_dispatch() {
    for (name, maximum) in [("bad", 1), ("200", 0)] {
        let (outbound, _) = LinkAudioQueue::new(1).unwrap().into_endpoints();
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
    assert!(!empty.process(&mut controller, false, &mut []).unwrap());
    assert!(!empty.process(&mut controller, false, &mut [0.0]).unwrap());
    assert_eq!(dispatcher.dispatch(1), 0);

    let (prepared, _) = peer("200", 1);
    let (mut links, _) = LinkAudio::new(vec![prepared], 1).unwrap();
    assert!(
        links
            .process(&mut controller, false, &mut [0.0, 0.0])
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
    links.process(&mut controller, false, &mut [0.0]).unwrap();
    links.process(&mut controller, false, &mut [0.0]).unwrap();
    signals.set_radio_keyed(true);
    links.process(&mut controller, false, &mut [0.0]).unwrap();
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
            .process(&mut controller, false, &mut vec![0.0; frames])
            .unwrap();
        signals.set_radio_keyed(false);
        let mut output = [0.0];
        links.process(&mut controller, false, &mut output).unwrap();
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
    links.process(&mut controller, false, &mut [0.0]).unwrap();
    assert_eq!(dispatcher.dispatch(1), 1);
    let mut output = [1.0];
    assert_eq!(outbound.read(&mut output), 1);
    assert_eq!(output, [0.0]);
}
