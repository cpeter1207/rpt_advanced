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

async fn check_resume(partial: usize, expected_packets: usize) {
    let (mut station, mut browser, mid) = connected_audio().await;
    let track = (mid, station.opus_payload(mid).unwrap());
    let provider = match std::env::var_os("RPTADV_TEST_WEBRTC_OPUS") {
        Some(path) => OpusProvider::load_from(Path::new(&path)),
        None => OpusProvider::load(),
    }
    .unwrap();
    let decoder = provider.decoder(provider.encoder_bytes()).unwrap();
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
    let mut playout =
        playout::Playout::new(&provider, key, binding, Arc::default(), observation.clone())
            .unwrap();
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
