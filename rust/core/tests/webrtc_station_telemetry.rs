//! Exercise production controller and dispatcher routing through their public endpoints.
use rpt_advanced_core::{
    audio::{LinkAudioConsumer, LinkAudioQueue, PcmRead, PcmStreamReader},
    controller::{
        ControllerSettings, CourtesySettings, NodeController, PlaybackDestination, PlaybackPolicy,
        PreparedMedia,
    },
    link::{AudioPeer, LinkAudio, Mode, PeerInput, PeerSignals, SourcePosition},
};
use std::sync::{Arc, atomic::AtomicBool};

struct Marker {
    sample: f32,
    frames: usize,
    remaining: usize,
}

impl PcmStreamReader for Marker {
    fn start(&mut self) {
        self.remaining = self.frames;
    }

    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        let count = output.len().min(self.remaining);
        output[..count].fill(self.sample);
        self.remaining -= count;
        if count == 0 {
            PcmRead::Finished
        } else if self.remaining == 0 {
            PcmRead::FinalSamples(count)
        } else {
            PcmRead::Samples(count)
        }
    }
}

fn marker(sample: f32, frames: usize) -> PreparedMedia {
    PreparedMedia::new_stream(Box::new(Marker {
        sample,
        frames,
        remaining: 0,
    }))
    .unwrap()
}

struct FiniteInput {
    signals: Arc<PeerSignals>,
    sample: f32,
    remaining: usize,
}

impl PeerInput for FiniteInput {
    fn source_rate(&self) -> u32 {
        48_000
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        self.remaining as u64
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        let count = output.len().min(self.remaining);
        output[..count].fill(self.sample);
        output[count..].fill(0.0);
        self.remaining -= count;
        Some(count)
    }
}

fn application(
    session: u128,
    transceive: bool,
    sample: f32,
) -> (AudioPeer<FiniteInput>, Arc<PeerSignals>, LinkAudioConsumer) {
    let signals = Arc::new(PeerSignals::for_webrtc(uuid::Uuid::from_u128(session)));
    signals.publish_pcm();
    signals.set_webrtc_deadline(1_000_000_000);
    let (writer, out) = LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
    let peer = AudioPeer::for_application(
        &signals.reply_target(),
        transceive,
        Arc::new(AtomicBool::new(true)),
        FiniteInput {
            signals: Arc::clone(&signals),
            sample,
            remaining: 12_800,
        },
        writer,
        4,
    )
    .unwrap();
    (peer, signals, out)
}

#[test]
fn webrtc_station_telemetry_follows_each_mix_minus_and_preserves_reply_privacy() {
    let (talker, talker_signals, mut talker_out) = application(1, true, 0.125);
    let (listener, listener_signals, mut listener_out) = application(2, false, 0.0);
    let (writer, mut link_out) = LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
    let asl = AudioPeer::new(
        "200",
        Mode::TRANSCEIVE,
        FiniteInput {
            signals: Arc::new(PeerSignals::new()),
            sample: 0.0,
            remaining: 0,
        },
        writer,
        4,
        0,
    )
    .unwrap();
    let (mut links, mut dispatcher) = LinkAudio::new(vec![talker, listener, asl], 4).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            telemetry_duck_db: 0,
            courtesy_delay_ms: 0,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings {
            receiver: Some(marker(0.25, 8)),
            ..Default::default()
        },
    )
    .unwrap();
    let mut tick = |node: &mut NodeController, keyed: bool| {
        talker_signals.set_radio_keyed(keyed);
        let mut rf = [0.0; 4];
        links
            .process(
                node,
                false,
                &mut rf,
                SourcePosition {
                    born_ns: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(dispatcher.dispatch(1), 1);
        rf
    };
    let read = |out: &mut LinkAudioConsumer| {
        let mut pcm = [0.0; 4];
        out.read_span(&mut pcm).map(|span| {
            assert_eq!(span.frames, 4);
            pcm
        })
    };

    // The receiver unkeys. The first half of its courtesy has no voice to open egress.
    node.process_audio(true, false, &[], &mut [0.0]);
    assert_eq!(tick(&mut node, false), [0.25; 4]);
    assert_eq!(read(&mut talker_out), Some([0.25; 4]));
    assert_eq!(read(&mut listener_out), Some([0.25; 4]));
    assert_eq!(read(&mut link_out), None);

    // The operator keys during the same courtesy. Only the other two hear that operator.
    assert_eq!(tick(&mut node, true), [0.375; 4]);
    assert_eq!(read(&mut talker_out), Some([0.25; 4]));
    assert_eq!(read(&mut listener_out), Some([0.375; 4]));
    assert_eq!(read(&mut link_out), Some([0.125; 4]));

    assert!(
        control
            .queue_prepared_status(marker(0.5, 4).reply_to(Some(listener_signals.reply_target())))
    );
    assert_eq!(tick(&mut node, false), [0.0; 4]);
    assert_eq!(read(&mut talker_out), None);
    assert_eq!(read(&mut listener_out), Some([0.5; 4]));
    assert_eq!(read(&mut link_out), None);

    node.process_audio(false, false, &[], &mut [0.0; 12_000]);
    for destination in [
        PlaybackDestination::Local,
        PlaybackDestination::LocalAndLinks,
        PlaybackDestination::Peers,
    ] {
        assert!(
            control.queue_prepared_status(marker(0.25, 4).with_playback_policy(PlaybackPolicy {
                destination,
                ..Default::default()
            }))
        );
        let rf = tick(&mut node, false);
        assert_eq!(
            rf,
            if destination == PlaybackDestination::Peers {
                [0.0; 4]
            } else {
                [0.25; 4]
            }
        );
        assert_eq!(read(&mut talker_out), Some([0.25; 4]));
        assert_eq!(read(&mut listener_out), Some([0.25; 4]));
        assert_eq!(
            read(&mut link_out),
            (destination != PlaybackDestination::Local).then_some([0.25; 4])
        );
    }
}
