use super::Playback;
use crate::audio::{PcmRead, PcmStreamReader};
use std::collections::VecDeque;

struct Stream(VecDeque<Option<Vec<f32>>>);

impl PcmStreamReader for Stream {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        let Some(chunk) = self.0.front_mut() else {
            return PcmRead::Finished;
        };
        let Some(chunk) = chunk else {
            self.0.pop_front();
            return PcmRead::Pending;
        };
        let count = output.len().min(chunk.len());
        output[..count].copy_from_slice(&chunk[..count]);
        if count == chunk.len() {
            self.0.pop_front();
        } else {
            chunk.drain(..count);
        }
        PcmRead::Samples(count)
    }
}

#[test]
fn stream_playback_keeps_temporary_empty_reads_distinct_from_eof() {
    let stream = Stream(VecDeque::from([
        Some(vec![0.25, 0.5]),
        None,
        Some(vec![0.75]),
    ]));
    let mut playback = Playback::new_stream(Box::new(stream), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 3];

    assert_eq!(playback.render(false, &mut output), 2);
    assert_eq!(&output[..2], &[0.25, 0.5]);
    assert!(!playback.is_finished());

    assert_eq!(playback.render(false, &mut output), 0);
    assert!(!playback.is_finished());

    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(output[0], 0.75);
    assert!(!playback.is_finished());
    assert_eq!(playback.render(false, &mut output), 0);
    assert!(playback.is_finished());
}

#[test]
fn stream_playback_can_return_its_reader_for_off_callback_retirement() {
    let stream = Stream(VecDeque::from([Some(vec![0.25])]));
    let mut playback = Playback::new_stream(Box::new(stream), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 1];
    assert_eq!(playback.render(true, &mut output), 1);
    assert!(playback.take_stream().is_some());
}

#[test]
fn absent_and_empty_pcm_share_morse_restart_and_trait_rendering() {
    use crate::audio::AudioSource;
    for prepared in [None, Some(vec![])] {
        let mut playback = Playback::new(prepared, "E", 20, 1000.0, -6, false).unwrap();
        let mut first = [0.0; 48];
        assert_eq!(AudioSource::render(&mut playback, &mut first), 48);
        assert!(first.iter().any(|sample| *sample != 0.0));
        playback.restart(false);
        let mut next = [0.0; 48];
        assert_eq!(AudioSource::render(&mut playback, &mut next), 48);
        assert_eq!(first, next);
    }
}

#[test]
fn playback_abandons_prepared_audio_for_unadvanced_morse_on_reception() {
    let mut playback =
        Playback::new(Some(vec![0.1, 0.2, 0.3, 0.4]), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 2];
    assert_eq!(playback.render(false, &mut output), 2);
    assert_eq!(output, [0.1, 0.2]);

    let mut morse = vec![0.0; 160];
    assert_eq!(playback.render(true, &mut morse), 160);
    assert_eq!(morse[0], 0.0);
    assert!(morse.iter().any(|sample| *sample != 0.0));
    assert_eq!(playback.render(false, &mut output), 2);
    assert_ne!(output, [0.3, 0.4]);
}

#[test]
fn reception_switches_to_morse_even_for_zero_capacity() {
    let mut playback = Playback::new(Some(vec![0.1]), "", 20, 1_000.0, -6, false).unwrap();
    assert_eq!(playback.render(true, &mut []), 0);
    let mut output = [99.0];
    assert_eq!(playback.render(false, &mut output), 0);
}

#[test]
fn playback_marks_prepared_media_complete_on_its_final_sample() {
    let mut playback = Playback::new(Some(vec![0.1, 0.2]), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 2];
    assert_eq!(playback.render(false, &mut output), 2);
    assert_eq!(playback.render(false, &mut output), 0);
}

#[test]
fn playback_restart_reuses_prepared_pcm_after_an_interrupted_run() {
    let mut playback = Playback::new(Some(vec![0.1, 0.2]), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 1];
    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(playback.render(true, &mut output), 1);
    playback.restart(false);
    assert!(!playback.is_finished());
    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(output, [0.1]);
}

#[test]
fn playback_restart_resets_morse_without_reconstructing_it() {
    let mut playback = Playback::new(None, "E", 20, 1_000.0, -6, false).unwrap();
    let mut first = [0.0; 48];
    let mut restarted = [0.0; 48];
    assert_eq!(playback.render(false, &mut first), 48);
    playback.restart(true);
    assert_eq!(playback.render(false, &mut restarted), 48);
    assert_eq!(restarted, first);
}

#[test]
fn tone_source_renders_in_blocks_and_restarts_at_the_same_phase() {
    let sequence = "1000/1,1200+1300@-12/1";
    let mut expected_source = crate::audio::ToneSequence::new(sequence, -6).unwrap();
    let mut expected = [0.0; 96];
    assert_eq!(expected_source.render(&mut expected), expected.len());

    let mut playback = Playback::new_with_tone(
        None,
        Some(crate::audio::ToneSequence::new(sequence, -6).unwrap()),
        "E",
        20,
        1_000.0,
        -6,
        false,
    )
    .unwrap();
    let mut first = [0.0; 96];
    assert_eq!(playback.render(false, &mut first), first.len());
    assert_eq!(first, expected);
    assert!(playback.is_finished());

    playback.restart(false);
    let mut restarted = [0.0; 96];
    assert_eq!(playback.render(false, &mut restarted), restarted.len());
    assert_eq!(restarted, expected);
}

#[test]
fn receiving_interrupts_tone_for_morse_fallback() {
    let mut playback = Playback::new_with_tone(
        None,
        Some(crate::audio::ToneSequence::new("1000/100", -6).unwrap()),
        "E",
        20,
        1_000.0,
        -6,
        false,
    )
    .unwrap();
    let mut tone = [0.0; 48];
    assert_eq!(playback.render(false, &mut tone), tone.len());
    assert!(tone.iter().any(|sample| *sample != 0.0));

    let mut morse = [1.0; 48];
    let mut expected = crate::audio::MorseRenderer::new("E", 20, 1_000.0, -6).unwrap();
    let mut expected_samples = [0.0; 48];
    expected.render(&mut expected_samples);
    assert_eq!(playback.render(true, &mut morse), morse.len());
    assert_eq!(morse, expected_samples);
}
