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

struct FailedStream;
impl PcmStreamReader for FailedStream {
    fn render(&mut self, _: &mut [f32]) -> PcmRead {
        PcmRead::Failed
    }
}

struct FinalStream;
impl PcmStreamReader for FinalStream {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        output[..3].copy_from_slice(&[0.1, 0.2, 0.3]);
        PcmRead::FinalSamples(3)
    }
}

struct ProducerFallbackStream(bool);
impl PcmStreamReader for ProducerFallbackStream {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        output.fill(if self.0 { 0.75 } else { 0.25 });
        PcmRead::Samples(output.len())
    }
    fn select_morse_fallback(&mut self) -> bool {
        self.0 = true;
        true
    }
}

#[test]
fn playback_reads_only_streamed_pcm() {
    let stream = Stream(VecDeque::from([
        Some(vec![0.25, 0.5]),
        None,
        Some(vec![0.75]),
    ]));
    let mut playback = Playback::new_stream(Box::new(stream));
    let mut output = [0.0; 3];

    assert_eq!(playback.render(false, &mut output), 2);
    assert_eq!(&output[..2], &[0.25, 0.5]);
    assert!(!playback.is_finished());

    assert_eq!(playback.render(false, &mut output), 0);
    assert!(playback.waiting_for_stream());
    assert!(!playback.is_finished());

    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(output[0], 0.75);
    assert!(!playback.waiting_for_stream());
    assert_eq!(playback.render(false, &mut output), 0);
    assert!(playback.is_finished());
}

#[test]
fn receive_interruption_selects_producer_generated_morse() {
    let mut playback = Playback::new_stream(Box::new(ProducerFallbackStream(false)));
    let mut output = [0.0; 8];

    assert_eq!(playback.render(true, &mut output), output.len());
    assert_eq!(output, [0.75; 8]);
    assert!(playback.is_streaming());
}

#[test]
fn empty_receive_event_selects_producer_generated_morse() {
    let mut playback = Playback::new_stream(Box::new(ProducerFallbackStream(false)));

    assert_eq!(playback.render(true, &mut []), 0);
    let mut output = [0.0; 2];
    assert_eq!(playback.render(true, &mut output), 2);
    assert_eq!(output, [0.75; 2]);
}

#[test]
fn failed_stream_ends_without_transmit_worker_fallback_generation() {
    let mut playback = Playback::new_stream(Box::new(FailedStream));
    let mut output = [0.0; 8];

    assert_eq!(playback.render(false, &mut output), 0);
    assert_eq!(output, [0.0; 8]);
    assert!(playback.is_finished());
}

#[test]
fn final_partial_samples_complete_playback_without_a_silent_followup_block() {
    let mut playback = Playback::new_stream(Box::new(FinalStream));
    let mut output = [0.0; 8];

    assert_eq!(playback.render(false, &mut output), 3);
    assert_eq!(&output[..3], &[0.1, 0.2, 0.3]);
    assert!(playback.is_finished());
}

#[test]
fn stream_can_be_retired_by_its_non_audio_owner() {
    let stream = Stream(VecDeque::from([Some(vec![0.25])]));
    let mut playback = Playback::new_stream(Box::new(stream));
    assert!(playback.take_stream().is_some());
    assert!(playback.is_finished());
}

#[test]
fn receive_without_a_fallback_stops_and_restart_keeps_that_policy() {
    let mut playback = Playback::new_stream(Box::new(Stream(VecDeque::new())));
    let mut output = [0.0; 4];

    assert_eq!(playback.render(true, &mut output), 0);
    assert!(playback.is_finished());
    assert_eq!(playback.render(false, &mut output), 0);

    playback.restart(true);
    assert!(playback.is_finished());
    assert_eq!(playback.render(false, &mut output), 0);
}

#[test]
fn restart_without_receive_starts_a_new_stream_pass() {
    let stream = Stream(VecDeque::from([Some(vec![0.5]), Some(vec![0.75])]));
    let mut playback = Playback::new_stream(Box::new(stream));
    let mut output = [0.0; 1];

    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(output, [0.5]);
    playback.restart(false);
    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(output, [0.75]);
}

#[test]
fn removed_stream_stays_silent_after_restart_and_receive_interruption() {
    let mut playback = Playback::new_stream(Box::new(Stream(VecDeque::new())));
    assert!(playback.take_stream().is_some());
    playback.restart(false);
    let mut output = [0.0; 2];
    assert_eq!(playback.render(true, &mut output), 0);
    assert!(playback.is_finished());
}

#[test]
fn audio_source_trait_delegates_to_stream_playback() {
    let mut playback = Playback::new_stream(Box::new(Stream(VecDeque::from([Some(vec![0.25])]))));
    let mut output = [0.0; 1];
    assert_eq!(
        crate::audio::AudioSource::render(&mut playback, &mut output),
        1
    );
    assert_eq!(output, [0.25]);
}

#[test]
fn absent_stream_is_started_as_finished_without_a_cancel_callback() {
    let mut playback = Playback {
        stream: None,
        stream_started: false,
        stream_fallback: false,
        waiting_for_stream: false,
        finished: false,
    };
    let mut output = [1.0; 2];

    assert_eq!(playback.render(false, &mut output), 0);
    assert!(playback.is_finished());
    assert_eq!(output, [1.0; 2]);
}
