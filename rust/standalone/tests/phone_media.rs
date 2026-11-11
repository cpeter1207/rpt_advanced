//! Independent wire-format contract for the native phone media boundary.
use rpt_advanced_standalone::phone::media::{G711, MediaDescription, MediaError};
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
async fn rtp_socket_preserves_packet_order_and_wraps_the_wire_clock() {
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
        u32::MAX - 79,
    )
    .await
    .unwrap();
    let local = transport.local_addr().unwrap();
    let mut wire = [0; 2048];
    for (sequence, timestamp) in [(u16::MAX, u32::MAX - 79), (0, 80)] {
        transport.send_audio(&[0xff; 160], true).await.unwrap();
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
    let mut transport = RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), description, 1, 1, 1)
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
            RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), media, 1, 1, 1).await,
            Err(MediaError::UnsupportedDescription)
        ));
    }
}
