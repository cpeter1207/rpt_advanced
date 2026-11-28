use super::*;

#[test]
fn occurrence_replacement_cancels_only_audio_that_has_not_begun() {
    let pending = MessageOccurrence::default();
    pending.cancel_pending();
    assert!(pending.cancelled());
    assert!(!pending.begin());
    let playing = MessageOccurrence::default();
    assert!(playing.begin());
    playing.cancel_pending();
    assert!(
        !playing.cancelled(),
        "new occurrences cannot cut off audible playback"
    );
}

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

struct PendingOnceReader {
    pending: bool,
    samples: Vec<f32>,
    offset: usize,
}

struct ZeroThenFinishedReader(bool);

impl PcmStreamReader for ZeroThenFinishedReader {
    fn render(&mut self, _: &mut [f32]) -> PcmRead {
        if std::mem::replace(&mut self.0, false) {
            PcmRead::FinalSamples(0)
        } else {
            PcmRead::Finished
        }
    }
}

impl PcmStreamReader for PendingOnceReader {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        if self.pending {
            self.pending = false;
            return PcmRead::Pending;
        }
        if self.offset == self.samples.len() {
            return PcmRead::Finished;
        }
        let count = output.len().min(self.samples.len() - self.offset);
        output[..count].copy_from_slice(&self.samples[self.offset..self.offset + count]);
        self.offset += count;
        PcmRead::Samples(count)
    }
}

fn parrot_media(samples: &[f32]) -> PreparedMedia {
    PreparedMedia::new_parrot_stream(None, sequence_reader(samples, false))
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
    assert_eq!(
        stream.select_morse_fallback(),
        crate::audio::MorseSwitch::Selected
    );
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
fn parrot_sequence_skips_zero_length_final_report_and_starts_recording() {
    let mut sequence = ParrotSequence::new(
        Some(Box::new(ZeroThenFinishedReader(true))),
        sequence_reader(&[0.6], false),
    );
    sequence.start();
    let mut output = [0.0; 1];

    assert_eq!(sequence.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output, [0.6]);
}

#[test]
fn parrot_sequence_waits_for_pending_report_then_cancels_both_streams() {
    let mut sequence = ParrotSequence::new(
        Some(Box::new(PendingOnceReader {
            pending: true,
            samples: vec![0.1],
            offset: 0,
        })),
        sequence_reader(&[0.8], false),
    );
    sequence.start();
    let mut output = [0.0; 1];
    assert_eq!(sequence.render(&mut []), PcmRead::Pending);
    assert_eq!(sequence.render(&mut output), PcmRead::Pending);
    assert_eq!(sequence.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output, [0.1]);
    assert_eq!(sequence.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output, [0.8]);
    sequence.cancel();
}

#[test]
fn parrot_lifecycle_acknowledges_enable_and_preserves_disable_request_on_full_queue() {
    let (_, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    assert!(control.set_parrot_enabled(false));
    assert!(!control.set_parrot_enabled(true));

    let (requests, mut audio_requests) = RingBuffer::new(1);
    let (mut audio_acknowledgements, acknowledgements) = RingBuffer::new(1);
    let flag = Arc::new(AtomicBool::new(false));
    control.configure_parrot_lifecycle(requests, acknowledgements, Arc::clone(&flag));
    assert!(control.set_parrot_enabled(true));
    assert!(flag.load(Ordering::Acquire));
    assert!(!control.set_parrot_enabled(false));
    let ParrotRequest::Enable(capture) = audio_requests.pop().unwrap() else {
        panic!("enable request queued");
    };
    audio_acknowledgements.push(ParrotAck::Enabled).unwrap();
    control.reclaim_parrot_lifecycle();
    assert!(control.parrot.is_some());
    assert!(control.set_parrot_enabled(false));
    assert!(!control.parrot_enabled());
    assert!(!flag.load(Ordering::Acquire));
    assert!(matches!(
        audio_requests.pop().unwrap(),
        ParrotRequest::Disable
    ));
    audio_acknowledgements
        .push(ParrotAck::Disabled(capture))
        .unwrap();
    control.reclaim_parrot_lifecycle();
    assert!(control.parrot.is_none());
}

#[test]
fn parrot_enable_queue_full_does_not_change_state() {
    let (_, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    let (mut requests, _) = RingBuffer::new(1);
    let (_, acknowledgements) = RingBuffer::new(1);
    let flag = Arc::new(AtomicBool::new(false));
    requests.push(ParrotRequest::Disable).unwrap();
    control.configure_parrot_lifecycle(requests, acknowledgements, Arc::clone(&flag));

    assert!(!control.set_parrot_enabled(true));
    assert!(!control.parrot_enabled());
    assert!(!flag.load(Ordering::Acquire));
}

#[test]
fn parrot_disable_queue_full_does_not_change_state() {
    let (_, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    let (mut requests, _) = RingBuffer::new(1);
    let (_, acknowledgements) = RingBuffer::new(1);
    let flag = Arc::new(AtomicBool::new(true));
    requests.push(ParrotRequest::Disable).unwrap();
    control.parrot_enabled = true;
    control.configure_parrot_lifecycle(requests, acknowledgements, Arc::clone(&flag));

    assert!(!control.set_parrot_enabled(false));
    assert!(control.parrot_enabled());
    assert!(flag.load(Ordering::Acquire));
}

#[test]
fn parrot_queue_rejects_status_and_resets_busy_after_full_ring() {
    let (_, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    assert!(!control.queue_prepared_parrot(parrot_media(&[0.4])));
    control.parrot_enabled = true;
    assert!(!control.queue_prepared_parrot(
        PreparedMedia::new(Some(vec![0.5]), "E", MorseSettings::default()).unwrap()
    ));
    assert!(
        control
            .parrot_pending
            .push(super::QueuedParrot {
                media: parrot_media(&[0.6]),
                epoch: 0
            })
            .is_ok()
    );

    assert!(!control.queue_prepared_parrot(parrot_media(&[0.7])));
    assert!(!control.parrot_busy.load(Ordering::Acquire));
}

#[test]
fn parrot_queue_holds_one_item_until_callback_reclaims_it() {
    let (mut planner, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    control.parrot_enabled = true;
    assert!(control.queue_prepared_parrot(parrot_media(&[0.4])));
    assert!(!control.queue_prepared_parrot(parrot_media(&[0.5])));
    planner.start_parrot(0);
    assert!(planner.active.is_none());
    assert!(planner.parrot.is_some());
    planner.finish_parrot();
    assert!(!control.queue_prepared_parrot(parrot_media(&[0.5])));
    control.reclaim().for_each(drop);
    assert!(control.queue_prepared_parrot(parrot_media(&[0.5])));
}

#[test]
fn empty_parrot_planner_and_unconfigured_capture_recycle_are_noops() {
    let (_, mut control) = TelemetryPlanner::new(MorseSettings::default(), -6);
    let (mut recorder, mut capture_control) = ParrotCapture::new();
    recorder.observe(true, &[0.25]);
    recorder.observe(false, &[]);
    control.recycle_parrot_capture(capture_control.take_completed().unwrap());

    let (mut planner, _) = TelemetryPlanner::new(MorseSettings::default(), -6);
    planner.start_parrot(0);
    planner.finish_parrot();
    assert!(planner.parrot.is_none());
}

#[test]
fn telemetry_observations_distinguish_queued_audio_from_pcm_and_terminal_results() {
    use super::super::{ControllerSettings, CourtesySettings, NodeController};
    use crate::controller::TelemetryPhase;

    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(
        control.queue_prepared_status(
            PreparedMedia::new_stream(sequence_reader(&[0.2], false)).unwrap()
        )
    );
    let mut rx = [0.0; 48];
    node.process_audio(true, false, &[], &mut rx);
    let queued = control.take_telemetry_observation().unwrap();
    assert_eq!(queued.phase, TelemetryPhase::Queued);
    assert!(control.take_telemetry_observation().is_none());

    let mut quiet = [0.0; 12_480];
    node.process_audio(false, false, &[], &mut quiet);
    let started = control.take_telemetry_observation().unwrap();
    assert_eq!(started.phase, TelemetryPhase::Started);
    assert_eq!(started.id, queued.id);
    assert!(started.sample_position > queued.sample_position);
    let finished = control.take_telemetry_observation().unwrap();
    assert_eq!(finished.phase, TelemetryPhase::Finished);
    assert_eq!(finished.id, queued.id);

    assert!(
        control
            .queue_prepared_status(PreparedMedia::new_stream(sequence_reader(&[], true)).unwrap())
    );
    node.process_audio(false, false, &[], &mut quiet);
    let queued = control.take_telemetry_observation().unwrap();
    let failed = control.take_telemetry_observation().unwrap();
    assert_eq!(queued.phase, TelemetryPhase::Queued);
    assert_eq!(failed.phase, TelemetryPhase::Failed);
    assert_eq!(failed.id, queued.id);
    assert!(control.take_telemetry_observation().is_none());
}

#[test]
fn courtesy_observation_retires_its_own_queued_instance_before_pcm() {
    use super::super::{ControllerSettings, CourtesySettings, NodeController, TelemetryPhase};

    let courtesy = CourtesySettings {
        link: Some(PreparedMedia::new_stream(sequence_reader(&[0.1], false)).unwrap()),
        ..CourtesySettings::default()
    };
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            courtesy_delay_ms: 250,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        courtesy,
    )
    .unwrap();
    node.link_unkeyed("2000", "", false);
    let queued = control.take_telemetry_observation().unwrap();
    assert_eq!(queued.phase, TelemetryPhase::Queued);
    assert_eq!(queued.category, "courtesy");
    assert_eq!(queued.deadline_sample, Some(12_000));
    node.link_keyed("3000");
    assert!(control.take_telemetry_observation().is_none());
    node.link_keyed("2000");
    let cancelled = control.take_telemetry_observation().unwrap();
    assert_eq!(cancelled.phase, TelemetryPhase::Cancelled);
    assert_eq!(cancelled.id, queued.id);
    assert_eq!(cancelled.reason, Some("source_rekeyed"));
    assert!(control.take_telemetry_observation().is_none());
}

#[test]
fn polite_id_observation_retains_its_deadline_without_a_pcm_start() {
    use super::super::{
        ControllerSettings, CourtesySettings, Identifier, NodeController, TelemetryPhase,
    };

    let identifier = Identifier {
        media: PreparedMedia::new_stream(sequence_reader(&[0.2], false)).unwrap(),
        interval_ms: 10,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: Some(300),
    };
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..ControllerSettings::default()
        },
        vec![identifier],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut traffic = [0.125; 960];
    node.process_audio(true, false, &[], &mut traffic);
    let queued = control.take_telemetry_observation().unwrap();
    assert_eq!(queued.category, "id");
    assert_eq!(queued.phase, TelemetryPhase::Queued);
    assert_eq!(queued.deadline_sample, Some(14_880));
    assert!(control.take_telemetry_observation().is_none());
}
