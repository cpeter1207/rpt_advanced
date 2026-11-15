//! Exact negotiated packet shapes use the existing socket and source-clock owner.
use rpt_advanced_standalone::phone::media::{
    AudioCodec, MediaDescription, MediaError, RtpEndpoint, RtpEvent, SendStatus, TimedAudio,
};
use std::time::Duration;

#[tokio::test]
async fn fixed_profiles_preserve_packet_shape_directional_payload_and_source_clock() {
    for (payload, format) in [
        (3, "GSM/8000"),
        (110, "G726-32/8000"),
        (111, "AAL2-G726-32/8000"),
        (112, "L16/8000"),
        (113, "L16/16000"),
    ] {
        let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sdp = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP {payload}\r\na=rtpmap:{payload} {format}\r\na=ptime:20\r\n",
            peer.local_addr().unwrap().port()
        );
        let media = MediaDescription::parse(sdp.as_bytes()).unwrap();
        let shape = media.codec.parameters();
        let mut codec = AudioCodec::load(media.codec).unwrap();
        let mut encoded = vec![0; shape.packet_bytes];
        codec
            .encode(&vec![0.125; shape.packet_frames as usize], &mut encoded)
            .unwrap();
        let mut endpoint =
            RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), media, 0x12345678, u16::MAX)
                .await
                .unwrap();
        let local = endpoint.local_addr().unwrap();
        let mut wire = vec![0; shape.packet_bytes + 12];
        for (sequence, timestamp) in [(u16::MAX, u32::MAX - 79), (0, 400)] {
            let request = TimedAudio {
                payload: &encoded,
                marker: true,
                timestamp,
                deadline_ms: endpoint.elapsed_ms().checked_add(200).unwrap(),
                epoch: endpoint.tx_epoch(),
            };
            loop {
                match endpoint.try_send_audio(request).unwrap() {
                    SendStatus::Sent => break,
                    SendStatus::WouldBlock => {
                        tokio::time::timeout(Duration::from_secs(1), endpoint.writable())
                            .await
                            .unwrap()
                            .unwrap()
                    }
                    SendStatus::Expired => panic!("fresh negotiated packet expired"),
                }
            }
            let (size, from) =
                tokio::time::timeout(Duration::from_secs(1), peer.recv_from(&mut wire))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(from, local);
            assert_eq!(size, shape.packet_bytes + 12);
            assert_eq!(wire[1], 0x80 | payload);
            assert_eq!(&wire[2..4], &sequence.to_be_bytes());
            assert_eq!(&wire[4..8], &timestamp.to_be_bytes());
            assert_eq!(&wire[8..12], &0x12345678u32.to_be_bytes());
            assert_eq!(&wire[12..], &encoded);
        }
        // A malformed size cannot admit a source or overwrite the caller's PCM packet.
        let mut received = vec![0x5a; shape.packet_bytes];
        wire[8..12].copy_from_slice(&77u32.to_be_bytes());
        peer.send_to(&wire[..wire.len() - 1], local).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), endpoint.receive(&mut received))
                .await
                .unwrap(),
            Err(MediaError::InvalidPacket)
        );
        assert!(received.iter().all(|byte| *byte == 0x5a));
        wire[8..12].copy_from_slice(&88u32.to_be_bytes());
        peer.send_to(&wire, local).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), endpoint.receive(&mut received))
                .await
                .unwrap()
                .unwrap(),
            RtpEvent::Audio {
                sequence: 0,
                timestamp: 400,
                ssrc: 88
            }
        );
        assert_eq!(received, encoded);
        let wrong = vec![0; shape.packet_bytes + 1];
        assert_eq!(
            endpoint.try_send_audio(TimedAudio {
                payload: &wrong,
                marker: false,
                timestamp: 401,
                deadline_ms: endpoint.elapsed_ms().checked_add(200).unwrap(),
                epoch: endpoint.tx_epoch()
            }),
            Err(MediaError::InvalidPacket)
        );
    }
}
