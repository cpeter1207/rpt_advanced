//! Native held-speech close and configured P proof with a synchronous test station.
use super::*;
use crate::link::{
    input::AudioInput,
    session::source::{Decoded, SourceIngress},
};
use rpt_advanced_core::{
    audio::{LinkAudioQueue, MorseRenderer, PcmRead, PcmStreamReader},
    config::ConfigDocument,
    link::{
        AudioPeer, LinkAudio, Mode, PeerIdentity, PhoneSpeechPhase, PhoneSpeechState,
        RenderedPeerBlock, RetirementDomain, SourcePosition,
    },
    media::{
        FileRequest, MediaError, MediaProgram, PreparedAudio, SpeechRequest, StationMediaSession,
    },
    runtime::{
        DeviceHandoff, GenerationSettings, NativeFilePreparer, NativeMediaPreparer,
        NativeSpeechPreparer, PreparedAdapter, Runtime, RuntimeClock,
    },
};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

#[derive(Default)]
struct Evidence {
    phase: AtomicU8,
    qualified: AtomicU64,
    past_hold: AtomicBool,
}
impl Evidence {
    fn phase(&self) -> PhoneSpeechPhase {
        match self.phase.load(Ordering::Relaxed) {
            1 => PhoneSpeechPhase::Open,
            2 => PhoneSpeechPhase::Draining,
            3 => PhoneSpeechPhase::Closed,
            _ => PhoneSpeechPhase::Unavailable,
        }
    }
}
// Observe only this owned input after its real render. No production API/state is added.
struct Observed {
    inner: AudioInput,
    evidence: Option<Arc<Evidence>>,
}
impl Observed {
    fn observe(&self) {
        if let (AudioInput::Phone(input), Some(evidence)) = (&self.inner, &self.evidence) {
            evidence.phase.store(
                match input.phone_speech_state().unwrap().phase {
                    PhoneSpeechPhase::Open => 1,
                    PhoneSpeechPhase::Draining => 2,
                    PhoneSpeechPhase::Closed => 3,
                    PhoneSpeechPhase::Unavailable => 0,
                },
                Ordering::Relaxed,
            );
            evidence
                .qualified
                .store(input.input.source().qualified, Ordering::Relaxed);
            evidence
                .past_hold
                .store(input.input.source().past_hold, Ordering::Relaxed);
        }
    }
}
impl PeerInput for Observed {
    fn phone_speech_state(&self) -> Option<PhoneSpeechState> {
        self.inner.phone_speech_state()
    }
    fn phone_reply(&self) -> Option<&PeerReply> {
        self.inner.phone_reply()
    }
    fn source_rate(&self) -> u32 {
        self.inner.source_rate()
    }
    fn receive_policy(&self) -> ReceivePolicy {
        self.inner.receive_policy()
    }
    fn signals(&self) -> &PeerSignals {
        self.inner.signals()
    }
    fn available(&self) -> u64 {
        self.inner.available()
    }
    fn render_timed_block(&mut self, out: &mut [f32]) -> Option<RenderedPeerBlock> {
        let result = self.inner.render_timed_block(out);
        self.observe();
        result
    }
    fn render(&mut self, out: &mut [f32]) -> Option<usize> {
        self.render_timed_block(out).map(|b| b.real)
    }
    fn silence(&mut self, out: &mut [f32]) {
        self.inner.silence(out);
        self.observe();
    }
}
struct Device;
impl DeviceHandoff for Device {
    fn quiesce(&mut self) -> bool {
        true
    }
    fn close(&mut self) {}
    fn open(&mut self, _: &GenerationSettings) -> bool {
        true
    }
}

#[test]
fn configured_native_phone_morse_p_follows_original_hold_and_real_dsp_tail() {
    const RATE: u32 = 48_000;
    const HOLD_MS: u32 = 200;
    const DELAY: usize = 4_800;
    let call = reply();
    let source = call.phone_source().unwrap().clone();
    let (dsp, graph) = crate::services::history_tests::processor_fixture(1, 1);
    let render_tap = crate::link::ring::tests::TimedTapGuard::next();
    let (mut ingress, mut input, shared) = super::super::prepare_voiced(
        call.clone(),
        RATE,
        NonZeroU32::MIN,
        Some(graph),
        PhoneSpeechSettings {
            hold_ms: HOLD_MS,
            ..Default::default()
        },
    )
    .unwrap();
    adopt(&mut ingress, 0, reference());
    let mut phone_output = ingress.take_output().unwrap();
    let output = input.take_output().unwrap();
    let evidence = Arc::new(Evidence::default());
    let phone = AudioPeer::for_phone(
        &source,
        &call,
        Mode::TRANSCEIVE,
        Observed {
            inner: AudioInput::Phone(input),
            evidence: Some(evidence.clone()),
        },
        output,
        1,
    )
    .unwrap();
    let (_writer, ordinary) = InboundRing::open(RATE, InboundPolicy::Peer).unwrap();
    let (out, mut peer_output) = LinkAudioQueue::new(960, 16).unwrap().into_endpoints();
    let public = AudioPeer::new(
        PeerIdentity::from_operator("2000").unwrap(),
        Mode::TRANSCEIVE,
        Observed {
            inner: AudioInput::Peer(ProcessedPeer::new(ordinary, None)),
            evidence: None,
        },
        out,
        1,
        0,
    )
    .unwrap();
    let domain = RetirementDomain::default();
    let (mixer, mut dispatcher) =
        LinkAudio::new_in_domain(vec![phone, public], 1, domain.clone()).unwrap();
    let mut mixer = Some(mixer.with_phone_clock(7));
    let config = ConfigDocument::parse(
        r#"
[1000]
radio_channel=fixture
duplex=full
transmit_timeout_ms=0
kerchunk_max_ms=0
courtesy_delay_ms=100
[courtesy 1000 phone]
input=phone
morse_text=P
morse_speed_wpm=25
morse_frequency_hz=800
level_db=-6
"#,
    )
    .unwrap();
    let mut runtime = Runtime::start(
        config,
        &NativeMorseStation,
        |_, _| {
            Ok(PreparedAdapter {
                phone_retirement: Some(domain.clone()),
                remote_commands: None,
                state: mixer.take().unwrap(),
                control: (),
                receive_maximum: 1,
                transmit_maximum: 1,
            })
        },
        |_, _| Ok(Box::new(Device) as Box<dyn DeviceHandoff>),
        RuntimeClock {
            now_ms: 0,
            wall_seconds: 0,
            wall_subsecond_ms: 0,
            civil: None,
        },
    )
    .unwrap();
    let (_receive, mut transmit) = runtime.node("1000").unwrap().register_audio().unwrap();
    let guard = crate::link::phone_speech::loss_reset_contract_tests::NativeGuard::new();
    let mut native = 0usize;
    let mut closed = None;
    let mut draining = None;
    let mut previous_qualified = 0;
    let mut tail_nonzero = false;
    let mut after_close = Vec::with_capacity(144_000);
    let mut remote_voice = false;
    let expected_hold = std::cell::Cell::new(None::<(u64, u64)>);
    let mut last_held = None;
    let mut previous_render_calls = 0;
    let mut tick = || {
        let ((rf, metadata), allocations) = guard.measure(|| {
            let mut generation = transmit.acquire().unwrap();
            let state = generation.state();
            let mut rf = [0.0];
            state
                .adapter
                .process(
                    &mut state.controller,
                    false,
                    &mut rf,
                    SourcePosition {
                        generation: 1,
                        sample: native as u64,
                        stream: 1,
                        born_ns: Some(10 + native as u64 * 1_000_000_000 / u64::from(RATE)),
                    },
                )
                .unwrap();
            let metadata = state.adapter.rf_programme();
            dispatcher.dispatch(2);
            (rf[0], metadata)
        });
        assert_eq!(allocations, 0);
        let phase = evidence.phase();
        let raw = render_tap.0.snapshot();
        let support = raw
            .coordinate
            .filter(|position| !position.negative)
            .map(|position| {
                assert_eq!(position.generation, shared.generation);
                assert!(position.fraction_billionths < 1_000_000_000);
                (
                    position,
                    position.frame + u64::from(position.fraction_billionths != 0),
                )
            });
        if let Some((hold_start, hold_end)) = expected_hold.get() {
            if let Some((position, upper)) = support.filter(|(_, upper)| *upper >= hold_start) {
                assert_eq!(
                    raw.calls,
                    previous_render_calls + 1,
                    "exactly one actual native source render per DSP frame"
                );
                if upper < hold_end {
                    assert_eq!(
                        dsp.retained_sample().to_bits(),
                        raw.pcm.to_bits(),
                        "original held source must not gate before independently recomputed endpoint"
                    );
                    last_held = Some((position, native, raw.pcm));
                } else {
                    assert_eq!(
                        dsp.retained_sample(),
                        0.0,
                        "no new qualified input after original endpoint"
                    );
                }
            }
            if matches!(phase, PhoneSpeechPhase::Draining | PhoneSpeechPhase::Closed) {
                let (_,upper)=support.expect("actual native source coordinate, not phase or past_hold, proves consumed endpoint");
                assert!(
                    upper >= hold_end,
                    "actual rendered source support must reach original endpoint before draining/close: {upper} < {hold_end}"
                );
                let (last_position, last_native, last_pcm) =
                    last_held.expect("original held source must actually render before tail");
                assert!(
                    last_position.frame + u64::from(last_position.fraction_billionths != 0)
                        < hold_end
                );
                if phase == PhoneSpeechPhase::Draining {
                    assert_eq!(native, last_native + 1);
                    assert_eq!(
                        rf.to_bits(),
                        last_pcm.to_bits(),
                        "actual retained output is the final original-held source sample"
                    );
                } else if closed.is_none() {
                    assert_eq!(
                        native,
                        last_native + 2,
                        "completed close follows actual last held source and one retained output frame"
                    );
                }
            }
        }
        previous_render_calls = raw.calls;
        if phase == PhoneSpeechPhase::Draining {
            assert!(evidence.past_hold.load(Ordering::Relaxed));
            assert_eq!(
                evidence.qualified.load(Ordering::Relaxed),
                previous_qualified,
                "retained DSP tail, not a newly qualified source sample"
            );
            assert!(metadata.active);
            tail_nonzero |= rf.abs() > 0.001;
            draining = Some(native);
        }
        if phase == PhoneSpeechPhase::Closed && closed.is_none() {
            closed = Some(native);
            assert!(!metadata.active);
        }
        if closed.is_some() {
            assert_eq!(
                phase,
                PhoneSpeechPhase::Closed,
                "no source reopen or unavailable timing may manufacture the positive case"
            );
            assert!(!metadata.active);
            after_close.push(rf);
        }
        let mut sent = [0.0];
        while phone_output.read_span(&mut sent).is_some() {
            assert_eq!(sent, [0.0], "no phone self echo or courtesy");
        }
        let mut ordinary_sample = 0.0;
        while peer_output.read_span(&mut sent).is_some() {
            ordinary_sample = sent[0];
            if closed.is_some() {
                assert_eq!(sent, [0.0], "Morse must stay off ordinary peer output");
            } else {
                remote_voice |= sent[0].abs() > 0.01;
            }
        }
        if closed.is_none() {
            assert_eq!(
                rf, ordinary_sample,
                "no local-only P may play before completed close"
            );
        }
        previous_qualified = evidence.qualified.load(Ordering::Relaxed);
        native += 1;
    };
    let speech: Vec<f32> = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm")
        .chunks_exact(2)
        .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
        .collect();
    let mut detector = SourceIngress::<64>::at_rate(true, RATE, 960).unwrap();
    let mut scratch = [0.0; 960];
    let mut digits = Vec::with_capacity(8);
    let mut onsets = 0;
    let mut cursor = 0usize;
    for block in speech.chunks(960) {
        let time = Some(birth(10 + cursor as u64 * 1_000_000_000 / u64::from(RATE)));
        let (result, allocations) = guard.measure(|| {
            detector
                .prepare(
                    &mut scratch,
                    Decoded {
                        start: cursor as u64,
                        pcm: block,
                        concealed: false,
                        birth: time,
                    },
                    |digit| digits.push(digit),
                    || onsets += 1,
                )
                .unwrap();
            let n = ingress
                .audio(cursor as u64, &scratch[..block.len()], time)
                .unwrap();
            detector.accepted(n).unwrap();
            n
        });
        assert_eq!(result, block.len());
        assert_eq!(allocations, 0);
        cursor += block.len();
        for _ in block {
            tick();
        }
        if evidence.phase() == PhoneSpeechPhase::Open {
            break;
        }
    }
    assert!(
        ingress.speech.is_some(),
        "native analyzer must remain installed"
    );
    assert_eq!(
        evidence.phase(),
        PhoneSpeechPhase::Open,
        "genuine native classifier/output proof must open the source"
    );
    let original_hold = (ingress.last_voice, ingress.voice_until);
    assert_eq!(
        original_hold.1,
        original_hold.0.unwrap() + 1 + u64::from(HOLD_MS) * 48
    );
    expected_hold.set(Some((
        original_hold.0.unwrap() + 1,
        original_hold.0.unwrap() + 1 + u64::from(HOLD_MS) * 48,
    )));
    assert!(
        original_hold.1 > cursor as u64,
        "leave actual original hold for synthetic continuity"
    );
    let tone: Vec<f32> = (0..HOLD_MS as usize * 48 + 1920)
        .map(|n| {
            let p = n as f32 * std::f32::consts::TAU / RATE as f32;
            0.25 * ((p * 770.0).sin() + (p * 1336.0).sin())
        })
        .collect();
    let mut synthetic = 0;
    // After real speech, known synthetic telephone tone traverses the actual
    // detector seam but cannot analyze, renew original hold, or collect a digit.
    for _ in 0..150 {
        let concealed = (cursor as u64) < original_hold.1 + 1920;
        let count = if concealed {
            960.min((original_hold.1 + 1920 - cursor as u64) as usize)
        } else {
            960
        };
        let silence = [0.0; 960];
        let pcm = if concealed {
            &tone[synthetic..synthetic + count]
        } else {
            &silence[..count]
        };
        let time = if concealed {
            Some(birth(10))
        } else {
            Some(birth(10 + cursor as u64 * 1_000_000_000 / u64::from(RATE)))
        };
        let (n, allocations) = guard.measure(|| {
            detector
                .prepare(
                    &mut scratch,
                    Decoded {
                        start: cursor as u64,
                        pcm,
                        concealed,
                        birth: time,
                    },
                    |digit| digits.push(digit),
                    || onsets += 1,
                )
                .unwrap();
            let n = if concealed {
                ingress.concealed(cursor as u64, &scratch[..count], time)
            } else {
                ingress.audio(cursor as u64, &scratch[..count], time)
            }
            .unwrap();
            detector.accepted(n).unwrap();
            n
        });
        assert_eq!(n, count);
        assert_eq!(allocations, 0);
        assert_eq!(
            (ingress.last_voice, ingress.voice_until),
            original_hold,
            "synthetic input and genuine silence cannot requalify this speech"
        );
        assert!(digits.is_empty());
        assert_eq!(onsets, 0);
        assert!(!detector.muted());
        if concealed {
            synthetic += count;
        }
        cursor += count;
        for _ in 0..count {
            tick();
        }
    }
    assert!(remote_voice);
    assert!(synthetic >= 1920, "meaningful synthetic tone exercise");
    assert!(
        tail_nonzero,
        "actual qualified nonzero post-DSP tail must drain"
    );
    assert_eq!(
        closed.unwrap(),
        draining.unwrap() + 1,
        "one-frame retained DSP tail precedes completed close"
    );
    assert!(
        after_close[..DELAY].iter().all(|sample| *sample == 0.0),
        "delay starts at completed close, not original hold expiry"
    );
    assert_eq!(
        after_close.iter().position(|s| *s != 0.0),
        Some(DELAY + 1),
        "configured delay plus native Morse's first zero sample"
    );
    let dot = 2304;
    let gates = [
        (0, dot),
        (2 * dot, 5 * dot),
        (6 * dot, 9 * dot),
        (10 * dot, 11 * dot),
    ];
    for (start, end) in gates {
        let mut cycles = after_close[DELAY + start..DELAY + end].chunks_exact(60);
        for cycle in &mut cycles {
            // Independent 800 Hz sin/cos projection, allowing arbitrary phase.
            // A 1600 Hz tone has ~zero projection even though both signs occur.
            let mut energy = 0.0;
            let mut sine = 0.0;
            let mut cosine = 0.0;
            for (index, sample) in cycle.iter().enumerate() {
                let phase = std::f64::consts::TAU * 800.0 * index as f64 / f64::from(RATE);
                let sample = f64::from(*sample);
                energy += sample * sample;
                sine += sample * phase.sin();
                cosine += sample * phase.cos();
            }
            let projected = 2.0 * (sine * sine + cosine * cosine) / cycle.len() as f64;
            assert!(
                energy > 0.01 && projected / energy > 0.999,
                "800 Hz reference energy fraction must exceed99.9%, got {}",
                projected / energy
            );
            assert!(
                cycle.iter().any(|s| *s > 0.05) && cycle.iter().any(|s| *s < -0.05),
                "complete 800 Hz cycles in every dot/dash, not a constant stand-in"
            );
        }
        let remainder = cycles.remainder();
        assert!(remainder.is_empty() || remainder.iter().any(|s| s.abs() > 0.05));
    }
    for (start, end) in [(dot, 2 * dot), (5 * dot, 6 * dot), (9 * dot, 10 * dot)] {
        assert!(
            after_close[DELAY + start..DELAY + end]
                .iter()
                .all(|s| *s == 0.0),
            "three exact intra-P Morse gaps"
        );
    }
    assert!(
        after_close[DELAY + 11 * dot..].iter().all(|s| *s == 0.0),
        "one complete .--. only, no silent callback repeat"
    );
    assert!(dsp.retained_sample().abs() < 0.001);
    assert!(!shared.reply.ended());
    eprintln!(
        "native hold_ms={HOLD_MS} last_voice={:?} voice_until={} synthetic_samples={synthetic} last_draining={draining:?} completed_close={closed:?} post_close_samples={} first_P={:?}",
        original_hold.0,
        original_hold.1,
        after_close.len(),
        after_close.iter().position(|s| *s != 0.0)
    );
}

// This synchronous TEST station consumes the actual runtime-resolved program.
// It uses the same released MorseRenderer as the product producer, but does NOT
// qualify asynchronous station scheduling, readiness/Pending or SIP end-to-end.
struct NativeMorseStation;
impl NativeFilePreparer for NativeMorseStation {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        panic!("no file configured")
    }
}
impl NativeSpeechPreparer for NativeMorseStation {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        panic!("no speech configured")
    }
}
impl NativeMediaPreparer for NativeMorseStation {
    fn station(&self, node: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
        assert_eq!(node, "1000");
        Ok(Box::new(Self))
    }
}
impl StationMediaSession for NativeMorseStation {
    fn register_program(
        &mut self,
        program: MediaProgram,
        one_shot: bool,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        assert!(!one_shot);
        let [source] = program.primary.as_slice() else {
            panic!("one configured courtesy source required")
        };
        assert!(source.file.is_none() && source.speech.is_none() && source.tone.is_none());
        let morse = source
            .morse
            .as_ref()
            .expect("runtime must supply the configured Morse source");
        assert_eq!(program.morse.as_slice(), std::slice::from_ref(morse));
        assert_eq!(morse.text, "P");
        assert_eq!(morse.speed_wpm, 25);
        assert_eq!(morse.frequency_hz, 800.0);
        assert_eq!(morse.level_db, -6);
        Ok(Box::new(NativeMorseStream(
            MorseRenderer::new(
                &morse.text,
                morse.speed_wpm,
                morse.frequency_hz,
                morse.level_db,
            )
            .unwrap(),
        )))
    }
    fn start(&mut self) -> Result<(), MediaError> {
        Ok(())
    }
}
struct NativeMorseStream(MorseRenderer);
impl PcmStreamReader for NativeMorseStream {
    fn start(&mut self) {
        self.0.restart();
    }
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        let count = self.0.render(output);
        if count < output.len() {
            PcmRead::FinalSamples(count)
        } else {
            PcmRead::Samples(count)
        }
    }
}
