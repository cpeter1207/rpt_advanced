//! Native-unit receive bounds are independent of the fixed transmit packet size.
use rpt_advanced_standalone::phone::media::DecodeProvenance;
use rpt_advanced_standalone::phone::media::{
    AudioCodec, AudioProfile, MediaDescription, MediaError, RtpEndpoint, RtpEvent,
};
use std::time::Duration;

#[test]
fn bounded_receive_accepts_complete_native_units_through_thirty_ms() {
    for profile in AudioProfile::ALL {
        let p = profile.parameters();
        let mut codec = AudioCodec::load(profile).unwrap();
        let mut encoded = vec![0; p.packet_bytes];
        codec
            .encode(&vec![0.125; p.packet_frames as usize], &mut encoded)
            .unwrap();
        let tx_ms = p.packet_frames as usize * 1000 / p.pcm_rate_hz as usize;
        let durations: &[usize] = if profile == AudioProfile::Gsm {
            &[20]
        } else if tx_ms == 30 {
            &[30]
        } else {
            &[10, 20, 30]
        };
        for &ms in durations {
            let frames = p.pcm_rate_hz as usize * ms / 1000;
            let length = p.packet_bytes * ms / tx_ms;
            let repeated = encoded.repeat(2);
            let mut pcm = vec![0.0; frames];
            assert_eq!(
                codec
                    .decode(&repeated[..length], &mut pcm)
                    .unwrap_or_else(|error| panic!("{profile:?}/{ms}ms: {error:?}")),
                DecodeProvenance::Original
            );
            assert!(pcm.iter().all(|sample| sample.is_finite()));
        }
        assert!(
            codec
                .decode(
                    &encoded.repeat(2),
                    &mut vec![0.0; p.packet_frames as usize * 2]
                )
                .is_err()
        );
        assert!(codec.decode(&[], &mut []).is_err());
    }
}

#[test]
fn gsm_receive_limit_remains_one_complete_twenty_ms_unit() {
    let mut codec = AudioCodec::load(AudioProfile::Gsm).unwrap();
    let mut packet = [0; 33];
    codec.encode(&[0.125; 160], &mut packet).unwrap();
    assert_eq!(
        codec.decode(&packet, &mut [0.0; 160]).unwrap(),
        DecodeProvenance::Original
    );
    for bytes in [0, 1, 32, 34, 49, 66] {
        assert!(codec.decode(&vec![0; bytes], &mut [0.0; 240]).is_err());
    }
}

#[tokio::test]
async fn bounded_rtp_receive_does_not_latch_an_invalid_source_or_overwrite_output() {
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut endpoint = RtpEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        MediaDescription {
            remote: peer.local_addr().unwrap(),
            codec: AudioProfile::Mulaw,
            receive_payload: 0,
            send_payload: 0,
            telephone_events: [None; 2],
        },
        123,
        0,
    )
    .await
    .unwrap();
    let target = endpoint.local_addr().unwrap();
    let codec = AudioCodec::load(AudioProfile::Mulaw).unwrap();
    let mut output = [0x55; 240];
    for (index, length) in [241, 80, 240].into_iter().enumerate() {
        let ssrc: u32 = if index == 0 { 77 } else { 88 };
        let mut wire = vec![0x80, 0, 0, index as u8, 0, 0, 0, 0];
        wire.extend_from_slice(&ssrc.to_be_bytes());
        wire.extend(std::iter::repeat_n(0xa5, length));
        peer.send_to(&wire, target).await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            endpoint.receive(&mut output, |packet| codec.inspect(packet)),
        )
        .await
        .unwrap();
        if index == 0 {
            assert_eq!(result, Err(MediaError::InvalidPacket));
            assert_eq!(output, [0x55; 240]);
        } else {
            assert!(
                matches!(result, Ok(RtpEvent::Audio { ssrc: 88, length: actual, span, .. }) if actual == length && span as usize == length),
                "{length}: {result:?}"
            );
            assert!(output[..length].iter().all(|value| *value == 0xa5));
            assert!(output[length..].iter().all(|value| *value == 0x55));
        }
    }
}
