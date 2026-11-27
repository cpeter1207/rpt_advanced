use super::*;
use crate::webrtc::{
    self, CHANNEL_PROTOCOL, ChannelConfig, Direction, Reliability, SdpOffer, SeqNo,
};

#[test]
fn ice_restart_keeps_authenticated_channels_and_rtp_on_the_same_driver() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let now = Instant::now();
            let mut caller = webrtc::transport(now);
            let mut change = caller.sdp_api();
            let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
            for (label, ordered, reliability) in [
                ("control", true, Reliability::Reliable),
                (
                    "timing",
                    false,
                    Reliability::MaxPacketLifetime { lifetime: 100 },
                ),
            ] {
                change.add_channel_with_config(ChannelConfig {
                    label: label.into(),
                    protocol: CHANNEL_PROTOCOL.into(),
                    ordered,
                    reliability,
                    negotiated: None,
                });
            }
            let (offer, pending) = change.apply().unwrap();
            let mut basis = IceRestartBasis::from_offer(&offer).unwrap();
            let initial = offer.to_sdp_string();
            let (answerer, answer) = webrtc::accept_offer(now, offer).unwrap();
            caller.sdp_api().accept_answer(pending, answer).unwrap();
            let socket = || UdpSocket::bind(("127.0.0.1", 0));
            let mut caller = UdpDriver::new(caller, socket().await.unwrap()).unwrap();
            let mut answerer = UdpDriver::new(answerer, socket().await.unwrap()).unwrap();
            let old_caller = caller.candidate().to_owned();
            let old_answerer = answerer.candidate().to_owned();
            caller.add_remote_candidate(&old_answerer).unwrap();
            answerer.add_remote_candidate(&old_caller).unwrap();

            tokio::time::timeout(Duration::from_secs(5), async {
                while caller.channels.contains(&None)
                    || answerer.channels.contains(&None)
                    || !caller.connection_state().secure_connected()
                    || !answerer.connection_state().secure_connected()
                {
                    tokio::select! {
                        event = caller.next_event() => record_open(&mut caller, event.unwrap()),
                        event = answerer.next_event() => record_open(&mut answerer, event.unwrap()),
                    }
                }
            })
            .await
            .expect("original ICE and both DCEP channels");
            let channel_ids = [caller.channels, answerer.channels];
            let payload = answerer.opus_payload(mid).unwrap();
            let frame = |sequence, timestamp| {
                RtpWrite::new(
                    payload,
                    SeqNo::from(sequence),
                    timestamp,
                    Instant::now(),
                    &[0xf8, 0xff, 0xfe][..],
                )
            };
            let source = answerer.write_rtp(mid, frame(1_u64, 9_600)).unwrap();

            let previous = caller.rtc.direct_api().local_ice_credentials();
            let mut restart = caller.rtc.sdp_api();
            let next = restart.ice_restart(true);
            let (offer, pending) = restart.apply().unwrap();
            let changed = offer.to_sdp_string();
            let mut fingerprint = changed
                .lines()
                .find(|line| line.starts_with("a=fingerprint:"))
                .unwrap()
                .to_owned();
            let digit = fingerprint.find(' ').unwrap() + 1;
            let other = if fingerprint.as_bytes()[digit] == b'A' {
                "B"
            } else {
                "A"
            };
            let original_fingerprint = fingerprint.clone();
            fingerprint.replace_range(digit..digit + 1, other);
            for invalid in [
                initial,
                changed.replace(&next.ufrag, &previous.ufrag),
                changed.replace(&next.pass, &previous.pass),
                changed.replace("a=sendrecv", "a=recvonly"),
                changed.replace(&original_fingerprint, &fingerprint),
            ] {
                let offer = SdpOffer::from_sdp_string(&invalid).unwrap();
                assert!(answerer.accept_restart_offer(&mut basis, offer).is_err());
                assert!(answerer.connection_state().secure_connected());
            }

            let answer = answerer.accept_restart_offer(&mut basis, offer).unwrap();
            assert_eq!(
                answerer.connection_state().ice,
                IceConnectionState::Checking
            );
            assert!(answerer.connection_state().dtls_authenticated);
            assert_eq!(answerer.channels, channel_ids[1]);
            assert!(
                answerer
                    .write_channel(ChannelKind::Control, false, b"old", 8, 8)
                    .is_err()
            );
            assert!(answerer.add_remote_candidate(&old_caller).is_err());
            assert_ne!(answerer.candidate(), old_answerer);
            caller.accept_restart_answer(pending, answer).unwrap();

            tokio::time::timeout(Duration::from_secs(5), async {
                while !caller.connection_state().secure_connected()
                    || !answerer.connection_state().secure_connected()
                {
                    tokio::select! {
                        event = caller.next_event() => retained(event.unwrap()),
                        event = answerer.next_event() => retained(event.unwrap()),
                    }
                }
            })
            .await
            .expect("fresh ICE nomination on the retained authenticated association");
            assert_eq!([caller.channels, answerer.channels], channel_ids);
            assert!(
                answerer
                    .write_channel(ChannelKind::Control, false, b"new", 8, 8)
                    .unwrap()
            );
            assert!(
                answerer
                    .write_channel(ChannelKind::Timing, true, b"tick", 8, 8)
                    .unwrap()
            );
            assert_eq!(
                answerer.write_rtp(mid, frame(2_u64, 10_560)).unwrap(),
                source
            );
            tokio::time::timeout(Duration::from_secs(3), async {
                let mut received = [false; 3];
                while !received.into_iter().all(|seen| seen) {
                    tokio::select! {
                        event = caller.next_event() => match event.unwrap() {
                            Event::ChannelData(data) if data.id == channel_ids[0][0].unwrap() => {
                                received[0] = !data.binary && data.data.as_slice() == b"new";
                            }
                            Event::ChannelData(data) if data.id == channel_ids[0][1].unwrap() => {
                                received[1] = data.binary && data.data.as_slice() == b"tick";
                            }
                            Event::RtpPacket(packet) if packet.header.timestamp == 10_560 => {
                                received[2] = packet.header.ssrc == source;
                            }
                            event => retained(event),
                        },
                        event = answerer.next_event() => retained(event.unwrap()),
                    }
                }
            })
            .await
            .expect("both original channels and RTP operate on the new ICE pair");
        });
}

fn record_open(driver: &mut UdpDriver, event: Event) {
    if let Event::ChannelOpen(id, _) = event {
        assert!(driver.bind_channel(id, 100).is_some());
    }
}

fn retained(event: Event) {
    assert!(
        !matches!(
            event,
            Event::Connected | Event::Closed | Event::ChannelOpen(..) | Event::ChannelClose(_)
        ),
        "restart must preserve the DTLS and DCEP associations"
    );
}

#[test]
fn driver_reports_consent_loss_before_returning_disconnected() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let now = Instant::now();
            let mut caller = webrtc::transport(now);
            let mut change = caller.sdp_api();
            change.add_channel_with_config(ChannelConfig {
                label: "control".into(),
                protocol: CHANNEL_PROTOCOL.into(),
                ordered: true,
                reliability: Reliability::Reliable,
                negotiated: None,
            });
            let (offer, pending) = change.apply().unwrap();
            let (answerer, answer) = webrtc::accept_offer(now, offer).unwrap();
            caller.sdp_api().accept_answer(pending, answer).unwrap();

            let socket = || UdpSocket::bind(("127.0.0.1", 0));
            let mut caller = UdpDriver::new(caller, socket().await.unwrap()).unwrap();
            let mut answerer = UdpDriver::new(answerer, socket().await.unwrap()).unwrap();
            caller.add_remote_candidate(answerer.candidate()).unwrap();
            answerer.add_remote_candidate(caller.candidate()).unwrap();
            tokio::time::timeout(Duration::from_secs(4), async {
                while !caller.connection_state().secure_connected()
                    || !answerer.connection_state().secure_connected()
                    || caller.channels[0].is_none()
                    || answerer.channels[0].is_none()
                {
                    tokio::select! {
                        event = caller.next_event() => record_open(&mut caller, event.unwrap()),
                        event = answerer.next_event() => record_open(&mut answerer, event.unwrap()),
                    }
                }
            })
            .await
            .expect("the only ICE pair and its DCEP channel are established");

            let channel = answerer.channels[0].unwrap();
            assert!(
                answerer
                    .write_channel(ChannelKind::Control, false, b"alive", 8, 64)
                    .unwrap()
            );

            // Keep the peer socket bound and stop polling it. The real driver
            // must lose its only authenticated pair without a local socket error,
            // a DTLS close, or any signaled session teardown.
            tokio::time::timeout(Duration::from_secs(29), async {
                loop {
                    match answerer.next_event().await.unwrap() {
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => break,
                        Event::Closed => panic!("transport closed before observing ICE loss"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("the unresponsive pair fails the pinned default consent checks");
            assert_eq!(
                answerer.connection_state().ice,
                IceConnectionState::Disconnected
            );
            assert!(answerer.connection_state().dtls_authenticated);
            assert!(!answerer.connection_state().secure_connected());
            assert_eq!(
                answerer.bound_channel_kind(channel),
                Some(ChannelKind::Control)
            );
            assert_eq!(
                answerer
                    .write_channel(ChannelKind::Control, false, b"lost", 8, 64)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotConnected
            );
            drop(caller);
        });
}
