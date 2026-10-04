use super::*;

struct SequenceTestReader {
    samples: Vec<f32>,
    offset: usize,
    fail: bool,
    cancelled: bool,
}

impl PcmStreamReader for SequenceTestReader {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        if self.fail {
            return PcmRead::Failed;
        }
        if self.offset == self.samples.len() {
            return PcmRead::Finished;
        }
        let count = output.len().min(self.samples.len() - self.offset);
        output[..count].copy_from_slice(&self.samples[self.offset..self.offset + count]);
        self.offset += count;
        PcmRead::Samples(count)
    }

    fn cancel(&mut self) {
        self.cancelled = true;
    }
}

fn sequence_reader(samples: &[f32], fail: bool) -> Box<dyn PcmStreamReader> {
    Box::new(SequenceTestReader {
        samples: samples.to_vec(),
        offset: 0,
        fail,
        cancelled: false,
    })
}

fn parrot_media(samples: &[f32]) -> PreparedMedia {
    PreparedMedia::new_parrot_stream(None, sequence_reader(samples, false)).unwrap()
}

#[test]
fn prepared_status_queue_rejects_the_fifth_item() {
    let settings = MorseSettings::default();
    let (_, mut control) = TelemetryPlanner::new(settings, -6);
    for _ in 0..4 {
        assert!(
            control.queue_prepared_status(
                PreparedMedia::new(Some(vec![0.25]), "E", settings).unwrap()
            )
        );
    }
    assert!(!control.can_queue_status());
    assert!(
        !control
            .queue_prepared_status(PreparedMedia::new(Some(vec![0.25]), "E", settings).unwrap())
    );
}

#[test]
fn prepared_status_queue_rejects_a_full_ring_without_incrementing_outstanding() {
    let settings = MorseSettings::default();
    let (_, mut control) = TelemetryPlanner::new(settings, -6);
    for _ in 0..4 {
        control
            .pending
            .push(PreparedMedia::new(Some(vec![0.25]), "E", settings).unwrap())
            .unwrap();
    }

    assert!(
        !control.queue_prepared_status(PreparedMedia::new(Some(vec![0.5]), "T", settings).unwrap())
    );
    assert_eq!(control.outstanding, 0);
}

#[test]
fn test_pcm_stream_reports_partial_and_finished_reads_and_restarts_fallback() {
    let mut stream = TestPcmStream {
        primary: vec![0.25, 0.5],
        fallback: vec![0.75],
        offset: 0,
        use_fallback: false,
    };
    let mut output = [0.0; 1];
    assert_eq!(stream.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output[0], 0.25);
    assert!(stream.select_morse_fallback());
    assert_eq!(stream.render(&mut output), PcmRead::FinalSamples(1));
    assert_eq!(output[0], 0.75);
    assert_eq!(stream.render(&mut output), PcmRead::Finished);
    stream.start();
    assert_eq!(stream.render(&mut []), PcmRead::Finished);
}

#[test]
fn parrot_sequence_plays_report_then_recording() {
    let mut sequence = ParrotSequence::new(
        Some(sequence_reader(&[0.1, 0.2], false)),
        sequence_reader(&[0.7, 0.8], false),
    );
    sequence.start();
    let mut output = [0.0; 2];
    assert_eq!(sequence.render(&mut output), PcmRead::Samples(2));
    assert_eq!(output, [0.1, 0.2]);
    assert_eq!(sequence.render(&mut output), PcmRead::Samples(2));
    assert_eq!(output, [0.7, 0.8]);
    assert_eq!(sequence.render(&mut output), PcmRead::Finished);
}

#[test]
fn parrot_sequence_skips_failed_speech_and_plays_recording() {
    let mut sequence = ParrotSequence::new(
        Some(sequence_reader(&[], true)),
        sequence_reader(&[0.6], false),
    );
    sequence.start();
    let mut output = [0.0; 1];
    assert_eq!(sequence.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output, [0.6]);
}

#[test]
fn parrot_queue_holds_one_item_until_callback_reclaims_it() {
    let (mut planner, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    control.parrot_enabled = true;
    assert!(control.queue_prepared_parrot(parrot_media(&[0.4])));
    assert!(!control.queue_prepared_parrot(parrot_media(&[0.5])));
    planner.start_parrot();
    assert!(planner.active.is_none());
    assert!(planner.parrot.is_some());
    planner.finish_parrot();
    assert!(!control.queue_prepared_parrot(parrot_media(&[0.5])));
    control.reclaim().for_each(drop);
    assert!(control.queue_prepared_parrot(parrot_media(&[0.5])));
}
