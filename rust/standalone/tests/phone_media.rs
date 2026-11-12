//! Independent wire-format contract for the native phone media boundary.
use rpt_advanced_standalone::phone::media::{
    G711, MediaDescription, MediaError, SendStatus, TimedAudio,
};
use std::net::SocketAddr;

fn offer(payload: &str, attributes: &str) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 42000 RTP/AVP {payload}\r\n{attributes}"
    )
}

#[test]
fn selects_g711_and_only_explicit_8000hz_telephone_events() {
    let source = offer(
        "8 0 101",
        "a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\n",
    );
    let media = MediaDescription::parse(source.as_bytes()).unwrap();
    assert_eq!(media.codec, G711::Alaw);
    assert_eq!(
        media.remote,
        "127.0.0.1:42000".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(media.telephone_event, Some(101));
    let answer = media.answer("127.0.0.1:43000".parse().unwrap(), 7).unwrap();
    let answer = std::str::from_utf8(&answer).unwrap();
    assert!(answer.contains("m=audio 43000 RTP/AVP 8 101\r\n"));
    assert!(answer.contains("a=rtpmap:8 PCMA/8000\r\n"));
    assert!(answer.contains("a=ptime:20\r\n"));
}

#[test]
fn rejects_unsupported_or_ambiguous_media_instead_of_downgrading() {
    let valid = offer("0", "a=ptime:20\r\n");
    let cases = [
        valid.replace("RTP/AVP", "RTP/SAVP"),
        valid.replace("42000", "0"),
        valid.replace("a=ptime:20", "a=ptime:30"),
        valid.replace("a=ptime:20", "a=rtpmap:0 PCMU/16000"),
        valid.replace("a=ptime:20", "a=sendonly"),
        valid.replace(
            "a=ptime:20",
            "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:secret",
        ),
        valid.replace("a=ptime:20", "a=rtcp-mux"),
        valid.replace("RTP/AVP 0", "RTP/AVP 111"),
        valid.clone() + "m=audio 44000 RTP/AVP 8\r\n",
        "not SDP".into(),
    ];
    for candidate in cases {
        assert_eq!(
            MediaDescription::parse(candidate.as_bytes()),
            Err(MediaError::UnsupportedDescription)
        );
    }
}

#[test]
fn session_attributes_and_media_connection_are_honored() {
    let source = offer(
        "0 101",
        "c=IN IP4 127.0.0.2\r\na=rtpmap:101 telephone-event/16000\r\n",
    );
    let media = MediaDescription::parse(source.as_bytes()).unwrap();
    assert_eq!(
        media.remote,
        "127.0.0.2:42000".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(media.telephone_event, None);
    assert!(
        MediaDescription::parse(
            offer("0", "")
                .replace("t=0 0", "a=inactive\r\nt=0 0")
                .as_bytes()
        )
        .is_err()
    );
}

#[tokio::test]
async fn rtp_socket_preserves_packet_order_and_original_source_clock() {
    use rpt_advanced_standalone::phone::media::{RtpEndpoint, RtpEvent};
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let description = MediaDescription {
        remote: peer.local_addr().unwrap(),
        codec: G711::Mulaw,
        telephone_event: Some(101),
    };
    let mut transport = RtpEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        description,
        0x12345678,
        u16::MAX,
    )
    .await
    .unwrap();
    let local = transport.local_addr().unwrap();
    let mut wire = [0; 2048];
    for (sequence, timestamp) in [(u16::MAX, u32::MAX - 79), (0, 400)] {
        let request = TimedAudio {
            payload: &[0xff; 160],
            marker: true,
            timestamp,
            deadline_ms: transport.elapsed_ms().checked_add(200).unwrap(),
            epoch: transport.tx_epoch(),
        };
        loop {
            match transport.try_send_audio(request).unwrap() {
                SendStatus::Sent => break,
                SendStatus::WouldBlock => {
                    assert!(transport.wants_writable());
                    tokio::time::timeout(Duration::from_secs(1), transport.writable())
                        .await
                        .unwrap()
                        .unwrap();
                }
                SendStatus::Expired => panic!("loopback missed its source deadline"),
            }
        }
        assert!(!transport.wants_writable());
        let (length, source) = peer.recv_from(&mut wire).await.unwrap();
        assert_eq!(source, local);
        assert_eq!(length, 172);
        assert_eq!(&wire[..2], &[0x80, 0x80]);
        assert_eq!(u16::from_be_bytes([wire[2], wire[3]]), sequence);
        assert_eq!(
            u32::from_be_bytes(wire[4..8].try_into().unwrap()),
            timestamp
        );
    }
    // Arrival order must remain intact for the existing packet-jitter owner.
    for sequence in [4u16, 2, 3] {
        wire[..12].copy_from_slice(&[0x80, 0, 0, 0, 0, 0, 0, 160, 0, 0, 0, 7]);
        wire[2..4].copy_from_slice(&sequence.to_be_bytes());
        wire[12..172].fill(0xff);
        peer.send_to(&wire[..172], local).await.unwrap();
        let mut payload = [0; 160];
        assert_eq!(
            transport.receive(&mut payload).await.unwrap(),
            RtpEvent::Audio {
                sequence,
                timestamp: 160,
                ssrc: 7
            }
        );
        assert_eq!(payload, [0xff; 160]);
    }
}

#[tokio::test]
async fn telephone_events_are_bounded_and_end_repetitions_are_deduplicated() {
    use rpt_advanced_standalone::phone::media::{RtpEndpoint, RtpEvent};
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let description = MediaDescription {
        remote: peer.local_addr().unwrap(),
        codec: G711::Alaw,
        telephone_event: Some(101),
    };
    let mut transport = RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), description, 1, 1)
        .await
        .unwrap();
    let local = transport.local_addr().unwrap();
    for (index, end, expected) in [
        (
            0,
            false,
            RtpEvent::Digit {
                digit: b'5',
                ended: false,
            },
        ),
        (
            1,
            true,
            RtpEvent::Digit {
                digit: b'5',
                ended: true,
            },
        ),
        (2, true, RtpEvent::None),
        (3, true, RtpEvent::None),
    ] {
        let packet = [
            0x80,
            101,
            0,
            index,
            0,
            0,
            0,
            90,
            0,
            0,
            0,
            7,
            5,
            if end { 0x80 | 10 } else { 10 },
            0,
            160,
        ];
        peer.send_to(&packet, local).await.unwrap();
        assert_eq!(transport.receive(&mut [0; 160]).await.unwrap(), expected);
    }
    // Unknown event and wrong codec payload fail closed, not as speech.
    for payload in [
        vec![0x80, 101, 0, 4, 0, 0, 0, 91, 0, 0, 0, 7, 99, 0x80, 0, 160],
        vec![0x80, 0, 0, 4, 0, 0, 0, 91, 0, 0, 0, 7, 0xff],
    ] {
        peer.send_to(&payload, local).await.unwrap();
        assert_eq!(
            transport.receive(&mut [0; 160]).await,
            Err(MediaError::InvalidPacket)
        );
    }
}

#[test]
fn g711_uses_the_released_dynamic_provider_in_both_directions() {
    use rpt_advanced_standalone::phone::media::G711Codec;
    for (format, silence) in [(G711::Mulaw, 0xff), (G711::Alaw, 0xd5)] {
        let mut codec = G711Codec::load(format).unwrap();
        let mut packet = [0; 160];
        codec.encode(&[0.0; 160], &mut packet).unwrap();
        assert_eq!(packet, [silence; 160]);
        let mut decoded = [1.0; 160];
        codec.decode(&packet, &mut decoded).unwrap();
        assert!(decoded.iter().all(|sample| sample.abs() < 0.001));
    }
}

#[test]
fn packet_time_and_telephone_event_ranges_are_negotiated_not_assumed() {
    assert!(MediaDescription::parse(offer("0", "a=maxptime:10\r\n").as_bytes()).is_err());
    assert!(MediaDescription::parse(offer("0", "a=rtpmap:0\r\n").as_bytes()).is_err());
    let partial = offer(
        "0 101",
        "a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 16\r\n",
    );
    assert_eq!(
        MediaDescription::parse(partial.as_bytes())
            .unwrap()
            .telephone_event,
        None
    );
}

#[tokio::test]
async fn constructing_media_cannot_bypass_payload_and_endpoint_validation() {
    use rpt_advanced_standalone::phone::media::RtpEndpoint;
    for (remote, event) in [
        ("127.0.0.1:0", None),
        ("0.0.0.0:42000", None),
        ("239.1.1.1:42000", None),
        ("127.0.0.1:42000", Some(0)),
    ] {
        let media = MediaDescription {
            remote: remote.parse().unwrap(),
            codec: G711::Mulaw,
            telephone_event: event,
        };
        assert!(matches!(
            RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), media, 1, 1).await,
            Err(MediaError::UnsupportedDescription)
        ));
    }
}

use rpt_advanced_standalone::phone::media::{RtpEndpoint, RtpEvent};
use std::time::Duration;

#[test]
fn sdp_size_media_and_connection_boundaries_fail_closed() {
    let valid = offer("0", "");
    let at_limit = valid.replace(
        "s=fixture",
        &format!("s=fixture{}", "x".repeat(16_384 - valid.len())),
    );
    assert!(MediaDescription::parse(at_limit.as_bytes()).is_ok());
    assert_eq!(
        MediaDescription::parse(format!("{at_limit}x").as_bytes()),
        Err(MediaError::UnsupportedDescription)
    );
    for candidate in [
        valid.replace("m=audio", "m=video"),
        valid.replace("42000", "42000/2"),
        valid.replace("m=audio", "k=clear:fixture\r\nm=audio"),
        valid.clone() + "k=clear:fixture\r\n",
        valid.replace("c=IN IP4 127.0.0.1", "c=AT IP4 127.0.0.1"),
        valid.replace("c=IN IP4 127.0.0.1", "c=IN IP4 127.0.0.1/1"),
        valid.replace("c=IN IP4 127.0.0.1", "c=IN IP4 127.0.0.1/1/2"),
        valid.replace("c=IN IP4 127.0.0.1", "c=IN IP4 0.0.0.0"),
        valid.replace("c=IN IP4 127.0.0.1", "c=IN IP4 239.1.1.1"),
        valid.replace("c=IN IP4 127.0.0.1", "c=IN IP6 127.0.0.1"),
        valid.replace("c=IN IP4 127.0.0.1", "c=IN IP4 ::1"),
    ] {
        assert_eq!(
            MediaDescription::parse(candidate.as_bytes()),
            Err(MediaError::UnsupportedDescription),
            "{candidate}"
        );
    }
}

#[test]
fn g711_mapping_and_ipv6_answers_preserve_the_selected_profile() {
    for (payload, name, codec) in [("0", "PCMU", G711::Mulaw), ("8", "PCMA", G711::Alaw)] {
        for channels in ["", "/1"] {
            let source = offer(
                payload,
                &format!("a=rtpmap:{payload} {name}/8000{channels}\r\n"),
            )
            .replace("c=IN IP4 127.0.0.1", "c=IN IP6 ::1");
            let media = MediaDescription::parse(source.as_bytes()).unwrap();
            assert_eq!(media.codec, codec);
            assert_eq!(media.remote, "[::1]:42000".parse().unwrap());
            let answer = media.answer("[::1]:43000".parse().unwrap(), 9).unwrap();
            let answered = MediaDescription::parse(&answer).unwrap();
            assert_eq!(answered.codec, codec);
            assert_eq!(answered.remote, "[::1]:43000".parse().unwrap());
            assert_eq!(answered.telephone_event, None);
            for local in [
                "0.0.0.0:43000",
                "239.1.1.1:43000",
                "127.0.0.1:0",
                "[::]:43000",
                "[ff02::1]:43000",
            ] {
                assert_eq!(
                    media.answer(local.parse().unwrap(), 9),
                    Err(MediaError::UnsupportedDescription)
                );
            }
        }
    }
}

#[test]
fn telephone_event_mappings_require_offered_dynamic_payloads_and_valid_ranges() {
    for (payloads, mapping, range) in [
        ("0 95", "95", "0-15"),
        ("0 128", "128", "0-15"),
        ("0", "101", "0-15"),
        ("0 101", "101", "bad-15"),
        ("0 101", "101", "0-bad"),
        ("0 101", "101", "15-0"),
    ] {
        let source = offer(
            payloads,
            &format!("a=rtpmap:{mapping} telephone-event/8000\r\na=fmtp:{mapping} {range}\r\n"),
        );
        let media = MediaDescription::parse(source.as_bytes()).unwrap();
        assert_eq!(media.codec, G711::Mulaw);
        assert_eq!(media.telephone_event, None, "{source}");
    }
    let media = MediaDescription::parse(
        offer(
            "0 101",
            "a=rtpmap:101 telephone-event/8000/1\r\na=fmtp:101 0-7,8-15\r\n",
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(media.telephone_event, Some(101));
}

async fn media_pair() -> (tokio::net::UdpSocket, RtpEndpoint) {
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let transport = RtpEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        MediaDescription {
            remote: peer.local_addr().unwrap(),
            codec: G711::Mulaw,
            telephone_event: Some(101),
        },
        1,
        1,
    )
    .await
    .unwrap();
    (peer, transport)
}

fn packet(payload_type: u8, timestamp: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
    let mut wire = vec![0x80, payload_type, 0, 1];
    wire.extend_from_slice(&timestamp.to_be_bytes());
    wire.extend_from_slice(&ssrc.to_be_bytes());
    wire.extend_from_slice(payload);
    wire
}

async fn receive_packet(
    peer: &tokio::net::UdpSocket,
    transport: &mut RtpEndpoint,
    wire: &[u8],
    output: &mut [u8; 160],
) -> Result<RtpEvent, MediaError> {
    peer.send_to(wire, transport.local_addr().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), transport.receive(output))
        .await
        .unwrap()
}

#[tokio::test]
async fn rtp_rejects_bad_framing_without_accepting_a_source_or_overwriting_audio() {
    let mut wrong_version = packet(0, 160, 7, &[0xff; 160]);
    wrong_version[0] = 0x40;
    let mut invalid = vec![
        vec![0; 2048],
        vec![0; 2049],
        wrong_version,
        packet(0, 160, 7, &[0xff; 159]),
        packet(0, 160, 7, &[0xff; 161]),
    ];
    for payload in [vec![], vec![0], vec![2]] {
        let mut wire = packet(0, 160, 7, &payload);
        wire[0] |= 0x20;
        invalid.push(wire);
    }
    for wire in invalid {
        let (peer, mut transport) = media_pair().await;
        let mut output = [0x55; 160];
        assert_eq!(
            receive_packet(&peer, &mut transport, &wire, &mut output).await,
            Err(MediaError::InvalidPacket)
        );
        assert_eq!(output, [0x55; 160]);
        assert_eq!(
            receive_packet(
                &peer,
                &mut transport,
                &packet(0, 320, 8, &[0xff; 160]),
                &mut output
            )
            .await,
            Ok(RtpEvent::Audio {
                sequence: 1,
                timestamp: 320,
                ssrc: 8
            })
        );
        assert_eq!(output, [0xff; 160]);
    }
}

#[tokio::test]
async fn rtp_strips_valid_padding_and_keeps_the_first_valid_source() {
    let (peer, mut transport) = media_pair().await;
    let mut output = [0; 160];
    for padding in [1, 4] {
        let mut payload = vec![0xd5; 160];
        payload.extend(std::iter::repeat_n(padding, usize::from(padding)));
        let mut wire = packet(0, 160, 7, &payload);
        wire[0] |= 0x20;
        assert_eq!(
            receive_packet(&peer, &mut transport, &wire, &mut output).await,
            Ok(RtpEvent::Audio {
                sequence: 1,
                timestamp: 160,
                ssrc: 7
            })
        );
        assert_eq!(output, [0xd5; 160]);
    }
    assert_eq!(
        receive_packet(
            &peer,
            &mut transport,
            &packet(0, 320, 8, &[0xff; 160]),
            &mut output
        )
        .await,
        Err(MediaError::InvalidPacket)
    );
    assert_eq!(output, [0xd5; 160]);
    assert_eq!(
        receive_packet(
            &peer,
            &mut transport,
            &packet(0, 480, 7, &[0xff; 160]),
            &mut output
        )
        .await,
        Ok(RtpEvent::Audio {
            sequence: 1,
            timestamp: 480,
            ssrc: 7
        })
    );
    assert_eq!(output, [0xff; 160]);
}

#[tokio::test]
async fn telephone_events_reject_malformed_updates_and_deduplicate_only_the_same_identity() {
    let (peer, mut transport) = media_pair().await;
    let mut output = [0x55; 160];
    for payload in [
        &[5, 0, 160][..],
        &[5, 0, 0, 160, 0],
        &[5, 0x40, 0, 160],
        &[5, 0, 0, 0],
    ] {
        assert_eq!(
            receive_packet(
                &peer,
                &mut transport,
                &packet(101, 90, 7, payload),
                &mut output
            )
            .await,
            Err(MediaError::InvalidPacket)
        );
    }
    for (timestamp, event, end, expected) in [
        (
            90,
            5,
            false,
            RtpEvent::Digit {
                digit: b'5',
                ended: false,
            },
        ),
        (90, 5, false, RtpEvent::None),
        (
            90,
            5,
            true,
            RtpEvent::Digit {
                digit: b'5',
                ended: true,
            },
        ),
        (90, 5, true, RtpEvent::None),
        (
            91,
            5,
            false,
            RtpEvent::Digit {
                digit: b'5',
                ended: false,
            },
        ),
        (
            91,
            6,
            true,
            RtpEvent::Digit {
                digit: b'6',
                ended: true,
            },
        ),
        (91, 6, false, RtpEvent::None),
    ] {
        let payload = [event, if end { 0x80 } else { 0 }, 0, 160];
        assert_eq!(
            receive_packet(
                &peer,
                &mut transport,
                &packet(101, timestamp, 7, &payload),
                &mut output
            )
            .await,
            Ok(expected)
        );
        assert_eq!(output, [0x55; 160]);
    }
}
