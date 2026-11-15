//! Timed RTP contracts test framing and socket admission independently.
use super::*;

fn egress() -> RtpEgress {
    RtpEgress {
        header: rtp::header::Header {
            version: 2,
            payload_type: 0,
            sequence_number: u16::MAX,
            timestamp: 77,
            ssrc: 0x12345678,
            ..Default::default()
        },
        wire: vec![0; 172].into_boxed_slice(),
        pending: None,
        epoch: 1,
        digit: None,
    }
}

fn audio(timestamp: u32) -> TimedAudio<'static> {
    TimedAudio {
        payload: &[0xff; 160],
        marker: true,
        timestamp,
        deadline_ms: 200,
        epoch: 1,
    }
}

#[test]
fn first_send_and_source_gap_keep_original_rtp_timestamps() {
    let mut sender = egress();
    for (sequence, timestamp) in [(u16::MAX, u32::MAX - 79), (0, 400)] {
        let mut received = Vec::new();
        assert_eq!(
            sender.send(audio(timestamp), 10, |wire| {
                received.extend_from_slice(wire);
                Ok(wire.len())
            }),
            Ok(SendStatus::Sent)
        );
        assert_eq!(
            &received[..4],
            &[0x80, 0x80, (sequence >> 8) as u8, sequence as u8]
        );
        assert_eq!(&received[4..8], &timestamp.to_be_bytes());
        assert_eq!(&received[8..12], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(&received[12..], &[0xff; 160]);
    }
}

#[test]
fn elapsed_deadline_never_calls_the_socket() {
    let mut sender = egress();
    assert_eq!(
        sender.send(audio(77), 200, |_| panic!("expired media reached socket")),
        Ok(SendStatus::Expired)
    );
}

#[test]
fn blocked_submission_keeps_wire_identity_and_rejects_replacement() {
    let mut sender = egress();
    let original = audio(77);
    let mut first_wire = Vec::new();
    assert_eq!(
        sender.send(original, 0, |wire| {
            first_wire.extend_from_slice(wire);
            Err(std::io::ErrorKind::WouldBlock.into())
        }),
        Ok(SendStatus::WouldBlock)
    );
    for changed in [
        TimedAudio {
            payload: &[0xd5; 160],
            ..original
        },
        TimedAudio {
            timestamp: 237,
            ..original
        },
        TimedAudio {
            marker: false,
            ..original
        },
        TimedAudio {
            deadline_ms: 201,
            ..original
        },
    ] {
        assert_eq!(
            sender.send(changed, 100, |_| panic!("replacement reached socket")),
            Err(MediaError::InvalidPacket)
        );
    }
    assert_eq!(
        sender.send(original, 199, |wire| {
            assert_eq!(wire, first_wire);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
}

#[test]
fn a_new_submission_cannot_extend_the_maximum_age_budget() {
    let mut sender = egress();
    let request = TimedAudio {
        deadline_ms: 211,
        ..audio(77)
    };
    assert_eq!(
        sender.send(request, 10, |_| panic!("overlong deadline reached socket")),
        Err(MediaError::InvalidPacket)
    );
}

#[test]
fn expiry_discards_pending_bytes_without_compressing_the_source_gap() {
    let mut sender = egress();
    assert_eq!(
        sender.send(audio(77), 0, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(
        sender.send(audio(77), 200, |_| panic!("expired retry reached socket")),
        Ok(SendStatus::Expired)
    );
    let next = TimedAudio {
        timestamp: 2477,
        deadline_ms: 400,
        ..audio(77)
    };
    assert_eq!(
        sender.send(next, 200, |wire| {
            assert_eq!(&wire[2..4], &[0xff, 0xff]);
            assert_eq!(&wire[4..8], &[0, 0, 9, 173]);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
}

#[test]
fn retirement_fences_old_work_without_discarding_the_new_generation() {
    let mut sender = egress();
    let old = audio(77);
    assert_eq!(
        sender.send(old, 0, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(sender.retire(), Ok(2));
    let next = TimedAudio {
        epoch: 2,
        timestamp: 477,
        ..old
    };
    assert_eq!(
        sender.send(next, 10, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(
        sender.send(old, 20, |_| panic!("retired media reached socket")),
        Ok(SendStatus::Expired)
    );
    assert_eq!(
        sender.send(next, 30, |wire| {
            assert_eq!(&wire[2..8], &[0xff, 0xff, 0, 0, 1, 221]);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
}

#[test]
fn socket_failure_reports_only_numeric_errno_and_releases_pending_media() {
    let mut sender = egress();
    let error = sender
        .send(audio(77), 0, |_| {
            Err(std::io::Error::from_raw_os_error(111))
        })
        .unwrap_err();
    assert_eq!(format!("{error:?}"), "Socket(111)");
    assert!(sender.pending.is_none());
    assert_eq!(sender.header.sequence_number, u16::MAX);
}

#[tokio::test(flavor = "current_thread")]
async fn socket_receipt_floor_requires_real_empty_probe_and_never_dequeues_on_cached_readiness() {
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut endpoint = RtpEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        MediaDescription {
            remote: sender.local_addr().unwrap(),
            codec: AudioProfile::Mulaw,
            receive_payload: AudioProfile::Mulaw.payload(),
            send_payload: AudioProfile::Mulaw.payload(),
            telephone_events: [None; 2],
        },
        1,
        1,
    )
    .await
    .unwrap();
    let codec = AudioCodec::load(AudioProfile::Mulaw).unwrap();
    let mut output = [0; 160];
    assert!(
        endpoint
            .try_receive(&mut output, &|packet| codec.inspect(packet))
            .unwrap()
            .is_none()
    );
    let first = endpoint.receipt_floor;
    let mut packet = [0xa5; 172];
    packet[..12].copy_from_slice(&[0x80, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 19]);
    sender
        .send_to(&packet, endpoint.local_addr().unwrap())
        .unwrap();
    // No reactor yield: the kernel has data but Tokio has not observed it.
    assert!(
        endpoint
            .try_receive(&mut output, &|packet| codec.inspect(packet))
            .unwrap()
            .is_none()
    );
    assert_eq!(endpoint.receipt_floor, first);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let (_, receipt) = endpoint
        .try_receive(&mut output, &|packet| codec.inspect(packet))
        .unwrap()
        .unwrap();
    assert_eq!(receipt, first);
    assert_eq!(
        output, [0xa5; 160],
        "the cached-readiness probe was nonconsuming"
    );
    assert!(
        endpoint
            .try_receive(&mut output, &|packet| codec.inspect(packet))
            .unwrap()
            .is_none()
    );
    let drained = endpoint.receipt_floor;
    assert!(drained > first);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert!(
        endpoint
            .try_receive(&mut output, &|packet| codec.inspect(packet))
            .unwrap()
            .is_none()
    );
    let idle = endpoint.receipt_floor;
    assert!(
        idle > drained,
        "real empty probes keep a fresh burst eligible"
    );
    sender
        .send_to(&packet, endpoint.local_addr().unwrap())
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert_eq!(
        endpoint
            .try_receive(&mut output, &|packet| codec.inspect(packet))
            .unwrap()
            .unwrap()
            .1,
        idle
    );
}

#[test]
fn telephone_events_use_one_rtp_owner_and_three_final_reports() {
    for rate in [8000u32, 16000] {
        for (event, symbol) in b"0123456789*#ABCD".iter().copied().enumerate() {
            let mut sender = egress();
            assert_eq!(sender.begin_digit(symbol, 101, rate, 100), 0);
            assert_eq!(sender.begin_digit(b'6', 101, rate, 100), 1);
            let mut wires = Vec::new();
            for now in (100..=320).step_by(20) {
                sender
                    .poll_digit(now, |wire| {
                        wires.push(wire.to_vec());
                        Ok(wire.len())
                    })
                    .unwrap();
            }
            assert_eq!(wires.len(), 12);
            for (index, wire) in wires.iter().enumerate() {
                assert_eq!(wire.len(), 16);
                assert_eq!(wire[1], 101 | if index == 0 { 0x80 } else { 0 });
                assert_eq!(
                    u16::from_be_bytes(wire[2..4].try_into().unwrap()),
                    u16::MAX.wrapping_add(index as u16)
                );
                assert_eq!(&wire[4..8], &(rate / 10).to_be_bytes());
                assert_eq!(&wire[8..12], &0x12345678u32.to_be_bytes());
                assert_eq!(wire[12], event as u8);
                assert_eq!(wire[13], 12 | if index >= 9 { 0x80 } else { 0 });
                assert_eq!(
                    u16::from_be_bytes(wire[14..16].try_into().unwrap()),
                    ((index + 1).min(10) as u32 * (rate / 50)) as u16
                );
            }
            sender
                .poll_digit(369, |_| panic!("gap emitted packet"))
                .unwrap();
            assert_eq!(sender.begin_digit(symbol, 101, rate, 369), 1);
            sender
                .poll_digit(370, |_| panic!("gap emitted packet"))
                .unwrap();
            assert_eq!(sender.begin_digit(symbol, 101, rate, 370), 0);
        }
    }
}

#[test]
fn event_and_audio_keep_shared_sequence_and_original_payload_clock() {
    let mut sender = egress();
    assert_eq!(sender.begin_digit(b'#', 110, 8000, 10), 0);
    sender
        .poll_digit(10, |wire| {
            assert_eq!(wire[1], 0x80 | 110);
            Ok(wire.len())
        })
        .unwrap();
    assert_eq!(
        sender.send(audio(77), 10, |wire| {
            assert_eq!(wire.len(), 172);
            assert_eq!(&wire[..8], &[0x80, 0x80, 0, 0, 0, 0, 0, 77]);
            assert_eq!(&wire[12..], &[0xff; 160]);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
    sender
        .poll_digit(30, |wire| {
            assert_eq!(&wire[..8], &[0x80, 110, 0, 1, 0, 0, 0, 80]);
            Ok(wire.len())
        })
        .unwrap();
}

#[test]
fn blocked_event_has_one_immutable_datagram_across_audio_cut() {
    let mut sender = egress();
    assert_eq!(sender.begin_digit(b'*', 101, 8000, 0), 0);
    let mut original = Vec::new();
    sender
        .poll_digit(0, |wire| {
            original.extend_from_slice(wire);
            Err(std::io::ErrorKind::WouldBlock.into())
        })
        .unwrap();
    assert_eq!(
        sender.send(audio(77), 10, |_| panic!("audio replaced event")),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(sender.retire(), Ok(2));
    sender
        .poll_digit(199, |wire| {
            assert_eq!(wire, original);
            Ok(wire.len())
        })
        .unwrap();
    assert_eq!(sender.header.sequence_number, 0);
    assert_eq!(
        sender.send(audio(77), 199, |_| panic!("old audio survived")),
        Ok(SendStatus::Expired)
    );
    sender
        .poll_digit(200, |_| panic!("late poll emitted catch-up burst"))
        .unwrap();
}

#[test]
fn event_cannot_replace_blocked_audio_or_rejuvenate_deadline() {
    let mut sender = egress();
    assert_eq!(
        sender.send(audio(77), 0, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(sender.begin_digit(b'1', 101, 8000, 0), 0);
    sender
        .poll_digit(10, |_| panic!("event replaced pending audio"))
        .unwrap();
    assert_eq!(
        sender.send(audio(77), 200, |_| panic!("expired audio sent")),
        Ok(SendStatus::Expired)
    );
    sender
        .poll_digit(200, |_| panic!("expired event sent"))
        .unwrap();
    assert!(sender.digit.is_none());
    assert!(sender.pending.is_none());
    assert_eq!(sender.header.sequence_number, u16::MAX);
}

#[test]
fn event_expiry_exhaustion_and_invalid_input_never_emit() {
    let mut sender = egress();
    for (symbol, payload, now) in [
        (b'x', 101, 0),
        (b'1', 0, 0),
        (b'1', 128, 0),
        (b'1', 101, u32::MAX),
    ] {
        assert_eq!(sender.begin_digit(symbol, payload, 8000, now), -1);
        assert!(sender.digit.is_none());
    }
    assert_eq!(sender.begin_digit(b'1', 101, 8000, 0), 0);
    sender
        .poll_digit(0, |_| Err(std::io::ErrorKind::WouldBlock.into()))
        .unwrap();
    sender
        .poll_digit(450, |_| panic!("expired event sent"))
        .unwrap();
    assert!(sender.pending.is_none());
    assert!(sender.digit.is_none());
    assert_eq!(sender.begin_digit(b'1', 101, 8000, 500), 0);
    sender.epoch = u64::MAX;
    assert_eq!(sender.retire(), Err(MediaError::Retired));
    assert!(sender.digit.is_none());
    sender
        .poll_digit(500, |_| panic!("retired event sent"))
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn negotiated_event_uses_existing_udp_socket_and_unsupported_is_refused() {
    let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_nonblocking(true).unwrap();
    let mut endpoint = RtpEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        MediaDescription {
            remote: receiver.local_addr().unwrap(),
            codec: AudioProfile::Alaw,
            receive_payload: AudioProfile::Alaw.payload(),
            send_payload: AudioProfile::Alaw.payload(),
            telephone_events: [Some(TelephoneEvent::symmetric(101, 8000)), None],
        },
        19,
        1,
    )
    .await
    .unwrap();
    let source = endpoint.local_addr().unwrap();
    assert_eq!(endpoint.send_digit(b'5'), 0);
    endpoint.writable().await.unwrap();
    endpoint.poll_digit().unwrap();
    let mut wire = [0; 172];
    let (length, from) = receiver.recv_from(&mut wire).unwrap();
    assert_eq!(length, 16);
    assert_eq!(from, source);
    assert_eq!(wire[1], 0x80 | 101);
    assert_eq!(wire[12], 5);
    endpoint.media.telephone_events = [None; 2];
    assert_eq!(endpoint.send_digit(b'6'), -1);
}

#[test]
fn negotiated_g711_profiles_keep_pcm_and_rtp_clocks_explicit() {
    for (codec, payload, provider) in [(AudioProfile::Mulaw, 0, 1), (AudioProfile::Alaw, 8, 2)] {
        let profile = codec.parameters();
        assert_eq!((profile.pcm_rate_hz, profile.rtp_clock_hz), (8000, 8000));
        assert_eq!(
            (profile.packet_frames, profile.packet_bytes, profile.mode_id),
            (160, 160, 0)
        );
        assert_eq!(profile.provider_profile as u32, provider);
        let source = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 5000 RTP/AVP {payload}\r\n"
        );
        let media = MediaDescription::parse(source.as_bytes()).unwrap();
        assert_eq!(media.codec, codec);
        assert_eq!(
            (media.receive_payload, media.send_payload),
            (payload, payload)
        );
    }
}

#[test]
fn advertised_profiles_have_verified_bidirectional_provider_metadata() {
    let catalog = super::super::codec::load_catalog().unwrap();
    for codec in AudioProfile::ALL {
        let expected = codec.parameters();
        let profile = catalog.profile(expected.provider_profile).unwrap();
        assert_eq!(profile.sample_rate_hz, expected.pcm_rate_hz);
        assert_eq!(profile.lookahead_frames, 0);
        assert!(
            catalog
                .encoder(
                    expected.provider_profile,
                    expected.mode_id,
                    expected.packet_frames,
                    expected.packet_bytes
                )
                .is_ok()
        );
        assert!(
            catalog
                .decoder(
                    expected.provider_profile,
                    expected.mode_id,
                    expected.packet_frames,
                    expected.packet_bytes
                )
                .is_ok()
        );
    }
    assert_eq!(catalog.profiles().count(), AudioProfile::ALL.len());
}

fn event_profile_sdp(payloads: &str, mappings: &str, port: u16) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP {payloads}\r\n{mappings}a=ptime:20\r\n"
    )
}

#[test]
fn incoming_event_answer_selects_the_audio_rtp_clock_not_pcm_rate() {
    for (audio, format, selected) in [(99, "L16/16000", 102), (9, "G722/8000", 101)] {
        let source = event_profile_sdp(
            &format!("{audio} 101 102"),
            &format!(
                "a=rtpmap:{audio} {format}\r\na=rtpmap:101 telephone-event/8000\r\na=rtpmap:102 telephone-event/16000\r\n"
            ),
            42000,
        );
        let media = MediaDescription::parse(source.as_bytes()).unwrap();
        let answer =
            String::from_utf8(media.answer("127.0.0.1:43000".parse().unwrap(), 9).unwrap())
                .unwrap();
        assert!(answer.contains(&format!("m=audio 43000 RTP/AVP {audio} {selected}\r\n")));
        let omitted = if selected == 101 { 102 } else { 101 };
        assert!(!answer.contains(&format!("a=rtpmap:{omitted} telephone-event")));
    }
}

#[test]
fn answer_payload_renumbering_preserves_format_but_reinterpretation_is_rejected() {
    let valid = event_profile_sdp(
        "113 120 121",
        "a=rtpmap:113 L16/16000\r\na=rtpmap:120 telephone-event/8000\r\na=rtpmap:121 telephone-event/16000\r\n",
        42000,
    );
    let media = MediaDescription::parse_answer(valid.as_bytes()).unwrap();
    assert_eq!((media.receive_payload, media.send_payload), (99, 113));
    for invalid in [
        valid.replace("113", "101"), // Offered event PT cannot be reinterpreted as audio.
        valid
            .replace("120 telephone-event/8000", "101 telephone-event/16000")
            .replace("113 120 121", "113 101 121"),
        valid.replace("L16/16000", "Opus/48000/2"),
        valid.replace("telephone-event/8000", "telephone-event/48000"),
    ] {
        assert!(
            MediaDescription::parse_answer(invalid.as_bytes()).is_err(),
            "{invalid}"
        );
    }
}

#[tokio::test]
async fn both_negotiated_receive_event_clocks_are_retained_and_tx_uses_only_matching_clock() {
    for mixed_only in [false, true] {
        let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let payloads = if mixed_only { "113 120" } else { "113 120 121" };
        let maps = format!(
            "a=rtpmap:113 L16/16000\r\na=rtpmap:120 telephone-event/8000\r\n{}",
            if mixed_only {
                ""
            } else {
                "a=rtpmap:121 telephone-event/16000\r\n"
            }
        );
        let sdp = event_profile_sdp(payloads, &maps, peer.local_addr().unwrap().port());
        let media = MediaDescription::parse_answer(sdp.as_bytes()).unwrap();
        assert_eq!(media.digit_capabilities(), if mixed_only { 512 } else { 0 });
        let codec = AudioCodec::load(media.codec).unwrap();
        let mut endpoint = RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), media, 55, 1)
            .await
            .unwrap();
        let local = endpoint.local_addr().unwrap();
        let mut output = [0x5a; 160];
        for pt in [101, 102].into_iter().take(if mixed_only { 1 } else { 2 }) {
            let mut wire = [
                0x80, pt, 0, 1, 0, 0, 0, 123, 0, 0, 0, 77, 5, 0x8c, 0x01, 0x40,
            ];
            for repeat in 0..3 {
                wire[3] = repeat + pt;
                peer.send_to(&wire, local).await.unwrap();
                let event = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    endpoint.receive(&mut output, |packet| codec.inspect(packet)),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(
                    event,
                    if repeat == 0 {
                        RtpEvent::Digit {
                            digit: b'5',
                            ended: true,
                        }
                    } else {
                        RtpEvent::None
                    }
                );
                assert_eq!(output, [0x5a; 160]);
            }
        }
        assert_eq!(endpoint.send_digit(b'5'), if mixed_only { -1 } else { 0 });
        endpoint.poll_digit().unwrap();
        let mut wire = [0; 32];
        if mixed_only {
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(30),
                    peer.recv_from(&mut wire)
                )
                .await
                .is_err()
            );
        } else {
            let (size, _) =
                tokio::time::timeout(std::time::Duration::from_secs(1), peer.recv_from(&mut wire))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(size, 16);
            assert_eq!(wire[1] & 0x7f, 121);
            assert_eq!(&wire[14..16], &320u16.to_be_bytes());
        }
    }
}

#[test]
fn every_packet_size_keeps_one_bounded_immutable_retry_buffer() {
    for profile in AudioProfile::ALL {
        let size = profile.parameters().packet_bytes;
        let mut sender = egress();
        sender.wire = vec![0; size + 12].into_boxed_slice();
        let storage = sender.wire.as_ptr();
        let payload = vec![0x5a; size];
        let request = TimedAudio {
            payload: &payload,
            ..audio(77)
        };
        let mut original = Vec::new();
        assert_eq!(
            sender.send(request, 0, |wire| {
                original.extend_from_slice(wire);
                Err(std::io::ErrorKind::WouldBlock.into())
            }),
            Ok(SendStatus::WouldBlock)
        );
        let wrong = vec![0x5b; size];
        assert_eq!(
            sender.send(
                TimedAudio {
                    payload: &wrong,
                    ..request
                },
                10,
                |_| panic!("replacement reached transport")
            ),
            Err(MediaError::InvalidPacket)
        );
        assert_eq!(
            sender.send(request, 199, |wire| {
                assert_eq!(wire, original);
                Ok(wire.len())
            }),
            Ok(SendStatus::Sent)
        );
        assert_eq!(sender.wire.len(), size + 12);
        assert_eq!(sender.wire.as_ptr(), storage);
        assert_eq!(
            sender.send(
                TimedAudio {
                    payload: &payload[..size - 1],
                    ..request
                },
                199,
                |_| panic!("wrong packet size reached transport")
            ),
            Err(MediaError::InvalidPacket)
        );
    }
}

#[test]
fn audio_encoding_names_are_case_insensitive_without_changing_clock_or_channels() {
    let local = "127.0.0.1:9999".parse().unwrap();
    for codec in AudioProfile::ALL {
        let description = MediaDescription {
            remote: local,
            codec,
            receive_payload: codec.payload(),
            send_payload: codec.payload(),
            telephone_events: [None, None],
        };
        let source = String::from_utf8(description.answer(local, 1).unwrap()).unwrap();
        let lower = source.replace(codec.name(), &codec.name().to_ascii_lowercase());
        for parse in [MediaDescription::parse, MediaDescription::parse_answer] {
            assert_eq!(parse(lower.as_bytes()), Ok(description.clone()));
            let wrong_clock =
                lower.replace(&format!("/{}", codec.parameters().rtp_clock_hz), "/12000");
            assert_eq!(
                parse(wrong_clock.as_bytes()),
                Err(MediaError::UnsupportedDescription)
            );
            let stereo = lower.replace(
                &format!("/{}", codec.parameters().rtp_clock_hz),
                &format!("/{}/2", codec.parameters().rtp_clock_hz),
            );
            assert_eq!(
                parse(stereo.as_bytes()),
                Err(MediaError::UnsupportedDescription)
            );
        }
    }
}

#[test]
fn event_encoding_names_are_case_insensitive_with_exact_directional_clocks() {
    let local = "127.0.0.1:9999".parse().unwrap();
    for codec in [AudioProfile::Mulaw, AudioProfile::L16Wideband] {
        let description = MediaDescription {
            remote: local,
            codec,
            receive_payload: codec.payload(),
            send_payload: codec.payload(),
            telephone_events: [
                Some(TelephoneEvent::symmetric(101, 8000)),
                Some(TelephoneEvent::symmetric(102, 16000)),
            ],
        };
        let source = String::from_utf8(description.answer(local, 1).unwrap()).unwrap();
        for spelling in ["TELEPHONE-EVENT", "Telephone-Event"] {
            let changed = source.replace("telephone-event", spelling);
            for parse in [MediaDescription::parse, MediaDescription::parse_answer] {
                assert_eq!(parse(changed.as_bytes()), parse(source.as_bytes()));
            }
        }
    }
}

#[test]
fn ilbc_offer_advertises_independent_receive_ceiling_without_common_ptime() {
    let offer =
        String::from_utf8(MediaDescription::offer("127.0.0.1:43000".parse().unwrap(), 1).unwrap())
            .unwrap();
    assert!(offer.contains("a=rtpmap:100 iLBC/8000\r\n"));
    assert!(offer.contains("a=fmtp:100 mode=30\r\n"));
    assert!(offer.contains("a=maxptime:30\r\n"));
    assert!(!offer.lines().any(|line| line.starts_with("a=ptime:")));
    for profile in AudioProfile::ALL {
        assert!(offer.contains(&format!(
            "a=rtpmap:{} {}/{}\r\n",
            profile.payload(),
            profile.name(),
            profile.parameters().rtp_clock_hz
        )));
    }
}

#[test]
fn ilbc_answers_keep_mode_thirty_and_directional_payload_identity() {
    for mode in ["", "a=fmtp:110 mode=20\r\n", "a=fmtp:110 mode=30\r\n"] {
        let answer = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 42000 RTP/AVP 110 120\r\na=rtpmap:110 iLBC/8000\r\n{mode}a=rtpmap:120 telephone-event/8000\r\na=fmtp:120 0-15\r\na=ptime:20.5\r\na=maxptime:30\r\n"
        );
        let media = MediaDescription::parse_answer(answer.as_bytes()).unwrap();
        assert_eq!((media.receive_payload, media.send_payload), (100, 110));
        assert_eq!(
            (
                media.codec.parameters().mode_id,
                media.codec.parameters().packet_frames
            ),
            (30, 240)
        );
        assert_eq!(
            media.transmit_event(),
            Some(TelephoneEvent {
                receive_payload: 101,
                send_payload: 120,
                clock_rate_hz: 8000
            })
        );
        assert!(
            MediaDescription::parse_answer(answer.replace("110", "99").as_bytes()).is_err(),
            "an offered L16 PT cannot be reinterpreted as iLBC"
        );
        assert!(
            MediaDescription::parse_answer(answer.replace("iLBC/8000", "iLBC/16000").as_bytes())
                .is_err()
        );
    }
}

#[tokio::test]
async fn native_partial_units_and_inspector_faults_never_adopt_or_overwrite() {
    for profile in [AudioProfile::Gsm, AudioProfile::L16Wideband] {
        let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut codec = AudioCodec::load(profile).unwrap();
        let shape = profile.parameters();
        let mut encoded = vec![0; shape.packet_bytes];
        codec
            .encode(&vec![0.125; shape.packet_frames as usize], &mut encoded)
            .unwrap();
        let mut endpoint = RtpEndpoint::bind(
            "127.0.0.1:0".parse().unwrap(),
            MediaDescription {
                remote: peer.local_addr().unwrap(),
                codec: profile,
                receive_payload: profile.payload(),
                send_payload: profile.payload(),
                telephone_events: [None; 2],
            },
            1,
            0,
        )
        .await
        .unwrap();
        let mut wire = vec![0x80, profile.payload(), 0, 1, 0, 0, 0, 0, 0, 0, 0, 77];
        wire.extend_from_slice(&encoded);
        let mut output = vec![0x55; shape.packet_bytes];
        assert_eq!(
            endpoint.receive_wire(&wire, &mut output, &|_| Err(MediaError::Codec)),
            Err(MediaError::Codec)
        );
        assert_eq!(endpoint.source, None);
        assert!(output.iter().all(|byte| *byte == 0x55));
        assert_eq!(
            endpoint.receive_wire(&wire[..wire.len() - 1], &mut output, &|packet| codec
                .inspect(packet)),
            Err(MediaError::InvalidPacket)
        );
        assert_eq!(endpoint.source, None);
        assert!(output.iter().all(|byte| *byte == 0x55));
        wire[11] = 88;
        assert_eq!(
            endpoint.receive_wire(&wire, &mut output, &|packet| codec.inspect(packet)),
            Ok(RtpEvent::Audio {
                sequence: 1,
                timestamp: 0,
                ssrc: 88,
                length: shape.packet_bytes,
                span: shape.packet_frames,
            })
        );
        assert_eq!(endpoint.source, Some(88));
        assert_eq!(output, encoded);
    }
}
