//! Negotiated mode30 and directional packet-time limits for the released iLBC provider.
use rpt_advanced_standalone::phone::media::{
    AudioCodec, AudioProfile, MediaDescription, MediaError,
};

fn offer(payloads: &str, attributes: &str) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 42000 RTP/AVP {payloads}\r\n{attributes}"
    )
}

#[test]
fn ilbc_resolves_explicit_twenty_thirty_and_absent_mode_to_thirty() {
    for mode in ["", "a=fmtp:100 mode=20\r\n", "a=fmtp:100 mode=30\r\n"] {
        let sdp = offer(
            "100",
            &format!("a=rtpmap:100 iLBC/8000\r\n{mode}a=ptime:20\r\na=maxptime:30\r\n"),
        );
        let media =
            MediaDescription::parse(sdp.as_bytes()).expect("mode30 is the common iLBC mode");
        let p = media.codec.parameters();
        assert_eq!((p.provider_profile as u32, p.mode_id), (11, 30));
        assert_eq!(
            (
                p.pcm_rate_hz,
                p.rtp_clock_hz,
                p.packet_frames,
                p.packet_bytes
            ),
            (8000, 8000, 240, 50)
        );
        assert_eq!((media.receive_payload, media.send_payload), (100, 100));
        let answer =
            String::from_utf8(media.answer("127.0.0.1:43000".parse().unwrap(), 7).unwrap())
                .unwrap();
        for expected in [
            "a=rtpmap:100 iLBC/8000\r\n",
            "a=fmtp:100 mode=30\r\n",
            "a=ptime:30\r\n",
            "a=maxptime:30\r\n",
        ] {
            assert!(answer.contains(expected), "missing {expected:?}");
        }
        let mut codec = AudioCodec::load(media.codec).unwrap();
        let mut packet = [0; 50];
        codec.encode(&[0.125; 240], &mut packet).unwrap();
        let mut pcm = [0.0; 240];
        codec.decode(&packet, &mut pcm).unwrap();
        assert!(pcm.iter().all(|sample| sample.is_finite()));
        assert!(codec.decode(&packet[..49], &mut pcm).is_err());
        assert!(
            codec.decode(&packet.repeat(2), &mut [0.0; 480]).is_err(),
            "60ms exceeds our advertised ceiling"
        );
    }
}

#[test]
fn ilbc_peer_maximum_is_a_transmit_eligibility_limit_not_our_receive_limit() {
    for maximum in ["20", "20.5", "29.999"] {
        let attributes =
            format!("a=rtpmap:100 iLBC/8000\r\na=fmtp:100 mode=30\r\na=maxptime:{maximum}\r\n");
        let fallback = MediaDescription::parse(offer("100 0", &attributes).as_bytes()).unwrap();
        assert_eq!(fallback.codec, AudioProfile::Mulaw);
        let answer = String::from_utf8(
            fallback
                .answer("127.0.0.1:43000".parse().unwrap(), 7)
                .unwrap(),
        )
        .unwrap();
        assert!(
            answer.contains("a=maxptime:30\r\n"),
            "peer's receive ceiling cannot shrink ours"
        );
        assert_eq!(
            MediaDescription::parse(offer("100", &attributes).as_bytes()),
            Err(MediaError::UnsupportedDescription)
        );
    }
    let attributes = "a=rtpmap:100 iLBC/8000\r\na=fmtp:100 mode=30\r\na=maxptime:30.5\r\n";
    assert_eq!(
        MediaDescription::parse(offer("100 0", attributes).as_bytes())
            .unwrap()
            .codec
            .parameters()
            .mode_id,
        30
    );
}

#[test]
fn packet_time_preferences_accept_valid_fractional_spelling_without_changing_fixed_tx() {
    for preference in ["10", "30", "20.5", "0.5", "20.0"] {
        let sdp = offer("0", &format!("a=ptime:{preference}\r\na=maxptime:30\r\n"));
        let media = MediaDescription::parse(sdp.as_bytes()).unwrap();
        assert_eq!(media.codec.parameters().packet_frames, 160);
    }
    for invalid in [
        "0",
        "0.0",
        "-20",
        "20e0",
        "NaN",
        ".5",
        "20.",
        "999999999999999999999999x",
    ] {
        assert!(
            MediaDescription::parse(offer("0", &format!("a=ptime:{invalid}\r\n")).as_bytes())
                .is_err()
        );
    }
    let large = offer("0", "a=maxptime:999999999999999999999999\r\n");
    assert!(MediaDescription::parse(large.as_bytes()).is_ok());
    let junk = large.replace("999999999999999999999999", "999999999999999999999999x");
    assert!(MediaDescription::parse(junk.as_bytes()).is_err());
    for maximum in ["19", "19.999"] {
        assert!(
            MediaDescription::parse(offer("0", &format!("a=maxptime:{maximum}\r\n")).as_bytes())
                .is_err()
        );
    }
}
