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
