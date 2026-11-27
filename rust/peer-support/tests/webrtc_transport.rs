//! Controller transport interoperability through the actual ICE, DTLS, SCTP and RTP stack.

#![cfg(target_os = "linux")]

use rptadv_peer_support::webrtc::{self, CHANNEL_PROTOCOL, ChannelKind};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Receive, Transmit};
use str0m::rtp::{RtpWrite, SeqNo};
use str0m::{Candidate, Event, Input, Output, Rtc};

const OFFERER: usize = 0;
const ANSWERER: usize = 1;
const LABELS: [&str; 2] = ["control", "timing"];
const TIMING_LIFETIME_MS: u16 = 100;

struct Endpoint {
    addr: SocketAddr,
    rtc: Rtc,
    timeout: Instant,
    events: Vec<Event>,
    last_arrival: Option<Instant>,
}

impl Endpoint {
    fn new(addr: &str, now: Instant) -> Self {
        Self {
            addr: addr.parse().unwrap(),
            rtc: webrtc::transport(now),
            timeout: now,
            events: Vec::new(),
            last_arrival: None,
        }
    }

    fn opened(&self, label: &str) -> Option<ChannelId> {
        self.events.iter().find_map(|event| match event {
            Event::ChannelOpen(id, actual) if actual == label => Some(*id),
            _ => None,
        })
    }
}

struct Wire {
    now: Instant,
    endpoints: [Endpoint; 2],
    packets: VecDeque<Transmit>,
}

impl Wire {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            now,
            endpoints: [
                Endpoint::new("192.0.2.1:5100", now),
                Endpoint::new("192.0.2.2:5101", now),
            ],
            packets: VecDeque::new(),
        }
    }

    fn update<T>(&mut self, endpoint: usize, mutate: impl FnOnce(&mut Rtc) -> T) -> T {
        let peer = &mut self.endpoints[endpoint];
        let result = mutate(&mut peer.rtc);
        loop {
            match peer.rtc.poll_output().unwrap() {
                Output::Transmit(packet) => self.packets.push_back(packet),
                Output::Event(event) => peer.events.push(event),
                Output::Timeout(deadline) => {
                    peer.timeout = deadline;
                    return result;
                }
            }
        }
    }

    fn advance_until(&mut self, condition: impl Fn(&[Endpoint; 2]) -> bool) {
        let deadline = self.now + Duration::from_secs(10);
        for _ in 0..4_096 {
            if condition(&self.endpoints) {
                return;
            }
            assert!(
                self.now <= deadline,
                "transport did not advance before deadline"
            );
            if let Some(packet) = self.packets.pop_front() {
                self.now += Duration::from_millis(1);
                let endpoint = self
                    .endpoints
                    .iter()
                    .position(|peer| peer.addr == packet.destination)
                    .expect("packet addressed to a test endpoint");
                let arrival = self.now;
                self.endpoints[endpoint].last_arrival = Some(arrival);
                self.update(endpoint, |rtc| {
                    let input = Receive::new(
                        packet.proto,
                        packet.source,
                        packet.destination,
                        &packet.contents,
                    )
                    .unwrap();
                    rtc.handle_input(Input::Receive(arrival, input)).unwrap();
                });
            } else {
                let endpoint =
                    usize::from(self.endpoints[ANSWERER].timeout < self.endpoints[OFFERER].timeout);
                self.now = self.now.max(self.endpoints[endpoint].timeout);
                let now = self.now;
                self.update(endpoint, |rtc| {
                    rtc.handle_input(Input::Timeout(now)).unwrap()
                });
            }
        }
        panic!("virtual transport exhausted its round budget");
    }

    fn trickle(&mut self, source: usize, destination: usize) {
        let candidate = Candidate::host(self.endpoints[source].addr, "udp").unwrap();
        let text = candidate.to_sdp_string();
        self.update(source, |rtc| {
            assert!(rtc.add_local_candidate(candidate).is_some());
        });
        self.update(destination, |rtc| {
            rtc.add_remote_candidate(Candidate::from_sdp_string(&text).unwrap());
        });
    }
}

#[test]
fn trickle_dtls_dcep_and_opus_timing() {
    let mut wire = Wire::new();
    let (mid, channels, offer, pending) = wire.update(OFFERER, |rtc| {
        let mut change = rtc.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let channels = LABELS.map(|label| {
            let ordered = label == LABELS[0];
            change.add_channel_with_config(ChannelConfig {
                label: label.into(),
                ordered,
                reliability: if ordered {
                    Reliability::Reliable
                } else {
                    Reliability::MaxPacketLifetime {
                        lifetime: TIMING_LIFETIME_MS,
                    }
                },
                negotiated: None,
                protocol: CHANNEL_PROTOCOL.into(),
            })
        });
        let (offer, pending) = change.apply().unwrap();
        (mid, channels, offer, pending)
    });
    assert!(offer.to_sdp_string().contains("a=fingerprint:sha-256 "));
    let now = wire.now;
    let offer = webrtc::SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap();
    let answer = wire.update(ANSWERER, |rtc| {
        let (connection, answer) = webrtc::accept_offer(now, offer).unwrap();
        *rtc = connection;
        answer
    });
    assert!(answer.to_sdp_string().contains("a=fingerprint:sha-256 "));
    wire.update(OFFERER, |rtc| {
        rtc.sdp_api().accept_answer(pending, answer).unwrap();
    });
    assert!(wire.packets.is_empty());
    wire.trickle(OFFERER, ANSWERER);
    wire.trickle(ANSWERER, OFFERER);
    wire.advance_until(|peers| {
        peers.iter().all(|peer| {
            peer.events
                .iter()
                .any(|event| matches!(event, Event::Connected))
                && LABELS.iter().all(|label| peer.opened(label).is_some())
        })
    });

    let peer_channels = LABELS.map(|label| wire.endpoints[ANSWERER].opened(label).unwrap());
    for (index, id) in peer_channels.into_iter().enumerate() {
        let channel = wire.endpoints[ANSWERER].rtc.channel(id).unwrap();
        let config = channel.config().unwrap();
        let kind = if index == 0 {
            ChannelKind::Control
        } else {
            ChannelKind::Timing
        };
        assert_eq!(webrtc::channel_kind(config, TIMING_LIFETIME_MS), Some(kind));
        if kind == ChannelKind::Timing {
            assert_eq!(webrtc::channel_kind(config, TIMING_LIFETIME_MS - 1), None);
        }
        wire.update(OFFERER, |rtc| {
            assert!(
                rtc.channel(channels[index])
                    .unwrap()
                    .write(true, LABELS[index].as_bytes())
                    .unwrap()
            );
        });
    }
    wire.advance_until(|peers| {
        peers[ANSWERER]
            .events
            .iter()
            .filter(|event| matches!(event, Event::ChannelData(_)))
            .count()
            == 2
    });
    for (id, label) in peer_channels.into_iter().zip(LABELS) {
        assert!(wire.endpoints[ANSWERER].events.iter().any(|event| {
            matches!(event, Event::ChannelData(msg) if msg.id == id && msg.binary && msg.data == label.as_bytes())
        }));
    }

    const SEQUENCE: u64 = 42;
    const TIMESTAMP: u32 = 424_960;
    const OPUS_SILENCE: &[u8] = &[0xf8, 0xff, 0xfe];
    let now = wire.now;
    let ssrc = wire.update(OFFERER, |rtc| {
        let pt = rtc.codec_config().params()[0].pt();
        let mut direct = rtc.direct_api();
        let stream = direct.stream_tx_by_mid(mid, None).unwrap();
        stream.write_rtp(RtpWrite::new(
            pt,
            SeqNo::from(SEQUENCE),
            TIMESTAMP,
            now,
            OPUS_SILENCE,
        ));
        stream.ssrc()
    });
    wire.advance_until(|peers| {
        peers[ANSWERER]
            .events
            .iter()
            .any(|event| matches!(event, Event::RtpPacket(_)))
    });
    let peer = &wire.endpoints[ANSWERER];
    let packet = peer
        .events
        .iter()
        .find_map(|event| match event {
            Event::RtpPacket(packet) => Some(packet),
            _ => None,
        })
        .unwrap();
    assert_eq!(packet.header.ssrc, ssrc);
    assert_eq!(packet.header.timestamp, TIMESTAMP);
    assert_eq!(packet.seq_no, SeqNo::from(SEQUENCE));
    assert_eq!(
        packet.time,
        MediaTime::new(u64::from(TIMESTAMP), Frequency::FORTY_EIGHT_KHZ)
    );
    assert_eq!(Some(packet.timestamp), peer.last_arrival);
    assert!(packet.timestamp > now);
    assert_eq!(packet.payload.as_ref(), OPUS_SILENCE);
    // The submitted codec frame starts at native sample 96,000. A 20 ms
    // meter window at that input must map to the packet's exact RTP interval.
    let anchor = webrtc::MediaAnchor {
        source_generation: 12,
        media_epoch: 4,
        source_sample: 96_000,
        rtp_timestamp: packet.time.numer(),
    };
    assert_eq!(
        anchor.rtp_window(12, 4, 96_000..96_960),
        Some(u64::from(TIMESTAMP)..u64::from(TIMESTAMP) + 960),
    );
    assert_eq!(anchor.rtp_window(13, 4, 96_000..96_960), None);
    assert_eq!(anchor.rtp_window(12, 5, 96_000..96_960), None);
    assert_eq!(webrtc::native_delta_ns(96_000, 96_960), 20_000_000);
}

#[test]
fn driver_exchanges_direct_ice_on_owned_udp_sockets() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        let now = Instant::now();
        let mut caller = webrtc::transport(now);
        let mut change = caller.sdp_api();
        let media = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        for (label, protocol, ordered, reliability) in [
            ("control", CHANNEL_PROTOCOL, true, Reliability::Reliable),
            ("control", CHANNEL_PROTOCOL, true, Reliability::Reliable),
            (
                "timing",
                CHANNEL_PROTOCOL,
                false,
                Reliability::MaxPacketLifetime {
                    lifetime: TIMING_LIFETIME_MS,
                },
            ),
            ("timing", "unrecognized.version", true, Reliability::Reliable),
        ] {
            change.add_channel_with_config(ChannelConfig {
                label: label.into(),
                protocol: protocol.into(),
                ordered,
                reliability,
                negotiated: None,
            });
        }
        let (offer, pending) = change.apply().unwrap();
        let (remote, answer) = webrtc::accept_offer(now, offer).unwrap();
        caller.sdp_api().accept_answer(pending, answer).unwrap();
        let socket = || tokio::net::UdpSocket::bind(("127.0.0.1", 0));
        let mut caller = webrtc::UdpDriver::new(caller, socket().await.unwrap()).unwrap();
        let mut remote = webrtc::UdpDriver::new(remote, socket().await.unwrap()).unwrap();
        let opus = caller.opus_payload(media).unwrap();
        assert_eq!(remote.opus_payload(media), Some(opus));
        assert!(!remote.connection_state().secure_connected());

        caller.add_remote_candidate(remote.candidate()).unwrap();
        remote.add_remote_candidate(caller.candidate()).unwrap();
        let control = tokio::time::timeout(Duration::from_secs(5), async {
            let mut control = None;
            let mut timing = None;
            let mut rejected = 0;
            while !caller.connection_state().secure_connected()
                || !remote.connection_state().secure_connected()
                || control.is_none()
                || timing.is_none()
                || rejected < 2
            {
                tokio::select! {
                    event = caller.next_event() => { event.unwrap(); }
                    event = remote.next_event() => {
                        if let Event::ChannelOpen(id, _) = event.unwrap() {
                            match remote.bind_channel(id, TIMING_LIFETIME_MS) {
                                Some(ChannelKind::Control) => assert!(control.replace(id).is_none()),
                                Some(ChannelKind::Timing) => assert!(timing.replace(id).is_none()),
                                None => {
                                    assert_eq!(remote.bound_channel_kind(id), None);
                                    rejected += 1;
                                }
                            }
                        }
                    }
                }
            }
            assert_eq!(rejected, 2);
            assert_eq!(
                remote.bound_channel_kind(timing.unwrap()),
                Some(ChannelKind::Timing),
            );
            control.unwrap()
        })
        .await
        .expect("both nominated authenticated paths and the two validated DCEP channels");
        assert!(remote.connection_state().ice.is_connected());
        assert_eq!(remote.bound_channel_kind(control), Some(ChannelKind::Control));
        assert_eq!(
            remote
                .write_channel(ChannelKind::Control, false, b"too long", 4, 8)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput,
        );
        assert!(!remote.write_channel(ChannelKind::Control, false, b"ready", 8, 4).unwrap());
        assert!(remote.write_channel(ChannelKind::Control, false, b"ready", 8, 8).unwrap());

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    event = remote.next_event() => { event.unwrap(); }
                    event = caller.next_event() => {
                        if let Event::ChannelData(message) = event.unwrap() {
                            assert!(!message.binary);
                            assert_eq!(message.data.as_slice(), b"ready");
                            break;
                        }
                    }
                }
            }
        })
        .await
        .expect("bounded data reached the accepted peer channel");
        remote.close_channel(control);
        assert_eq!(remote.bound_channel_kind(control), None);

        let frame = RtpWrite::new(opus, SeqNo::from(5), 9_600, Instant::now(), &[0xf8, 0xff, 0xfe][..]);
        let source = caller.write_rtp(media, frame).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    event = caller.next_event() => { event.unwrap(); }
                    event = remote.next_event() => {
                        if let Event::RtpPacket(packet) = event.unwrap() {
                            assert_eq!(packet.header.ssrc, source);
                            assert_eq!(packet.time, MediaTime::new(9_600, Frequency::FORTY_EIGHT_KHZ));
                            assert_eq!(packet.payload.as_ref(), &[0xf8, 0xff, 0xfe]);
                            break;
                        }
                    }
                }
            }
        })
        .await
        .expect("prepared Opus reached the receiver");
    });
}
