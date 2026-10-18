use super::*;
use std::sync::atomic::AtomicUsize;

fn segment(ring: InboundConsumer, budget: u64) -> SegmentConsumer {
    SegmentConsumer {
        ring,
        progress: Arc::new(SegmentProgress {
            budget: AtomicU64::new(budget),
            undrainable: AtomicBool::new(false),
        }),
        consumed: 0,
    }
}

static READS: AtomicUsize = AtomicUsize::new(0);
static READING: AtomicBool = AtomicBool::new(false);
static RELEASE_READ: AtomicBool = AtomicBool::new(false);

fn composition_tone(frequency: u32) -> MediaSource {
    MediaSource {
        tone: Some(rpt_advanced_core::media::ToneSource {
            sequence: format!("{frequency}/10"),
            level_db: -6,
        }),
        ..MediaSource::default()
    }
}

#[test]
fn composition_boundary_rechecks_late_generation() {
    let (mut current, current_rx) = InboundRing::open(48_000, InboundPolicy::Media).unwrap();
    current.write(&[0.25]).unwrap();
    drop(current);
    let (mut stale, stale_rx) = InboundRing::open(48_000, InboundPolicy::Media).unwrap();
    stale.write(&[0.75; 8]).unwrap();
    drop(stale);
    let (mut ready_tx, ready) = RingBuffer::new(1);
    ready_tx.push((1, 0, segment(stale_rx, 8))).unwrap();
    let (retired, _retired_rx) = RingBuffer::new(1);
    let control = Arc::new(JobControl::default());
    control.requested.store(2, Ordering::Release);
    let mut reader = StreamReader {
        control,
        ready,
        retired,
        generation: 2,
        delay_remaining: 0,
        consumer: Some((2, segment(current_rx, 1))),
        morse_available: true,
        fallback_morse_pending: false,
    };
    let mut output = [0.0; 16];
    assert_eq!(reader.render(&mut output), PcmRead::Samples(1));
    assert_eq!(&output[1..], &[0.0; 15]);
}

#[test]
fn composition_failure_preserves_pending_converter_tail() {
    let (mut producer, consumer) = InboundRing::open(48_000, InboundPolicy::Media).unwrap();
    let (_ready_tx, ready) = RingBuffer::new(1);
    let (retired, _retired_rx) = RingBuffer::new(1);
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    control.failed.store(1, Ordering::Release);
    control.completed.store(1, Ordering::Release);
    let mut reader = StreamReader {
        control,
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, 8))),
        morse_available: false,
        fallback_morse_pending: false,
    };
    // Model converter latency: a temporary empty result is not lost source PCM.
    assert_eq!(reader.render(&mut [0.0; 16]), PcmRead::Pending);
    producer.write(&[0.25; 8]).unwrap();
    drop(producer);
    assert_eq!(reader.render(&mut [0.0; 16]), PcmRead::Samples(8));
    assert_eq!(reader.render(&mut [0.0; 16]), PcmRead::Failed);
}

fn composition_morse(text: &str) -> rpt_advanced_core::media::MorseSource {
    rpt_advanced_core::media::MorseSource {
        text: text.into(),
        speed_wpm: 20,
        frequency_hz: 800.0,
        level_db: -6,
    }
}

fn composition_render(primary: Vec<MediaSource>) -> Vec<f32> {
    let file = fixture_context(fixture(22_050, 441, 0.25), true, false);
    let mut session = StationSession::new("1000".into(), 1, file, context(None)).unwrap();
    let mut reader = session
        .register_program(
            rpt_advanced_core::media::MediaProgram {
                primary,
                morse: vec![],
            },
            false,
        )
        .unwrap();
    session.start().unwrap();
    reader.start();
    collect_samples(reader.as_mut(), &mut [0.0; 97])
}

#[test]
fn composition_file_success_keeps_following_macro() {
    let file = MediaSource {
        file: Some("test.wav".into()),
        ..MediaSource::default()
    };
    let following = composition_tone(1_000);
    let mut expected = composition_render(vec![file.clone()]);
    expected.extend(composition_render(vec![following.clone()]));
    assert_eq!(composition_render(vec![file, following]), expected);
}

#[test]
fn composition_segment_rates_keep_final_samples() {
    let sources = vec![
        composition_tone(500),
        MediaSource {
            file: Some("test.wav".into()),
            ..MediaSource::default()
        },
        composition_tone(1_000),
    ];
    let expected: Vec<_> = sources
        .iter()
        .flat_map(|source| composition_render(vec![source.clone()]))
        .collect();
    assert!(expected.len() >= 1_920);
    assert_eq!(composition_render(sources), expected);
}

#[test]
fn composition_fractional_source_duration_excludes_flush_guard() {
    for count in [1, 8, 440, 442] {
        let file = fixture_context(fixture(22_050, count, 0.25), true, false);
        let mut session = StationSession::new("1000".into(), 1, file, context(None)).unwrap();
        let mut reader = session
            .register(MediaSource {
                file: Some("test.wav".into()),
                ..MediaSource::default()
            })
            .unwrap();
        session.start().unwrap();
        reader.start();
        assert_eq!(
            collect_samples(reader.as_mut(), &mut [0.0; 97]).len(),
            (count * 48_000).div_ceil(22_050),
            "source samples={count}"
        );
    }
}

#[test]
fn composition_stream_one_completion() {
    let mut session = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    let mut reader = session
        .register_program(
            rpt_advanced_core::media::MediaProgram {
                primary: vec![
                    composition_tone(500),
                    composition_tone(1_000),
                    composition_tone(1_500),
                ],
                morse: vec![],
            },
            false,
        )
        .unwrap();
    session.start().unwrap();
    reader.start();
    let mut count = 0;
    let mut final_samples = false;
    for _ in 0..10_000 {
        match reader.render(&mut [0.0; 31]) {
            PcmRead::Samples(n) => {
                assert!(!final_samples);
                count += n;
            }
            PcmRead::FinalSamples(n) => {
                assert!(!final_samples);
                final_samples = true;
                count += n;
            }
            PcmRead::Pending => thread::sleep(Duration::from_millis(1)),
            PcmRead::Finished => {
                assert!(count >= 1_440);
                return;
            }
            PcmRead::Failed => panic!("valid program failed"),
        }
    }
    panic!("program did not complete");
}

#[test]
fn composition_receive_restarts_full_morse() {
    let morse = vec![composition_morse("E"), composition_morse("T")];
    let expected: Vec<_> = morse
        .iter()
        .flat_map(|source| {
            composition_render(vec![MediaSource {
                morse: Some(source.clone()),
                ..MediaSource::default()
            }])
        })
        .collect();
    let mut session = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    let mut reader = session
        .register_program(
            rpt_advanced_core::media::MediaProgram {
                primary: vec![composition_tone(500), composition_tone(1_000)],
                morse,
            },
            false,
        )
        .unwrap();
    session.start().unwrap();
    reader.start();
    let mut heard = false;
    for _ in 0..10_000 {
        if matches!(reader.render(&mut [0.0; 17]), PcmRead::Samples(_)) {
            heard = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(heard);
    assert_ne!(reader.select_morse_fallback(), MorseSwitch::Unavailable);
    assert_eq!(collect_samples(reader.as_mut(), &mut [0.0; 97]), expected);
}

#[test]
fn composition_attention_positions_survive_speech_fallback() {
    let message = MediaSource {
        speech: Some(rpt_advanced_core::media::SpeechSource {
            text: "unavailable".into(),
            model: "missing.onnx".into(),
            speed_percent: 100,
            level_db: -6,
        }),
        morse: Some(composition_morse("E")),
        ..MediaSource::default()
    };
    let sources = vec![
        composition_tone(500),
        message.clone(),
        composition_tone(1_000),
        message,
        composition_tone(1_500),
    ];
    let expected: Vec<_> = sources
        .iter()
        .flat_map(|source| composition_render(vec![source.clone()]))
        .collect();
    assert_eq!(composition_render(sources), expected);
}

#[test]
fn composition_reuses_complete_program() {
    let sources = vec![composition_tone(500), composition_tone(1_000)];
    let expected = composition_render(sources.clone());
    let mut session = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    let mut reader = session
        .register_program(
            rpt_advanced_core::media::MediaProgram {
                primary: sources,
                morse: vec![],
            },
            false,
        )
        .unwrap();
    session.start().unwrap();
    for _ in 0..2 {
        reader.start();
        assert_eq!(collect_samples(reader.as_mut(), &mut [0.0; 97]), expected);
        reader.cancel();
    }
}

#[test]
fn composition_one_shot_rewind_keeps_its_producer_until_completion() {
    let sources = vec![composition_tone(500), composition_tone(1_000)];
    let expected = composition_render(sources.clone());
    let mut session = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    let mut reader = session
        .register_program(
            rpt_advanced_core::media::MediaProgram {
                primary: sources,
                morse: vec![],
            },
            true,
        )
        .unwrap();
    session.start().unwrap();
    reader.start();
    let mut heard = false;
    for _ in 0..10_000 {
        if matches!(reader.render(&mut [0.0; 17]), PcmRead::Samples(_)) {
            heard = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(heard);
    reader.rewind();
    assert_eq!(collect_samples(reader.as_mut(), &mut [0.0; 97]), expected);
}

#[test]
fn composition_pending_successor_keeps_playback_active() {
    let (mut producer, consumer) = InboundRing::open(48_000, InboundPolicy::Media).unwrap();
    producer.write(&[0.25; 8]).unwrap();
    drop(producer);
    let control = Arc::new(JobControl::default());
    let (mut ready_tx, ready) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    ready_tx.push((1, 0, segment(consumer, 8))).unwrap();
    let reader = StreamReader {
        control,
        ready,
        retired,
        generation: 0,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };
    let mut playback = rpt_advanced_core::audio::Playback::new_stream(Box::new(reader));
    assert_eq!(playback.render(false, &mut [0.0; 16]), 8);
    drop(retired_rx.pop().unwrap());
    assert_eq!(playback.render(false, &mut [0.0; 16]), 0);
    assert!(playback.waiting_for_stream());
    assert_eq!(
        playback.outcome(),
        rpt_advanced_core::audio::PlaybackOutcome::Active
    );
    let (mut producer, consumer) = InboundRing::open(48_000, InboundPolicy::Media).unwrap();
    producer.write(&[0.5; 8]).unwrap();
    drop(producer);
    ready_tx.push((1, 0, segment(consumer, 8))).unwrap();
    assert_eq!(playback.render(false, &mut [0.0; 16]), 8);
    assert!(!playback.is_finished());
}

#[test]
fn morse_reused_stream_restart() {
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, _retired_rx) = RingBuffer::new(1);
    let reader = StreamReader {
        control: Arc::clone(&control),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: None,
        morse_available: true,
        fallback_morse_pending: false,
    };
    let mut playback = rpt_advanced_core::audio::Playback::new_stream(Box::new(reader));
    playback.restart(true);
    assert_eq!(playback.render(false, &mut [0.0]), 0);
    assert_eq!(
        control.fallback_morse.load(Ordering::Acquire),
        control.requested.load(Ordering::Acquire),
        "restart must keep the selected Morse generation"
    );
}

#[test]
fn morse_pending_retirement_gates_primary() {
    let (_, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut retired, mut retired_rx) = RingBuffer::new(1);
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let (_ready, ready) = RingBuffer::new(1);
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    let reader = StreamReader {
        control: Arc::clone(&control),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, u64::MAX))),
        morse_available: true,
        fallback_morse_pending: false,
    };
    let mut playback = rpt_advanced_core::audio::Playback::new_stream(Box::new(reader));
    assert_eq!(playback.render(true, &mut [0.0]), 0);
    assert!(
        !playback.is_finished(),
        "retirement pressure is not a missing Morse fallback"
    );
    drop(retired_rx.pop().unwrap());
    assert_eq!(playback.render(false, &mut [0.0]), 0);
    assert!(!playback.is_finished());
    assert_eq!(
        control.fallback_morse.load(Ordering::Acquire),
        control.requested.load(Ordering::Acquire)
    );
}

#[test]
fn morse_late_primary_writes_rejected() {
    let (mut old_producer, old_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut late_producer, late_consumer) =
        InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut ready_tx, ready) = RingBuffer::new(2);
    let (retired, mut retired_rx) = RingBuffer::new(2);
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    let mut reader = StreamReader {
        control: Arc::clone(&control),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(old_consumer, u64::MAX))),
        morse_available: true,
        fallback_morse_pending: false,
    };
    reader.select_morse_fallback();
    old_producer.write(&[0.75; 64]).unwrap();
    late_producer.write(&[0.75; 64]).unwrap();
    ready_tx
        .push((1, 0, segment(late_consumer, u64::MAX)))
        .unwrap();
    let mut output = [0.0; 16];
    assert_eq!(reader.render(&mut output), PcmRead::Pending);
    assert_eq!(output, [0.0; 16]);
    assert_eq!(retired_rx.pop().unwrap().0, 1);
    assert_eq!(retired_rx.pop().unwrap().0, 1);
    assert_eq!(control.requested.load(Ordering::Acquire), 2);
}

#[test]
fn morse_failed_stream_tail_is_not_success() {
    for provider_failure in [false, true] {
        let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
        producer.write(&[0.25; 8]).unwrap();
        let control = Arc::new(JobControl::default());
        control.requested.store(1, Ordering::Release);
        control.completed.store(1, Ordering::Release);
        if provider_failure {
            control.failed.store(1, Ordering::Release);
        }
        let (_ready, ready) = RingBuffer::new(1);
        let (retired, _retired_rx) = RingBuffer::new(1);
        let consumer = if provider_failure {
            consumer
        } else {
            crate::link::ring::tests::sample_then_fail_consumer()
        };
        let mut reader = StreamReader {
            control,
            ready,
            retired,
            generation: 1,
            delay_remaining: 0,
            consumer: Some((
                1,
                segment(consumer, if provider_failure { 8 } else { u64::MAX }),
            )),
            morse_available: false,
            fallback_morse_pending: false,
        };
        assert!(matches!(reader.render(&mut [0.0; 64]), PcmRead::Samples(n) if n > 0));
        assert_eq!(reader.render(&mut [0.0; 64]), PcmRead::Failed);
    }
}

#[test]
fn morse_switch_pending_cannot_retire_a_one_shot_job() {
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    control.completed.store(1, Ordering::Release);
    let (ready_tx, ready) = RingBuffer::new(1);
    let (mut retired, retired_rx) = RingBuffer::new(1);
    let (_, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_, primary) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let mut reader = StreamReader {
        control: Arc::clone(&control),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(primary, u64::MAX))),
        morse_available: true,
        fallback_morse_pending: false,
    };
    let mut job = WorkerJob {
        source: WorkerSource::Program(MediaProgram::single(MediaSource::default())),
        control,
        ready: ready_tx,
        retired: retired_rx,
        handled: 1,
        one_shot: true,
    };
    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Pending);
    let selected = job.control.requested.load(Ordering::Acquire);
    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Pending);
    assert_eq!(job.control.requested.load(Ordering::Acquire), selected);
    assert_eq!(job.control.fallback_morse.load(Ordering::Acquire), selected);
    drop(job.retired.pop().unwrap());
    job.control.consumer_outstanding.store(0, Ordering::Release);
    assert!(
        !one_shot_done(&job),
        "pending Morse selection is not terminal cancellation"
    );
}

#[test]
fn morse_replacement_waits_for_late_primary_consumer() {
    let (mut session, marker, mut marker_reader) = registered_file_job(fixture(48000, 0, 0.0));
    marker_reader.start();
    let marker_control = Arc::clone(&marker.control);
    let control = Arc::new(JobControl::default());
    control.requested.store(2, Ordering::Release);
    control.fallback_morse.store(2, Ordering::Release);
    control.consumer_outstanding.store(1, Ordering::Release);
    let (mut ready_tx, ready) = RingBuffer::new(1);
    let (retired, retired_rx) = RingBuffer::new(1);
    let (mut old_producer, old_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    old_producer.write(&[0.75; 64]).unwrap();
    ready_tx
        .push((1, 0, segment(old_consumer, u64::MAX)))
        .unwrap();
    let mut reader = StreamReader {
        control: Arc::clone(&control),
        ready,
        retired,
        generation: 2,
        delay_remaining: 0,
        consumer: None,
        morse_available: true,
        fallback_morse_pending: false,
    };
    let job = WorkerJob {
        source: WorkerSource::Program(MediaProgram::single(MediaSource {
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        })),
        control: Arc::clone(&control),
        ready: ready_tx,
        retired: retired_rx,
        handled: 1,
        one_shot: true,
    };
    let (_requests, requests) = RingBuffer::new(1);
    let file = Arc::clone(&session.file);
    let speech = Arc::clone(&session.speech);
    let stopping = Arc::clone(&session.stopping);
    session.worker = Some(thread::spawn(move || {
        worker_loop(vec![job, marker], requests, file, speech, stopping);
    }));
    // The second job confirms that the worker visited the replacement before audio drains ready.
    for _ in 0..1000 {
        if marker_control.completed.load(Ordering::Acquire) == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(marker_control.completed.load(Ordering::Acquire), 1);
    assert_eq!(
        control.failed.load(Ordering::Acquire),
        0,
        "a late primary endpoint must not fail replacement publication"
    );
    let pcm = collect_samples(&mut reader, &mut [0.0; 64]);
    assert!((2880..=2882).contains(&pcm.len()));
    assert!(
        pcm.iter().all(|sample| sample.abs() < 0.6),
        "no old primary PCM"
    );
}

#[test]
fn morse_final_samples_retire_producer_without_an_extra_read() {
    let control = Arc::new(JobControl::default());
    control.completed.store(1, Ordering::Release);
    let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    producer.write(&[0.25; 8]).unwrap();
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    let reader = StreamReader {
        control: Arc::clone(&control),
        ready,
        retired,
        generation: 0,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, 8))),
        morse_available: true,
        fallback_morse_pending: false,
    };
    let mut playback = rpt_advanced_core::audio::Playback::new_stream(Box::new(reader));
    assert!(playback.render(false, &mut [0.0; 64]) > 0);
    assert_eq!(
        playback.outcome(),
        rpt_advanced_core::audio::PlaybackOutcome::Succeeded
    );
    assert_eq!(control.cancelled.load(Ordering::Acquire), 1);
    assert_eq!(retired_rx.pop().unwrap().0, 1);
}

struct Fixture {
    open_code: i32,
    null_handle: bool,
    rate: u32,
    first_read_code: i32,
    later_read_code: i32,
    first_count: usize,
    later_count: usize,
    invalid_count: bool,
    sample: f32,
    invalid_sample: bool,
    nan_sample: bool,
    cancel_after_first_read: bool,
    cancel_after_later_read: bool,
    reads: AtomicUsize,
}

fn fixture(rate: u32, first_count: usize, sample: f32) -> Fixture {
    Fixture {
        open_code: 0,
        null_handle: false,
        rate,
        first_read_code: 0,
        later_read_code: 0,
        first_count,
        later_count: 0,
        invalid_count: false,
        sample,
        invalid_sample: false,
        nan_sample: false,
        cancel_after_first_read: false,
        cancel_after_later_read: false,
        reads: AtomicUsize::new(0),
    }
}

#[test]
fn job_cancellation_checks_stop_generation_and_explicit_cancel() {
    let control = JobControl::default();
    let stopping = AtomicBool::new(false);
    control.requested.store(7, Ordering::Release);
    assert!(!control.is_cancelled(&stopping, 7));
    assert!(control.is_cancelled(&stopping, 8));

    control.cancelled.store(7, Ordering::Release);
    assert!(control.is_cancelled(&stopping, 7));
    control.cancelled.store(0, Ordering::Release);
    stopping.store(true, Ordering::Release);
    assert!(control.is_cancelled(&stopping, 7));
}

fn fixture_context(fixture: Fixture, open_file: bool, open_speech: bool) -> Arc<Context> {
    let handle = Box::into_raw(Box::new(fixture)).cast::<c_void>();
    Arc::new(Context {
        // SAFETY: Box::into_raw returns a non-null aligned fixture pointer.
        handle: unsafe { std::ptr::NonNull::new_unchecked(handle) },
        destroy: destroy_fixture_context,
        read_stream: read_fixture_stream,
        close_stream: close_test_stream,
        open_file: open_file.then_some(open_fixture_file),
        open_speech: open_speech.then_some(open_fixture_speech),
    })
}

fn registered_file_job(file: Fixture) -> (StationSession, WorkerJob, Box<dyn PcmStreamReader>) {
    let file = fixture_context(file, true, false);
    let speech = context(None);
    let mut session =
        StationSession::new("1000".into(), 1, Arc::clone(&file), Arc::clone(&speech)).unwrap();
    let reader = session
        .register(MediaSource {
            file: Some("test.wav".into()),
            ..MediaSource::default()
        })
        .unwrap();
    let job = session.jobs.pop().unwrap();
    (session, job, reader)
}

fn failed_ring_open(
    _: u32,
    _: InboundPolicy,
) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError> {
    Err(crate::link::ring::RingError)
}

fn failed_delay_open(
    _: u32,
    _: InboundPolicy,
) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError> {
    Ok(crate::link::ring::tests::failed_output_delay_endpoints())
}

fn failed_push_open(
    _: u32,
    _: InboundPolicy,
) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError> {
    Ok(crate::link::ring::tests::failed_endpoints())
}

fn fail_after_initial_push_open(
    _: u32,
    _: InboundPolicy,
) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError> {
    Ok(crate::link::ring::tests::fail_after_initial_push_endpoints())
}

unsafe extern "C" fn destroy_fixture_context(handle: *mut c_void) {
    // SAFETY: fixture_context transfers exactly one Box allocation to this callback.
    drop(unsafe { Box::from_raw(handle.cast::<Fixture>()) });
}

unsafe fn open_fixture(context: *const c_void, output: *mut ffi::rptadv_media_stream) -> i32 {
    // SAFETY: the owning Context keeps its fixture alive through the synchronous call.
    let fixture = unsafe { &*context.cast::<Fixture>() };
    if fixture.open_code != 0 {
        return fixture.open_code;
    }
    unsafe {
        (*output).handle = if fixture.null_handle {
            ptr::null_mut()
        } else {
            context.cast_mut()
        };
        (*output).sample_rate_hz = fixture.rate;
    }
    0
}

unsafe extern "C" fn open_fixture_file(
    context: *const c_void,
    _: *const std::ffi::c_char,
    _: *const ffi::rptadv_media_cancellation,
    output: *mut ffi::rptadv_media_stream,
) -> i32 {
    // SAFETY: forwarded unchanged to the fixture's synchronous open helper.
    unsafe { open_fixture(context, output) }
}

unsafe extern "C" fn open_null_handle_file(
    _: *const c_void,
    _: *const std::ffi::c_char,
    _: *const ffi::rptadv_media_cancellation,
    output: *mut ffi::rptadv_media_stream,
) -> i32 {
    // SAFETY: the test provides a live, writable stream result.
    unsafe {
        (*output).handle = ptr::null_mut();
        (*output).sample_rate_hz = 48_000;
    }
    0
}

unsafe extern "C" fn open_fixture_speech(
    context: *const c_void,
    _: *const ffi::rptadv_media_speech_request,
    _: *const ffi::rptadv_media_cancellation,
    output: *mut ffi::rptadv_media_stream,
) -> i32 {
    // SAFETY: forwarded unchanged to the fixture's synchronous open helper.
    unsafe { open_fixture(context, output) }
}

unsafe extern "C" fn read_fixture_stream(
    handle: *mut c_void,
    cancellation: *const ffi::rptadv_media_cancellation,
    output: *mut f32,
    capacity: usize,
    count: *mut usize,
) -> i32 {
    // SAFETY: the stream handle borrows its owning fixture Context.
    let fixture = unsafe { &*handle.cast::<Fixture>() };
    let first = fixture.reads.fetch_add(1, Ordering::AcqRel) == 0;
    let code = if first {
        fixture.first_read_code
    } else {
        fixture.later_read_code
    };
    if code != 0 {
        unsafe { *count = 0 };
        return code;
    }
    if (first && fixture.cancel_after_first_read) || (!first && fixture.cancel_after_later_read) {
        // SAFETY: worker open/read callbacks borrow the live worker cancellation value.
        let cancellation = unsafe { &*(*cancellation).context.cast::<Cancellation<'_>>() };
        cancellation
            .control
            .cancelled
            .store(cancellation.generation, Ordering::Release);
    }
    let samples = if first {
        fixture.first_count
    } else {
        fixture.later_count
    };
    unsafe {
        *count = if fixture.invalid_count {
            capacity + 1
        } else {
            samples
        };
        if !fixture.invalid_count {
            std::slice::from_raw_parts_mut(output, samples).fill(if fixture.nan_sample {
                f32::NAN
            } else if fixture.invalid_sample {
                2.0
            } else {
                fixture.sample
            });
        }
    }
    0
}

unsafe extern "C" fn open_test_file(
    _: *const c_void,
    _: *const std::ffi::c_char,
    _: *const ffi::rptadv_media_cancellation,
    output: *mut ffi::rptadv_media_stream,
) -> i32 {
    unsafe {
        (*output).handle = 1_usize as *mut c_void;
        (*output).sample_rate_hz = 48000;
    }
    0
}

unsafe extern "C" fn read_test_file(
    _: *mut c_void,
    cancellation: *const ffi::rptadv_media_cancellation,
    output: *mut f32,
    capacity: usize,
    count: *mut usize,
) -> i32 {
    if READS.fetch_add(1, Ordering::AcqRel) == 0 {
        unsafe {
            std::slice::from_raw_parts_mut(output, capacity).fill(0.25);
            *count = capacity;
        }
        return 0;
    }
    READING.store(true, Ordering::Release);
    while !RELEASE_READ.load(Ordering::Acquire) {
        let cancelled = unsafe { &*cancellation };
        if unsafe {
            cancelled
                .is_cancelled
                .is_some_and(|check| check(cancelled.context) != 0)
        } {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    unsafe { *count = 0 };
    0
}

unsafe extern "C" fn close_test_stream(_: *mut c_void) {}
unsafe extern "C" fn destroy_test_context(_: *mut c_void) {}

struct ReleaseRead;
impl Drop for ReleaseRead {
    fn drop(&mut self) {
        RELEASE_READ.store(true, Ordering::Release);
    }
}

fn context(
    open_file: Option<
        unsafe extern "C" fn(
            *const c_void,
            *const std::ffi::c_char,
            *const ffi::rptadv_media_cancellation,
            *mut ffi::rptadv_media_stream,
        ) -> i32,
    >,
) -> Arc<Context> {
    Arc::new(Context {
        handle: std::ptr::NonNull::new(1_usize as *mut c_void).unwrap(),
        destroy: destroy_test_context,
        read_stream: read_test_file,
        close_stream: close_test_stream,
        open_file,
        open_speech: None,
    })
}

#[test]
fn producer_streams_first_pcm_while_provider_is_still_decoding() {
    READS.store(0, Ordering::Release);
    READING.store(false, Ordering::Release);
    RELEASE_READ.store(false, Ordering::Release);
    let _release = ReleaseRead;
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();
    let mut reader = session
        .register(MediaSource {
            file: Some("memory.wav".into()),
            ..MediaSource::default()
        })
        .unwrap();
    session.start().unwrap();
    reader.start();

    let mut output = [0.0; 64];
    let mut streamed = false;
    for _ in 0..1000 {
        if matches!(reader.render(&mut output), PcmRead::Samples(count) if count != 0) {
            streamed = READING.load(Ordering::Acquire) && (output[0] - 0.25).abs() < 0.003;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        streamed,
        "audio must reach the consumer before provider EOF; reads={}, reading={}",
        READS.load(Ordering::Acquire),
        READING.load(Ordering::Acquire)
    );
    RELEASE_READ.store(true, Ordering::Release);
    let mut finished = false;
    for _ in 0..1000 {
        match reader.render(&mut output) {
            PcmRead::Samples(_) | PcmRead::FinalSamples(_) | PcmRead::Pending => {
                thread::sleep(Duration::from_millis(1));
            }
            PcmRead::Finished => {
                finished = true;
                break;
            }
            PcmRead::Failed => panic!("valid streamed media failed"),
        }
    }
    assert!(
        finished,
        "stream must finish after source EOF and ring drain"
    );
}

#[test]
fn station_worker_starts_even_without_preconfigured_media() {
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();

    session.start().unwrap();

    assert!(session.worker.is_some());
}

#[test]
fn startup_source_count_is_not_limited_by_live_status_capacity() {
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();
    let readers = (0..17)
        .map(|_| {
            session
                .register(MediaSource {
                    file: Some("memory.wav".into()),
                    ..MediaSource::default()
                })
                .unwrap()
        })
        .collect::<Vec<_>>();

    assert_eq!(readers.len(), 17);
}

#[test]
fn file_open_failure_falls_back_to_streamed_speech_with_provider_gain() {
    let mut file = fixture(48000, 0, 0.0);
    file.open_code = 1;
    let file = fixture_context(file, true, false);
    let speech = fixture_context(fixture(48000, 16, 0.25), false, true);
    let mut session = StationSession::new("1000".into(), 1, file, speech).unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_once(MediaSource {
            file: Some("missing.wav".into()),
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "fallback".into(),
                model: "voice.onnx".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            provider_gain_db: -6,
            ..MediaSource::default()
        })
        .unwrap();
    reader.start();

    let samples = collect_samples(reader.as_mut(), &mut [0.0; 64]);
    assert!(samples.iter().any(|sample| (*sample - 0.125).abs() < 0.003));
}

#[test]
fn worker_loop_reclaims_consumer_after_stream_generation_is_cancelled() {
    READS.store(0, Ordering::Release);
    READING.store(false, Ordering::Release);
    RELEASE_READ.store(false, Ordering::Release);
    let _release = ReleaseRead;
    let file = context(Some(open_test_file));
    let speech = context(None);
    let mut session =
        StationSession::new("1000".into(), 1, Arc::clone(&file), Arc::clone(&speech)).unwrap();
    let mut reader = session
        .register(MediaSource {
            file: Some("memory.wav".into()),
            ..MediaSource::default()
        })
        .unwrap();
    let job = session.jobs.pop().unwrap();
    reader.start();
    let control = Arc::clone(&job.control);
    let stopping = Arc::new(AtomicBool::new(false));
    let worker_stopping = Arc::clone(&stopping);
    let (_, requests) = RingBuffer::new(1);
    let worker =
        thread::spawn(move || worker_loop(vec![job], requests, file, speech, worker_stopping));

    for _ in 0..1_000 {
        if READING.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(READING.load(Ordering::Acquire));
    assert!(matches!(
        reader.render(&mut [0.0; 64]),
        PcmRead::Samples(64)
    ));
    control.cancelled.store(1, Ordering::Release);
    assert_eq!(reader.render(&mut [0.0; 64]), PcmRead::Pending);

    for _ in 0..1_000 {
        if control.consumer_outstanding.load(Ordering::Acquire) == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(control.consumer_outstanding.load(Ordering::Acquire) == 0);
    stopping.store(true, Ordering::Release);
    worker.join().unwrap();
}

#[test]
fn producer_cancelled_during_a_failed_stream_write_is_not_reported_as_failure() {
    let mut later = fixture(48000, 8, 0.25);
    later.later_count = 8;
    let (session, mut job, mut reader) = registered_file_job(later);
    reader.start();
    crate::link::ring::tests::set_cancel_on_failed_push(&job.control.cancelled, 1, 1);

    run_job(
        &mut job,
        1,
        &session.file,
        &session.speech,
        &session.stopping,
        crate::link::ring::tests::cancel_on_push_open,
    );

    assert_eq!(job.control.cancelled.load(Ordering::Acquire), 1);
    assert_eq!(job.control.failed.load(Ordering::Acquire), 0);
    assert_eq!(job.control.completed.load(Ordering::Acquire), 0);
}

#[test]
fn cancellation_releases_a_completed_job_waiting_for_reader_retirement() {
    let (session, mut job, mut reader) = registered_file_job(fixture(48000, 8, 0.25));
    reader.start();
    let control = Arc::clone(&job.control);
    let file = Arc::clone(&session.file);
    let speech = Arc::clone(&session.speech);
    let stopping = Arc::clone(&session.stopping);
    let worker = thread::spawn(move || {
        run_job(&mut job, 1, &file, &speech, &stopping, InboundRing::open);
    });

    for _ in 0..1_000 {
        if control.completed.load(Ordering::Acquire) == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(control.completed.load(Ordering::Acquire), 1);
    control.cancelled.store(1, Ordering::Release);
    worker.join().unwrap();
    drop(reader);
}

#[test]
fn empty_file_output_falls_back_to_speech() {
    let file = fixture_context(fixture(48000, 0, 0.0), true, false);
    let speech = fixture_context(fixture(48000, 16, 0.25), false, true);
    let mut session = StationSession::new("1000".into(), 1, file, speech).unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_once(MediaSource {
            file: Some("empty.wav".into()),
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "fallback".into(),
                model: "voice.onnx".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();
    reader.start();

    assert!(
        collect_samples(reader.as_mut(), &mut [0.0; 64])
            .iter()
            .any(|sample| (*sample - 0.25).abs() < 0.003)
    );
}

#[test]
fn empty_or_failed_speech_reads_fall_through_to_tone() {
    let mut failed = fixture(48000, 8, 0.25);
    failed.first_read_code = 1;
    for speech in [fixture(48000, 0, 0.0), failed] {
        let mut session = StationSession::new(
            "1000".into(),
            1,
            context(None),
            fixture_context(speech, false, true),
        )
        .unwrap();
        session.start().unwrap();
        let mut reader = session
            .register_once(MediaSource {
                speech: Some(rpt_advanced_core::media::SpeechSource {
                    text: "empty".into(),
                    model: "voice.onnx".into(),
                    speed_percent: 100,
                    level_db: -6,
                }),
                tone: Some(rpt_advanced_core::media::ToneSource {
                    sequence: "500/100".into(),
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .unwrap();
        reader.start();
        assert!(
            collect_samples(reader.as_mut(), &mut [0.0; 64])
                .iter()
                .any(|sample| sample.abs() > 0.1)
        );
    }
}

#[test]
fn invalid_file_stream_outputs_fall_through_to_tone() {
    for file in [
        {
            let mut file = fixture(48000, 8, 0.25);
            file.null_handle = true;
            file
        },
        fixture(7000, 8, 0.25),
        fixture(192001, 8, 0.25),
        {
            let mut file = fixture(48000, 8, 0.25);
            file.invalid_count = true;
            file
        },
        {
            let mut file = fixture(48000, 8, 0.25);
            file.invalid_sample = true;
            file
        },
        {
            let mut file = fixture(48000, 8, 0.25);
            file.nan_sample = true;
            file
        },
        {
            let mut file = fixture(48000, 8, 0.25);
            file.first_read_code = 1;
            file
        },
    ] {
        let mut session = StationSession::new(
            "1000".into(),
            1,
            fixture_context(file, true, false),
            context(None),
        )
        .unwrap();
        session.start().unwrap();
        let mut reader = session
            .register_once(MediaSource {
                file: Some("bad.wav".into()),
                tone: Some(rpt_advanced_core::media::ToneSource {
                    sequence: "500/100".into(),
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .unwrap();
        reader.start();

        let samples = collect_samples(reader.as_mut(), &mut [0.0; 64]);
        assert!(samples.iter().any(|sample| sample.abs() > 0.1));
    }
}

#[test]
fn file_stream_open_validates_handles_and_inclusive_rate_bounds() {
    let control = JobControl::default();
    let stopping = AtomicBool::new(false);
    let cancellation = Cancellation {
        control: &control,
        stopping: &stopping,
        generation: 1,
    };
    for file in [fixture(8000, 1, 0.25), fixture(192000, 1, 0.25)] {
        let context = fixture_context(file, true, false);
        assert!(open_file(&context, Path::new("test.wav"), &cancellation).is_ok());
    }
    for file in [fixture(7999, 1, 0.25), fixture(192001, 1, 0.25)] {
        let context = fixture_context(file, true, false);
        assert!(matches!(
            open_file(&context, Path::new("test.wav"), &cancellation),
            Err(MediaError::InvalidOutput)
        ));
    }
    let mut context = fixture_context(fixture(48000, 1, 0.25), true, false);
    Arc::get_mut(&mut context)
        .map(|context| context.open_file = Some(open_null_handle_file))
        .expect("fixture context is unshared");
    assert!(matches!(
        open_file(&context, Path::new("test.wav"), &cancellation),
        Err(MediaError::InvalidOutput)
    ));
}

#[test]
fn stream_validation_requires_a_handle_and_an_inclusive_rate() {
    let handle = std::hint::black_box(std::ptr::NonNull::<u8>::dangling().as_ptr().cast());
    for (handle, rate, valid) in [
        (std::ptr::null_mut(), 48_000, false),
        (handle, 8_000, true),
        (handle, 192_000, true),
        (handle, 7_999, false),
        (handle, 192_001, false),
    ] {
        assert_eq!(
            valid_stream(std::hint::black_box(handle), std::hint::black_box(rate)),
            valid
        );
    }
}

#[test]
fn invalid_speech_rate_falls_back_to_morse() {
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(None),
        fixture_context(fixture(192001, 8, 0.25), false, true),
    )
    .unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_once(MediaSource {
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "fallback".into(),
                model: "voice.onnx".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();
    reader.start();

    assert!((2_880..=2_882).contains(&collect_stream(reader.as_mut(), &mut [0.0; 64])));
}

#[test]
fn pad_test_long_reply_streams_speech_and_morse_fallback() {
    let catalog = rpt_advanced_core::messages::MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap();
    let forms = catalog.format_pad_test(&"*".repeat(127), true).unwrap();
    assert!(forms.tts.len() > 127 && forms.morse.len() > 127);
    for speech_rate in [48000, 192001] {
        let mut session = StationSession::new(
            "1000".into(),
            1,
            context(None),
            fixture_context(fixture(speech_rate, 960, 0.25), false, true),
        )
        .unwrap();
        session.start().unwrap();
        let mut reader = session
            .register_once(MediaSource {
                speech: Some(rpt_advanced_core::media::SpeechSource {
                    text: forms.tts.clone(),
                    model: "voice.onnx".into(),
                    speed_percent: 100,
                    level_db: -6,
                }),
                morse: Some(rpt_advanced_core::media::MorseSource {
                    text: forms.morse.clone(),
                    speed_wpm: 20,
                    frequency_hz: 800.0,
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .unwrap();
        reader.start();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut output = [0.0; 960];
            let _ = reader.render(&mut output);
            if output.iter().any(|sample| *sample != 0.0) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no streamed pad reply at speech rate {speech_rate}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[test]
fn active_producer_streams_tone_then_morse_fallback_on_fresh_registration() {
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();
    session.start().unwrap();
    let source = MediaSource {
        tone: Some(rpt_advanced_core::media::ToneSource {
            sequence: "500/100".into(),
            level_db: -6,
        }),
        morse: Some(rpt_advanced_core::media::MorseSource {
            text: "E".into(),
            speed_wpm: 20,
            frequency_hz: 800.0,
            level_db: -6,
        }),
        ..MediaSource::default()
    };
    let mut reader = session
        .register_once(source.clone())
        .expect("runtime telemetry can be submitted to the active producer");
    reader.start();

    let mut output = [0.0; 64];
    assert!((4_800..=4_802).contains(&collect_stream(reader.as_mut(), &mut output)));
    drop(reader);

    let mut reader = session
        .register_once(source)
        .expect("a fresh telemetry stream can be submitted to the active producer");
    reader.start();
    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Selected);
    assert!((2_880..=2_882).contains(&collect_stream(reader.as_mut(), &mut output)));
}

#[test]
fn receive_present_before_first_playback_selects_morse_fallback() {
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_once(MediaSource {
            tone: Some(rpt_advanced_core::media::ToneSource {
                sequence: "500/100".into(),
                level_db: -6,
            }),
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();

    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Selected);
    reader.start();

    let mut output = [0.0; 64];
    assert!((2_880..=2_882).contains(&collect_stream(reader.as_mut(), &mut output)));
}

fn collect_samples(reader: &mut dyn PcmStreamReader, output: &mut [f32]) -> Vec<f32> {
    let mut samples = Vec::new();
    for _ in 0..10_000 {
        match reader.render(output) {
            PcmRead::Samples(count) | PcmRead::FinalSamples(count) => {
                samples.extend_from_slice(&output[..count]);
            }
            PcmRead::Pending => thread::sleep(Duration::from_millis(1)),
            PcmRead::Finished => return samples,
            PcmRead::Failed => panic!("configured producer source failed"),
        }
    }
    panic!("stream did not finish");
}

fn collect_stream(reader: &mut dyn PcmStreamReader, output: &mut [f32]) -> usize {
    collect_samples(reader, output).len()
}

#[test]
fn prepared_native_parrot_recording_uses_the_station_media_ring() {
    let mut session = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_prepared(PreparedAudio::new(48_000, vec![0.25; 512]).unwrap())
        .unwrap();
    reader.start();

    let samples = collect_samples(reader.as_mut(), &mut [0.0; 64]);
    assert!(samples.len() >= 512);
    assert!(samples.iter().any(|sample| (*sample - 0.25).abs() < 0.003));
}

#[test]
fn prepared_media_rejects_registration_before_start_after_stop_and_out_of_range_rate() {
    let audio = PreparedAudio::new(48_000, vec![0.25; 8]).unwrap();
    let mut before_start =
        StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    assert_eq!(
        before_start.register_prepared(audio.clone()).err(),
        Some(MediaError::InvalidRequest)
    );

    let mut stopped = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    stopped.start().unwrap();
    stopped.stopping.store(true, Ordering::Release);
    assert_eq!(
        stopped.register_prepared(audio).err(),
        Some(MediaError::InvalidRequest)
    );
    drop(stopped);

    let mut invalid_rate =
        StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    invalid_rate.start().unwrap();
    assert_eq!(
        invalid_rate
            .register_prepared(PreparedAudio::new(7_999, vec![0.25; 8]).unwrap())
            .err(),
        Some(MediaError::InvalidRequest)
    );
}

#[test]
fn media_worker_reclaims_retired_consumer_after_one_shot_playback() {
    let (session, mut job, mut reader) = registered_file_job(fixture(48_000, 8, 0.25));
    job.one_shot = true;
    reader.start();
    let control = Arc::clone(&job.control);
    let file = Arc::clone(&session.file);
    let speech = Arc::clone(&session.speech);
    let stopping = Arc::new(AtomicBool::new(false));
    let worker_stopping = Arc::clone(&stopping);
    let (_, requests) = RingBuffer::new(1);
    let worker =
        thread::spawn(move || worker_loop(vec![job], requests, file, speech, worker_stopping));

    assert!(!collect_samples(reader.as_mut(), &mut [0.0; 64]).is_empty());
    for _ in 0..1_000 {
        if control.consumer_outstanding.load(Ordering::Acquire) == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(control.consumer_outstanding.load(Ordering::Acquire) == 0);
    stopping.store(true, Ordering::Release);
    worker.join().unwrap();
}

#[test]
fn reader_fills_requested_output_from_available_ring_samples() {
    let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    assert_eq!(producer.write(&[0.25; 4096]).unwrap(), 4096);
    let delay = producer.output_delay_samples().unwrap();
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    control.completed.store(1, Ordering::Release);
    let (_ready, ready_rx) = RingBuffer::new(1);
    let (retired, _retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control,
        ready: ready_rx,
        retired,
        generation: 1,
        delay_remaining: delay,
        consumer: Some((1, segment(consumer, 4096))),
        morse_available: false,
        fallback_morse_pending: false,
    };
    let mut output = [0.0; 4];

    assert_eq!(reader.render(&mut output), PcmRead::Samples(4));
    assert!(
        output.iter().all(|sample| (*sample - 0.25).abs() < 0.003),
        "delay={delay}; output={output:?}"
    );
}

#[test]
fn reader_returns_pending_or_partial_samples_while_a_live_producer_is_empty() {
    let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, _retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, u64::MAX))),
        morse_available: false,
        fallback_morse_pending: false,
    };
    assert_eq!(reader.render(&mut [0.0; 8]), PcmRead::Pending);

    producer.write(&[0.25; 4096]).unwrap();
    assert!(matches!(reader.render(&mut [0.0; 5000]), PcmRead::Samples(n) if n > 0));
}

#[test]
fn reader_preserves_error_and_completion_states() {
    let failed = crate::link::ring::tests::failed_sample_consumer();
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(failed, u64::MAX))),
        morse_available: false,
        fallback_morse_pending: false,
    };
    assert_eq!(reader.render(&mut [0.0; 1]), PcmRead::Failed);
    drop(retired_rx.pop().unwrap());

    let consumer = crate::link::ring::tests::sample_then_fail_consumer();
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, u64::MAX))),
        morse_available: false,
        fallback_morse_pending: false,
    };
    let mut output = [0.0; 2];
    assert_eq!(reader.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output[0], 0.25);
    assert_eq!(reader.render(&mut output), PcmRead::Failed);
    drop(retired_rx.pop().unwrap());
}

#[test]
fn failed_reader_keeps_its_consumer_when_the_retirement_queue_is_full() {
    let (_unused, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let failed = crate::link::ring::tests::failed_sample_consumer();
    let (mut retired, _retired_rx) = RingBuffer::new(1);
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let (_ready, ready) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(failed, u64::MAX))),
        morse_available: false,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.render(&mut [0.0; 1]), PcmRead::Pending);
    assert!(reader.consumer.is_some());
}

#[test]
fn cancel_retires_a_consumer_published_before_callback_acquired_it() {
    let (_producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    let (mut ready, ready_rx) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    ready.push((1, 0, segment(consumer, u64::MAX))).unwrap();
    let mut reader = StreamReader {
        control,
        ready: ready_rx,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };

    reader.cancel();

    assert_eq!(retired_rx.pop().unwrap().0, 1);
}

#[test]
fn cancel_preserves_ready_owners_when_retirement_is_backpressured() {
    let (_unused, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_unused, ready_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut retired, _retired_rx) = RingBuffer::new(1);
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let (mut ready_tx, ready) = RingBuffer::new(1);
    ready_tx
        .push((1, 0, segment(ready_consumer, u64::MAX)))
        .unwrap();
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };

    reader.cancel();

    assert!(reader.consumer.is_some());
}

#[test]
fn active_fallback_waits_when_a_consumer_cannot_be_retired() {
    let (_unused, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_unused, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut retired, _retired_rx) = RingBuffer::new(1);
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let (_ready, ready) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, u64::MAX))),
        morse_available: true,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Pending);
    assert_eq!(reader.generation, 2);
    assert!(reader.consumer.is_some());
}

#[test]
fn morse_fallback_retains_a_ready_consumer_when_retirement_is_full() {
    let (_unused, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_unused, ready_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut retired, _retired_rx) = RingBuffer::new(1);
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let (mut ready_tx, ready) = RingBuffer::new(1);
    ready_tx
        .push((1, 0, segment(ready_consumer, u64::MAX)))
        .unwrap();
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: None,
        morse_available: true,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Pending);
    assert_eq!(reader.generation, 2);
    assert!(reader.consumer.is_some());
}

#[test]
fn morse_fallback_retires_primary_but_keeps_a_ready_replacement() {
    let (_unused, active_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_unused, ready_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (retired, mut retired_rx) = RingBuffer::new(2);
    let (mut ready_tx, ready) = RingBuffer::new(1);
    ready_tx
        .push((2, 0, segment(ready_consumer, u64::MAX)))
        .unwrap();
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(active_consumer, u64::MAX))),
        morse_available: true,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Selected);
    assert_eq!(reader.generation, 2);
    assert_eq!(
        reader.consumer.as_ref().map(|(generation, _)| *generation),
        Some(2)
    );
    assert_eq!(retired_rx.pop().unwrap().0, 1);
    assert!(retired_rx.pop().is_err());
}

#[test]
fn stream_reader_reports_empty_cancelled_pending_and_failed_states() {
    let control = Arc::new(JobControl::default());
    let (_ready, ready_rx) = RingBuffer::new(1);
    let (retired, _retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::clone(&control),
        ready: ready_rx,
        retired,
        generation: 0,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };
    let mut output = [0.0; 1];

    assert_eq!(reader.render(&mut []), PcmRead::Pending);
    assert_eq!(reader.render(&mut output), PcmRead::Pending);
    reader.start();
    assert_eq!(reader.render(&mut output), PcmRead::Pending);
    control.failed.store(reader.generation, Ordering::Release);
    assert_eq!(reader.render(&mut output), PcmRead::Failed);
    control
        .cancelled
        .store(reader.generation, Ordering::Release);
    assert_eq!(reader.render(&mut output), PcmRead::Pending);
}

#[test]
fn stream_reader_discards_stale_consumers_and_preserves_backpressured_owners() {
    let (mut source, stale_consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    source.write(&[0.5; 8]).unwrap();
    let (mut retired_source, retired_consumer) =
        InboundRing::open(48000, InboundPolicy::Media).unwrap();
    retired_source.write(&[0.25; 8]).unwrap();
    let (mut retired, mut retired_rx) = RingBuffer::new(1);
    retired
        .push((7, segment(retired_consumer, u64::MAX)))
        .unwrap();
    let (mut ready_tx, ready) = RingBuffer::new(1);
    ready_tx
        .push((1, 0, segment(stale_consumer, u64::MAX)))
        .unwrap();
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 2,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };
    let mut output = [0.0; 2];

    assert_eq!(reader.render(&mut output), PcmRead::Pending);
    assert!(reader.consumer.is_some());
    assert!(output.iter().all(|sample| *sample == 0.0));
    assert_eq!(retired_rx.pop().unwrap().0, 7);
    drop((source, retired_source));
}

#[test]
fn acquiring_a_stale_ready_consumer_retains_it_when_reclaimer_is_full() {
    let (_unused, stale) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (_unused, occupied) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut retired, _retired_rx) = RingBuffer::new(1);
    retired.push((0, segment(occupied, u64::MAX))).unwrap();
    let (mut ready_tx, ready) = RingBuffer::new(1);
    ready_tx.push((1, 0, segment(stale, u64::MAX))).unwrap();
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 2,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.render(&mut [0.0; 1]), PcmRead::Pending);
    assert_eq!(reader.consumer.as_ref().unwrap().0, 1);
}

#[test]
fn render_retires_a_consumer_from_an_older_generation() {
    let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    producer.write(&[0.25; 8]).unwrap();
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control: Arc::new(JobControl::default()),
        ready,
        retired,
        generation: 2,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, u64::MAX))),
        morse_available: false,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.render(&mut [0.0; 1]), PcmRead::Pending);
    assert_eq!(retired_rx.pop().unwrap().0, 1);
}

#[test]
fn render_keeps_a_completed_consumer_when_retirement_queue_is_full() {
    let (_producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let (mut retired, mut retired_rx) = RingBuffer::new(1);
    let (_unused_producer, placeholder) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    retired.push((0, segment(placeholder, u64::MAX))).unwrap();
    let control = Arc::new(JobControl::default());
    control.completed.store(1, Ordering::Release);
    let (_ready, ready) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control,
        ready,
        retired,
        generation: 1,
        delay_remaining: 0,
        consumer: Some((1, segment(consumer, 0))),
        morse_available: false,
        fallback_morse_pending: false,
    };

    assert_eq!(reader.render(&mut [0.0; 1]), PcmRead::Pending);
    assert!(reader.consumer.is_some());
    drop(retired_rx.pop().unwrap().1);
    assert_eq!(reader.render(&mut [0.0; 1]), PcmRead::Finished);
}

#[test]
fn fallback_is_unavailable_without_morse_and_pending_until_first_start() {
    let control = Arc::new(JobControl::default());
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, _retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control,
        ready,
        retired,
        generation: 0,
        delay_remaining: 0,
        consumer: None,
        morse_available: false,
        fallback_morse_pending: false,
    };
    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Unavailable);

    reader.morse_available = true;
    assert_eq!(reader.select_morse_fallback(), MorseSwitch::Selected);
    reader.start();
    assert_eq!(reader.control.fallback_morse.load(Ordering::Acquire), 1);
}

#[test]
fn producer_reports_failure_when_all_configured_sources_fail() {
    let mut file = fixture(48000, 0, 0.0);
    file.open_code = 1;
    let mut speech = fixture(48000, 0, 0.0);
    speech.open_code = 1;
    let mut session = StationSession::new(
        "1000".into(),
        1,
        fixture_context(file, true, false),
        fixture_context(speech, false, true),
    )
    .unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_once(MediaSource {
            file: Some("missing.wav".into()),
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "fallback".into(),
                model: "voice.onnx".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();
    reader.start();

    assert_eq!(render_until_terminal(reader.as_mut()), PcmRead::Failed);
}

#[test]
fn empty_morse_output_fails_only_after_every_source_is_exhausted() {
    let mut session = StationSession::new("1000".into(), 1, context(None), context(None)).unwrap();
    session.start().unwrap();
    let mut reader = session
        .register_once(MediaSource {
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: " ".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();
    reader.start();

    assert_eq!(render_until_terminal(reader.as_mut()), PcmRead::Failed);
}

#[test]
fn producer_reports_ring_open_delay_and_initial_write_failures() {
    for open in [failed_ring_open, failed_delay_open, failed_push_open] {
        let (session, mut job, mut reader) = registered_file_job(fixture(48000, 8, 0.25));
        reader.start();
        run_job(
            &mut job,
            1,
            &session.file,
            &session.speech,
            &session.stopping,
            open,
        );
        assert_eq!(job.control.failed.load(Ordering::Acquire), 1);
        assert_eq!(job.control.completed.load(Ordering::Acquire), 1);
        assert!(job.control.consumer_outstanding.load(Ordering::Acquire) == 0);
        assert_eq!(reader.render(&mut [0.0; 64]), PcmRead::Failed);
    }
}

#[test]
fn producer_reports_a_full_ready_queue_without_leaking_its_consumer() {
    let (session, mut job, mut reader) = registered_file_job(fixture(48000, 8, 0.25));
    let (_, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    job.ready.push((0, 0, segment(consumer, u64::MAX))).unwrap();
    reader.start();

    run_job(
        &mut job,
        1,
        &session.file,
        &session.speech,
        &session.stopping,
        InboundRing::open,
    );

    assert_eq!(job.control.failed.load(Ordering::Acquire), 1);
    assert_eq!(job.control.completed.load(Ordering::Acquire), 1);
    assert!(job.control.consumer_outstanding.load(Ordering::Acquire) == 0);
    assert_eq!(reader.render(&mut [0.0; 64]), PcmRead::Failed);
}

#[test]
fn producer_marks_late_read_streaming_write_and_padding_failures() {
    type RingOpener =
        fn(
            u32,
            InboundPolicy,
        ) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError>;

    let mut read_error = fixture(48000, 8, 0.25);
    read_error.later_read_code = 1;
    let mut stream_write_error = fixture(48000, 8, 0.25);
    stream_write_error.later_count = 8;
    let cases: Vec<(Fixture, RingOpener)> = vec![
        (read_error, InboundRing::open),
        (stream_write_error, fail_after_initial_push_open),
        (fixture(48000, 8, 0.25), fail_after_initial_push_open),
    ];
    for (file, open) in cases {
        let (session, mut job, mut reader) = registered_file_job(file);
        reader.start();
        let control = Arc::clone(&job.control);
        let file = Arc::clone(&session.file);
        let speech = Arc::clone(&session.speech);
        let stopping = Arc::clone(&session.stopping);
        let worker = thread::spawn(move || {
            run_job(&mut job, 1, &file, &speech, &stopping, open);
            job
        });

        let mut completed = false;
        for _ in 0..10_000 {
            match reader.render(&mut [0.0; 64]) {
                PcmRead::Pending => thread::sleep(Duration::from_millis(1)),
                PcmRead::Samples(_) | PcmRead::FinalSamples(_) => {}
                PcmRead::Finished | PcmRead::Failed => {
                    completed = true;
                    break;
                }
            }
        }
        assert!(completed, "media ring did not drain after provider failure");

        let job = worker.join().unwrap();
        assert_eq!(control.failed.load(Ordering::Acquire), 1);
        assert_eq!(control.completed.load(Ordering::Acquire), 1);
        assert!(job.control.consumer_outstanding.load(Ordering::Acquire) == 0);
    }
}

#[test]
fn producer_write_retries_zero_and_partial_acceptance() {
    let control = JobControl::default();
    control.requested.store(1, Ordering::Release);
    let stopping = AtomicBool::new(false);
    let cancellation = Cancellation {
        control: &control,
        stopping: &stopping,
        generation: 1,
    };
    for (mut producer, _) in [
        crate::link::ring::tests::zero_then_push_endpoints(),
        crate::link::ring::tests::partial_then_push_endpoints(),
    ] {
        assert!(push_all(&mut producer, &[0.25; 8], &cancellation));
    }
}

#[test]
fn producer_cancellation_stops_initial_and_later_write_paths_without_failure() {
    let mut initial = fixture(48000, 8, 0.25);
    initial.cancel_after_first_read = true;
    let (session, mut job, mut reader) = registered_file_job(initial);
    reader.start();
    run_job(
        &mut job,
        1,
        &session.file,
        &session.speech,
        &session.stopping,
        InboundRing::open,
    );
    assert_eq!(job.control.cancelled.load(Ordering::Acquire), 1);
    assert_eq!(job.control.failed.load(Ordering::Acquire), 0);
    assert!(job.control.consumer_outstanding.load(Ordering::Acquire) == 0);
    assert_eq!(job.control.completed.load(Ordering::Acquire), 0);

    let mut later = fixture(48000, 8, 0.25);
    later.cancel_after_later_read = true;
    let (session, mut job, mut reader) = registered_file_job(later);
    reader.start();
    run_job(
        &mut job,
        1,
        &session.file,
        &session.speech,
        &session.stopping,
        InboundRing::open,
    );
    assert_eq!(job.control.cancelled.load(Ordering::Acquire), 1);
    assert_eq!(job.control.failed.load(Ordering::Acquire), 0);
    assert_eq!(job.control.completed.load(Ordering::Acquire), 0);
}

#[test]
fn producer_cancellation_after_initial_and_padding_writes_is_not_failure() {
    let (session, mut job, mut reader) = registered_file_job(fixture(48000, 8, 0.25));
    reader.start();
    crate::link::ring::tests::set_cancel_on_push(&job.control.cancelled, 1, 0, false);
    run_job(
        &mut job,
        1,
        &session.file,
        &session.speech,
        &session.stopping,
        crate::link::ring::tests::cancel_on_push_open,
    );
    assert_eq!(job.control.cancelled.load(Ordering::Acquire), 1);
    assert_eq!(job.control.failed.load(Ordering::Acquire), 0);

    let (session, mut job, mut reader) = registered_file_job(fixture(48000, 8, 0.25));
    reader.start();
    crate::link::ring::tests::set_cancel_on_push(&job.control.cancelled, 1, 1, true);
    run_job(
        &mut job,
        1,
        &session.file,
        &session.speech,
        &session.stopping,
        crate::link::ring::tests::cancel_on_push_open,
    );
    assert_eq!(job.control.cancelled.load(Ordering::Acquire), 1);
    assert_eq!(job.control.failed.load(Ordering::Acquire), 0);
    assert_eq!(job.control.completed.load(Ordering::Acquire), 0);
}

#[test]
fn cancellation_during_an_empty_provider_read_does_not_publish_failure_or_fallback() {
    let mut file = fixture(48000, 0, 0.0);
    file.cancel_after_first_read = true;
    let mut session = StationSession::new(
        "1000".into(),
        1,
        fixture_context(file, true, false),
        fixture_context(fixture(48000, 8, 0.25), false, true),
    )
    .unwrap();
    let mut reader = session
        .register_once(MediaSource {
            file: Some("memory.wav".into()),
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "cancelled".into(),
                model: "voice.onnx".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            tone: Some(rpt_advanced_core::media::ToneSource {
                sequence: "500/100".into(),
                level_db: -6,
            }),
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();
    session.start().unwrap();
    reader.start();
    // SAFETY: the session retains both fixture contexts until this test ends.
    let file = unsafe { &*session.file.handle.as_ptr().cast::<Fixture>() };
    // SAFETY: the session retains both fixture contexts until this test ends.
    let speech = unsafe { &*session.speech.handle.as_ptr().cast::<Fixture>() };
    for _ in 0..1000 {
        if file.reads.load(Ordering::Acquire) != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(file.reads.load(Ordering::Acquire), 1);
    assert_eq!(speech.reads.load(Ordering::Acquire), 0);
    assert_eq!(reader.render(&mut [0.0; 64]), PcmRead::Pending);
}

fn render_until_terminal(reader: &mut dyn PcmStreamReader) -> PcmRead {
    for _ in 0..1000 {
        match reader.render(&mut [0.0; 64]) {
            PcmRead::Pending => thread::sleep(Duration::from_millis(1)),
            result => return result,
        }
    }
    panic!("media producer did not reach a terminal state");
}

#[test]
fn stream_reader_marks_partial_final_audio_and_retires_after_drain() {
    let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    producer.write(&[0.25; 8]).unwrap();
    let control = Arc::new(JobControl::default());
    control.requested.store(1, Ordering::Release);
    control.completed.store(1, Ordering::Release);
    let (_ready, ready) = RingBuffer::new(1);
    let (retired, mut retired_rx) = RingBuffer::new(1);
    let mut reader = StreamReader {
        control,
        ready,
        retired,
        generation: 1,
        delay_remaining: 2,
        consumer: Some((1, segment(consumer, 8))),
        morse_available: false,
        fallback_morse_pending: false,
    };
    let mut output = [0.0; 32];

    assert!(matches!(reader.render(&mut output), PcmRead::FinalSamples(n) if n != 0));
    assert_eq!(reader.render(&mut output), PcmRead::Finished);
    assert_eq!(retired_rx.pop().unwrap().0, 1);
}

#[test]
fn one_shot_registration_validates_sources_queue_and_session_state() {
    assert_eq!(
        StationSession::new(String::new(), 1, context(None), context(None)).err(),
        Some(MediaError::InvalidRequest)
    );
    assert_eq!(
        StationSession::new("1000".into(), 0, context(None), context(None)).err(),
        Some(MediaError::InvalidRequest)
    );
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();
    assert_eq!(
        session.register(MediaSource::default()).err(),
        Some(MediaError::InvalidRequest)
    );
    for _ in 0..4 {
        session
            .register_once(MediaSource {
                morse: Some(rpt_advanced_core::media::MorseSource {
                    text: "E".into(),
                    speed_wpm: 20,
                    frequency_hz: 800.0,
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .unwrap();
    }
    assert_eq!(
        session
            .register_once(MediaSource {
                morse: Some(rpt_advanced_core::media::MorseSource {
                    text: "E".into(),
                    speed_wpm: 20,
                    frequency_hz: 800.0,
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .err(),
        Some(MediaError::QueueFull)
    );
}

#[test]
fn one_shot_jobs_finish_only_after_terminal_state_and_consumer_release() {
    let (_, mut job, _reader) = registered_file_job(fixture(48000, 8, 0.25));
    assert!(!one_shot_done(&job));

    job.one_shot = true;
    assert!(!one_shot_done(&job));
    job.control.abandoned.store(true, Ordering::Release);
    assert!(one_shot_done(&job));
    job.control.abandoned.store(false, Ordering::Release);

    job.handled = 1;
    assert!(!one_shot_done(&job));
    job.control.requested.store(1, Ordering::Release);
    job.control.completed.store(1, Ordering::Release);
    assert!(!one_shot_done(&job));
    job.control.completed.store(0, Ordering::Release);
    job.control.cancelled.store(1, Ordering::Release);
    assert!(one_shot_done(&job));
    job.control.cancelled.store(0, Ordering::Release);
    assert!(!one_shot_done(&job));
    job.control.consumer_outstanding.store(1, Ordering::Release);
    assert!(!one_shot_done(&job));
}

#[test]
fn session_rejects_invalid_media_and_duplicate_start_or_persistent_registration() {
    let mut session = StationSession::new(
        "1000".into(),
        1,
        context(Some(open_test_file)),
        context(None),
    )
    .unwrap();
    for source in [
        MediaSource {
            file: Some("bad\0path".into()),
            ..MediaSource::default()
        },
        MediaSource {
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "bad\0text".into(),
                model: "model".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            ..MediaSource::default()
        },
        MediaSource {
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "text".into(),
                model: "model\0path".into(),
                speed_percent: 100,
                level_db: -6,
            }),
            ..MediaSource::default()
        },
        MediaSource {
            speech: Some(rpt_advanced_core::media::SpeechSource {
                text: "text".into(),
                model: "model".into(),
                speed_percent: 0,
                level_db: -6,
            }),
            ..MediaSource::default()
        },
        MediaSource {
            provider_gain_db: 1,
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        },
        MediaSource {
            tone: Some(rpt_advanced_core::media::ToneSource {
                sequence: "invalid".into(),
                level_db: -6,
            }),
            ..MediaSource::default()
        },
        MediaSource {
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 0,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        },
    ] {
        assert_eq!(
            session.register(source).err(),
            Some(MediaError::InvalidRequest)
        );
    }
    session
        .register(MediaSource {
            morse: Some(rpt_advanced_core::media::MorseSource {
                text: "E".into(),
                speed_wpm: 20,
                frequency_hz: 800.0,
                level_db: -6,
            }),
            ..MediaSource::default()
        })
        .unwrap();
    session.start().unwrap();
    assert_eq!(session.start().err(), Some(MediaError::InvalidRequest));
    assert_eq!(
        session
            .register(MediaSource {
                morse: Some(rpt_advanced_core::media::MorseSource {
                    text: "E".into(),
                    speed_wpm: 20,
                    frequency_hz: 800.0,
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .err(),
        Some(MediaError::InvalidRequest)
    );
    session.stopping.store(true, Ordering::Release);
    assert_eq!(
        session
            .register(MediaSource {
                morse: Some(rpt_advanced_core::media::MorseSource {
                    text: "E".into(),
                    speed_wpm: 20,
                    frequency_hz: 800.0,
                    level_db: -6,
                }),
                ..MediaSource::default()
            })
            .err(),
        Some(MediaError::InvalidRequest)
    );
}
