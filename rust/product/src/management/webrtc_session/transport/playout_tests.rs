//! Finite native Opus tails and resumed source coordinates on one negotiated track.

use super::*;
use crate::{host::webrtc::Endpoint, link::source_clock::SourceClock};
use rpt_advanced_core::audio::{AudioSpan, LinkAudioQueue};
use webrtc::{ChannelConfig, Reliability, channel::Control, opus::PACKET_FRAMES};

const FRAMES: usize = PACKET_FRAMES as usize;

async fn connected_audio() -> (UdpDriver, UdpDriver, Mid) {
    let now = Instant::now();
    let mut browser = webrtc::transport(now);
    let mut change = browser.sdp_api();
    let mid = change.add_media(
        MediaKind::Audio,
        webrtc::Direction::SendRecv,
        None,
        None,
        None,
    );
    change.add_channel_with_config(ChannelConfig {
        label: "control".into(),
        protocol: webrtc::CHANNEL_PROTOCOL.into(),
        ordered: true,
        reliability: Reliability::Reliable,
        negotiated: None,
    });
    let (offer, pending) = change.apply().unwrap();
    let (station, answer) = webrtc::accept_offer(now, offer).unwrap();
    browser.sdp_api().accept_answer(pending, answer).unwrap();
    let socket = || UdpSocket::bind(("127.0.0.1", 0));
    let mut browser = UdpDriver::new(browser, socket().await.unwrap()).unwrap();
    let mut station = UdpDriver::new(station, socket().await.unwrap()).unwrap();
    browser.add_remote_candidate(station.candidate()).unwrap();
    station.add_remote_candidate(browser.candidate()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                event = browser.next_event() => { event.unwrap(); }
                event = station.next_event() => {
                    if let Event::ChannelOpen(id, _) = event.unwrap() {
                        assert_eq!(station.bind_channel(id, TIMING_LIFETIME_MS), Some(ChannelKind::Control));
                        break;
                    }
                }
            }
        }
    })
    .await
    .expect("establish the authorized transport's anchor channel");
    (station, browser, mid)
}

fn test_provider() -> OpusProvider {
    match std::env::var_os("RPTADV_TEST_WEBRTC_OPUS") {
        Some(path) => OpusProvider::load_from(Path::new(&path)),
        None => OpusProvider::load(),
    }
    .unwrap()
}

fn test_playout(provider: &OpusProvider) -> (playout::Playout, Arc<Mutex<WebRtcObservation>>) {
    let key = WorkKey {
        session: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        port: "00000000-0000-4000-8000-000000000002".parse().unwrap(),
        ice_generation: 1,
    };
    let binding = "00000000-0000-4000-8000-000000000003".parse().unwrap();
    let observation = Arc::new(Mutex::new(WebRtcObservation {
        binding,
        connection_generation: key.ice_generation,
        transceive: false,
        graph_generation: 4,
        anchor: None,
        codec_input: None,
        decoded: None,
        ingress: None,
        sender_rate: None,
        receiver_quality: Vec::new(),
    }));
    let playout =
        playout::Playout::new(provider, key, binding, Arc::default(), observation.clone()).unwrap();
    (playout, observation)
}

async fn check_resume(partial: usize, expected_packets: usize) {
    let (mut station, mut browser, mid) = connected_audio().await;
    let track = (mid, station.opus_payload(mid).unwrap());
    let provider = test_provider();
    let decoder = provider.decoder(provider.encoder_bytes()).unwrap();
    let (mut playout, observation) = test_playout(&provider);
    let clock = Arc::new(SourceClock::new(Instant::now() - Duration::from_secs(1)));
    clock.restart();
    let (mut source, egress) = LinkAudioQueue::new(3 * FRAMES, 4).unwrap().into_endpoints();
    let mut endpoint = Endpoint::for_playout_test(clock.clone(), egress);
    // The first source turn is old enough to drain when its queue empties, but still fresh.
    let span = AudioSpan {
        generation: 4,
        burst: 5,
        source_start: 96_000,
        stream: clock.stream(),
        born_ns: Some(clock.now_ns() - 90_000_000),
    };
    let count = FRAMES + partial;
    let pcm: Vec<_> = (0..count).map(|i| (i as f32 * 0.13).sin() * 0.2).collect();
    assert_eq!(source.write(span, &pcm), 0);
    playout
        .send(&mut endpoint, &mut station, track)
        .expect("first full packet");
    let first = observation
        .lock()
        .unwrap()
        .anchor
        .clone()
        .expect("initial anchor");
    // Exactly one empty-queue turn before fresh PCM arrives. With 800 extra samples
    // the finite flush spans two 960-tick packets because of the Opus lookahead.
    playout
        .send(&mut endpoint, &mut station, track)
        .expect("idle drain");
    let resumed = AudioSpan {
        source_start: span.source_start + count as u64,
        born_ns: Some(clock.now_ns()),
        ..span
    };
    assert_eq!(source.write(resumed, &pcm[..FRAMES]), 0);
    for _ in 0..3 {
        playout
            .send(&mut endpoint, &mut station, track)
            .expect("resume after one idle turn");
        if observation
            .lock()
            .unwrap()
            .anchor
            .as_ref()
            .is_some_and(|a| a.source_sample.0 == resumed.source_start)
        {
            break;
        }
    }
    let current = observation
        .lock()
        .unwrap()
        .anchor
        .clone()
        .expect("resumed anchor");
    assert_eq!(current.source_sample.0, resumed.source_start);
    assert_eq!(current.time_ns.map(|t| t.0), resumed.born_ns);
    assert!(current.epoch.0 > first.epoch.0);
    assert_eq!(current.source_generation, first.source_generation);
    assert_eq!(current.binding, first.binding);
    assert_eq!(current.ssrc, first.ssrc);

    let mut packets = Vec::new();
    let mut anchors = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while packets.len() < expected_packets || anchors.len() < 2 {
            tokio::select! {
                event = station.next_event() => { event.unwrap(); }
                event = browser.next_event() => match event.unwrap() {
                    Event::RtpPacket(packet) => {
                        assert_eq!(*packet.header.ssrc, current.ssrc);
                        assert_eq!(packet.header.payload_type, track.1);
                        assert_eq!(decoder.inspect(&packet.payload).unwrap().frames, PACKET_FRAMES);
                        packets.push((*packet.seq_no, packet.time.numer()));
                    }
                    Event::ChannelData(data) => {
                        let Some(Control::Anchor { reference }) = Control::decode(&data.data) else {
                            panic!("expected the Product anchor on the control channel");
                        };
                        anchors.push(reference);
                    }
                    _ => {}
                }
            }
        }
    })
    .await
    .expect("all finite Opus packets and both anchors reached the receiving peer");
    assert_eq!(anchors, [first.clone(), current.clone()]);
    assert_eq!(packets.len(), expected_packets);
    for pair in packets.windows(2) {
        assert_eq!(
            pair[1],
            (pair[0].0 + 1, pair[0].1 + u64::from(PACKET_FRAMES))
        );
    }
    assert_eq!(first.source_sample.0, span.source_start);
    let lookahead = first.rtp_timestamp.0 - packets[0].1;
    assert!(lookahead > 0 && lookahead < u64::from(PACKET_FRAMES));
    assert_eq!(
        current.rtp_timestamp.0,
        packets.last().unwrap().1 + lookahead
    );
}

#[tokio::test]
async fn idle_full_opus_tail_resumes_at_a_new_wire_anchor() {
    check_resume(0, 3).await;
}

#[tokio::test]
async fn idle_partial_opus_tail_resumes_at_a_new_wire_anchor() {
    check_resume(800, 4).await;
}

#[tokio::test]
async fn hot_station_mix_keeps_both_tones_below_clip_and_a_fresh_solo_run_at_unity() {
    const PACKETS: usize = 22; // Warm the 400 ms meter window and cover the final codec lookahead.
    const VOICE: f64 = 0.9375;
    const TELEMETRY: f64 = 0.5;
    let (mut station, mut browser, mid) = connected_audio().await;
    let track = (mid, station.opus_payload(mid).unwrap());
    let provider = test_provider();
    let mut decoder = provider.decoder(provider.encoder_bytes()).unwrap();
    let (mut playout, observation) = test_playout(&provider);
    let clock = Arc::new(SourceClock::default());
    let (mut source, egress) = LinkAudioQueue::new(FRAMES, 2).unwrap().into_endpoints();
    let mut endpoint = Endpoint::for_playout_test(clock.clone(), egress);
    let mut previous_wire = None;
    let mut previous_anchor = None;
    let mut next_source = 96_000;
    let mut pcm = [0.0; FRAMES];
    let mut decoded = [0.0; FRAMES];
    let mut spans = [webrtc::opus::DecodeSpan::default(); 48];

    for hot in [true, false] {
        // A physical reopen retains the native sample position and creates a fresh media epoch.
        clock.restart();
        let run_start = next_source;
        let telemetry = if hot { TELEMETRY } else { 0.0 };
        for (index, value) in pcm.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU * index as f64 / 48_000.0;
            *value = (VOICE * (1000.0 * phase).cos() + telemetry * (1500.0 * phase).cos()) as f32;
        }
        let mut saw_solo_rms = false;
        for index in 0..PACKETS {
            let span = AudioSpan {
                generation: 4,
                burst: 1,
                stream: clock.stream(),
                source_start: next_source,
                born_ns: Some(clock.now_ns()),
            };
            assert_eq!(source.write(span, &pcm), 0);
            playout.send(&mut endpoint, &mut station, track).unwrap();
            let packet = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    tokio::select! {
                        event = station.next_event() => { event.unwrap(); }
                        event = browser.next_event() => {
                            if let Event::RtpPacket(packet) = event.unwrap() { break packet; }
                        }
                    }
                }
            })
            .await
            .expect("the receiving peer got this Opus packet");
            assert_eq!(packet.header.payload_type, track.1);
            assert_eq!(
                decoder
                    .decode(&packet.payload, &mut decoded, &mut spans)
                    .unwrap()
                    .frames,
                FRAMES
            );
            let wire = (*packet.seq_no, packet.time.numer());
            if let Some((sequence, timestamp)) = previous_wire {
                assert_eq!(wire, (sequence + 1, timestamp + u64::from(PACKET_FRAMES)));
            }
            previous_wire = Some(wire);
            let status = observation.lock().unwrap();
            if index == 0 {
                let anchor = status.anchor.as_ref().unwrap();
                assert_eq!(anchor.ssrc, *packet.header.ssrc);
                assert_eq!(anchor.source_generation.0, clock.stream());
                assert_eq!(anchor.source_sample.0, run_start);
                let lead = anchor.rtp_timestamp.0 - wire.1;
                assert!((1..u64::from(PACKET_FRAMES)).contains(&lead));
                if let Some((previous_epoch, previous_lead)) = previous_anchor {
                    assert!(anchor.epoch.0 > previous_epoch);
                    assert_eq!(lead, previous_lead);
                }
                previous_anchor = Some((anchor.epoch.0, lead));
            }
            if !hot
                && let Some(reading) = status.codec_input
                && let Some(rms) = reading.rms_dbfs
            {
                let expected = 20.0 * (VOICE / 2.0_f64.sqrt()).log10();
                assert!(
                    (rms - expected).abs() < 0.01,
                    "solo RMS {rms} dBFS, expected {expected}"
                );
                saw_solo_rms = true;
            }
            next_source += u64::from(PACKET_FRAMES);
        }
        let reading = observation.lock().unwrap().codec_input.unwrap();
        assert_eq!(reading.quality, crate::status_meter::Quality::Valid);
        assert_eq!(reading.valid_samples, 19_200);
        assert_eq!(reading.source_generation, clock.stream());
        assert!(reading.end_sample > run_start && reading.end_sample <= next_source);
        assert_eq!(
            reading.clipped_samples, 0,
            "over-range samples reached the Opus encoder input"
        );
        let peak = reading.peak_dbfs.unwrap();
        if hot {
            assert!(peak < 0.0);
            // A flat-topped signal changes the ratio between its RMS and its peak.
            let expected = 20.0
                * (((VOICE * VOICE + TELEMETRY * TELEMETRY) / 2.0).sqrt() / (VOICE + TELEMETRY))
                    .log10();
            assert!((reading.rms_dbfs.unwrap() - peak - expected).abs() < 0.1);
            for hz in [1000.0, 1500.0] {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, sample) in decoded.iter().enumerate() {
                    let phase = std::f64::consts::TAU * hz * i as f64 / 48_000.0;
                    re += f64::from(*sample) * phase.cos();
                    im += f64::from(*sample) * phase.sin();
                }
                assert!(
                    2.0 * re.hypot(im) / FRAMES as f64 > 0.1,
                    "{hz} Hz missing from received Opus"
                );
            }
        } else {
            assert!(saw_solo_rms);
            assert!((peak - 20.0 * VOICE.log10()).abs() < 0.01);
        }
    }
}
