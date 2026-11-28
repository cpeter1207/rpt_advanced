use super::*;
use crate::link::ring::{InboundPolicy, InboundRing};
use crate::services::ProcessedPeer;
use rpt_advanced_core::{
    audio::LinkAudioQueue,
    config::resources::profiles::VadMode,
    controller::{ControllerSettings, CourtesySettings, NodeController},
    link::{
        AudioPeer, LinkAudio, Mode, PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource,
        SourcePosition,
    },
};

// Decode the existing independent telephone caller's one-second, 8 kHz PCMU fixture.
fn pcmu_sample(byte: u8) -> f32 {
    let code = !byte;
    let magnitude = ((132 + 8 * i32::from(code & 15)) << ((code >> 4) & 7)) - 132;
    let signed = if code & 128 == 0 {
        magnitude
    } else {
        -magnitude
    };
    signed as f32 / 32768.0
}

#[test]
fn native_phone_voice_releases_during_healthy_silence_before_station_hang_ends() {
    let speech = include_bytes!("../configuration_transaction/tests/http/review/speech-yes.ulaw");
    let source = PhoneSource::new(
        PhoneAccountId::new("fixture").unwrap(),
        1,
        PhoneCallId::new([3; 16]).unwrap(),
        1,
        1,
    )
    .unwrap();
    let activity = Arc::new(PeerSignals::for_phone(source.clone()));
    assert!(activity.authenticate_phone());
    let reply = activity.reply_target();
    let (writer, reader) = InboundRing::redirected(8000, InboundPolicy::Phone, &reply).unwrap();
    let (output, egress) = LinkAudioQueue::new(4096, 8).unwrap().into_endpoints();
    let confirmed = Arc::new(AtomicBool::new(true));
    let slot = AudioPeer::for_phone(
        &source,
        &reply,
        Mode::phone(false),
        ProcessedPeer::new(reader),
        output,
        960,
    )
    .unwrap()
    .with_phone_policy(false, false, Arc::clone(&confirmed));
    let (mut graph, _dispatch) = LinkAudio::new(vec![slot], 960).unwrap();
    let settings = ControllerSettings {
        full_duplex: true,
        hang_ms: 5000,
        ..Default::default()
    };
    let (mut controller, _control) =
        NodeController::new(settings, vec![], vec![], CourtesySettings::default()).unwrap();
    let timing = PhoneLatencyTiming {
        target_latency_ms: 250,
        maximum_latency_ms: 500,
    };
    let vad = PhoneVadSettings {
        enabled: true,
        mode: VadMode::Continuous,
        hang_ms: 1000,
    };
    let mut radio = Radio::new(writer, egress, activity, vad, timing, confirmed).unwrap();
    radio.activate();

    let mut heard_voice = false;
    let mut source_keyed = false;
    let mut held_after_release = false;
    let mut final_ptt = false;
    let silent_lead = 20;
    let speech_end = silent_lead + speech.len() / 160;
    // Each turn supplies a fresh, non-concealed frame, including the eight seconds of silence.
    for frame in 0..speech_end + 400 {
        let now = 1_000_000_000 + frame as u64 * 20_000_000;
        let mut pcm = [0.0; 160];
        if (silent_lead..speech_end).contains(&frame) {
            let start = (frame - silent_lead) * 160;
            for (sample, byte) in pcm.iter_mut().zip(&speech[start..start + 160]) {
                *sample = pcmu_sample(*byte);
            }
        }
        let receipt = radio.begin(now);
        let input = Input::Audio {
            start: frame as u64 * 160,
            pcm: &pcm,
            born_ns: Some(now),
            concealed: false,
        };
        assert_eq!(radio.input(input, now, receipt), Ok(160));
        source_keyed = radio.incoming.signals().output_radio_keyed(receipt);
        let mut rf = [0.0; 960];
        final_ptt = graph
            .process(&mut controller, false, &mut rf, SourcePosition::default())
            .unwrap();
        if frame < silent_lead {
            assert!(!source_keyed && !final_ptt);
        }
        heard_voice |= source_keyed && final_ptt && rf.iter().any(|sample| sample.abs() > 0.001);
        if frame >= speech_end + 100 {
            assert!(!source_keyed, "fresh silence kept the phone source keyed");
            held_after_release |= final_ptt;
        }
    }
    assert!(
        heard_voice,
        "the real speech detector must first admit the spoken recording"
    );
    assert!(
        held_after_release,
        "station PTT hang must outlast phone voice"
    );
    assert!(
        !source_keyed && !final_ptt,
        "fresh silence must outlast both timers"
    );
}
