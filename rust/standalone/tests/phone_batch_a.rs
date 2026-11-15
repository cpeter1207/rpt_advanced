//! Batch A wire contracts; the qualified G.722 checkpoint is a separate branch.
use rpt_advanced_standalone::phone::media::{AudioCodec, MediaDescription};

fn description(payload: u8, format: &str) -> MediaDescription {
    let sdp = format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 42000 RTP/AVP {payload}\r\na=rtpmap:{payload} {format}\r\na=ptime:20\r\n"
    );
    MediaDescription::parse(sdp.as_bytes()).expect("verified Batch A wire profile")
}

fn profile(payload: u8, format: &str, id: u32, rate: u32, frames: u32, bytes: usize) {
    let media = description(payload, format);
    let parameters = media.codec.parameters();
    assert_eq!(parameters.provider_profile as u32, id);
    assert_eq!(parameters.mode_id, 0);
    assert_eq!(parameters.pcm_rate_hz, rate);
    assert_eq!(parameters.rtp_clock_hz, rate);
    assert_eq!(parameters.packet_frames, frames);
    assert_eq!(parameters.packet_bytes, bytes);
    assert_eq!(media.receive_payload, payload);
    assert_eq!(media.send_payload, payload);
    assert_eq!(media.telephone_event, None);
    let answer = media.answer("127.0.0.1:43000".parse().unwrap(), 7).unwrap();
    let answer = String::from_utf8(answer).unwrap();
    assert!(answer.contains(&format!("m=audio 43000 RTP/AVP {payload}\r\n")));
    assert!(answer.contains(&format!("a=rtpmap:{payload} {format}\r\n")));
}

#[test]
fn gsm_uses_standard_full_rate_33_byte_packets() {
    profile(3, "GSM/8000", 3, 8000, 160, 33);
}

#[test]
fn g726_standard_uses_its_distinct_dynamic_mapping() {
    profile(110, "G726-32/8000", 12, 8000, 160, 80);
}

#[test]
fn g726_aal2_uses_its_distinct_dynamic_mapping() {
    profile(111, "AAL2-G726-32/8000", 5, 8000, 160, 80);
}

#[test]
fn l16_narrowband_has_two_encoded_bytes_per_pcm_frame() {
    profile(112, "L16/8000", 7, 8000, 160, 320);
}

#[test]
fn l16_wideband_uses_16k_rtp_clock_and_640_byte_packets() {
    profile(113, "L16/16000", 14, 16000, 320, 640);
}

#[test]
fn both_l16_rates_are_network_endian_on_the_wire() {
    for (payload, rate) in [(112, 8000), (113, 16000)] {
        let media = description(payload, &format!("L16/{rate}"));
        let frames = media.codec.parameters().packet_frames as usize;
        let words = [8192_i16, -16384, 0, 32767, -32768];
        let pcm: Vec<_> = (0..frames)
            .map(|i| f32::from(words[i % words.len()]) / 32768.0)
            .collect();
        let expected: Vec<_> = (0..frames)
            .flat_map(|i| words[i % words.len()].to_be_bytes())
            .collect();
        let mut codec = AudioCodec::load(media.codec).unwrap();
        let mut packet = vec![0; frames * 2];
        codec.encode(&pcm, &mut packet).unwrap();
        assert_eq!(packet, expected);
        let mut decoded = vec![0.0; frames];
        codec.decode(&expected, &mut decoded).unwrap();
        assert_eq!(decoded, pcm);
    }
}

#[test]
fn g726_wire_variants_differ_only_in_nibble_packing() {
    let standard = description(110, "G726-32/8000");
    let aal2 = description(111, "AAL2-G726-32/8000");
    let pcm: Vec<_> = (0..160).map(|i| (i as f32 * 0.31).sin() * 0.2).collect();
    let mut standard_codec = AudioCodec::load(standard.codec).unwrap();
    let mut aal2_codec = AudioCodec::load(aal2.codec).unwrap();
    let mut standard_packet = [0; 80];
    let mut aal2_packet = [0; 80];
    standard_codec.encode(&pcm, &mut standard_packet).unwrap();
    aal2_codec.encode(&pcm, &mut aal2_packet).unwrap();
    assert!(standard_packet.iter().any(|value| value >> 4 != value & 15));
    for (&standard, &aal2) in standard_packet.iter().zip(&aal2_packet) {
        assert_eq!(standard, aal2.rotate_left(4));
    }
    let mut standard_pcm = [0.0; 160];
    let mut aal2_pcm = [0.0; 160];
    standard_codec
        .decode(&standard_packet, &mut standard_pcm)
        .unwrap();
    aal2_codec.decode(&aal2_packet, &mut aal2_pcm).unwrap();
    assert_eq!(standard_pcm, aal2_pcm);
}

#[test]
fn ignored_unknown_format_does_not_hide_a_reinterpreted_static_payload() {
    let source = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 42000 RTP/AVP 0 128\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:128 telephone-event/8000\r\n";
    let media = MediaDescription::parse(source.as_bytes()).unwrap();
    assert_eq!(media.receive_payload, 0);
    assert_eq!(media.telephone_event, None);
    assert!(MediaDescription::parse(source.replace("PCMU/8000", "L16/16000").as_bytes()).is_err());
}
