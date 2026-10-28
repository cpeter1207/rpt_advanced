use super::*;
use rpt_advanced_core::{
    link::{HumanMessage, TextAudience, TextOrigin},
    media::SpeechSource,
};
use std::{
    ffi::{CStr, c_void},
    path::Path,
    ptr::NonNull,
    sync::Arc,
};

#[derive(Default)]
pub(crate) struct SpeechFixture {
    release: AtomicBool,
    pub(crate) opened: Mutex<Vec<String>>,
    active: AtomicUsize,
    maximum: AtomicUsize,
}
struct StreamFixture {
    state: Arc<SpeechFixture>,
    text: String,
    read: usize,
}

pub(crate) fn preparer(state: Arc<SpeechFixture>) -> NativeMediaPreparer {
    unsafe extern "C" fn open(
        context: *const c_void,
        request: *const super::super::ffi::rptadv_media_speech_request,
        _: *const super::super::ffi::rptadv_media_cancellation,
        output: *mut super::super::ffi::rptadv_media_stream,
    ) -> i32 {
        let state = unsafe { &*context.cast::<Arc<SpeechFixture>>() }.clone();
        let request = unsafe { &*request };
        assert_eq!((request.speed_percent, request.level_db), (100, -6));
        assert_eq!(unsafe { CStr::from_ptr(request.model) }, c"voice.onnx");
        let text = unsafe { CStr::from_ptr(request.text) }
            .to_str()
            .unwrap()
            .to_owned();
        state.opened.lock().unwrap().push(text.clone());
        let active = state.active.fetch_add(1, Ordering::AcqRel) + 1;
        state.maximum.fetch_max(active, Ordering::AcqRel);
        unsafe {
            *output = super::super::ffi::rptadv_media_stream {
                handle: Box::into_raw(Box::new(StreamFixture {
                    state,
                    text,
                    read: 0,
                }))
                .cast(),
                sample_rate_hz: 48000,
            };
        }
        0
    }
    unsafe extern "C" fn read(
        handle: *mut c_void,
        cancel: *const super::super::ffi::rptadv_media_cancellation,
        output: *mut f32,
        capacity: usize,
        count: *mut usize,
    ) -> i32 {
        let stream = unsafe { &mut *handle.cast::<StreamFixture>() };
        let body = stream
            .text
            .split_once(" says: ")
            .map_or(stream.text.as_str(), |(_, body)| body);
        if body.starts_with("F failure") {
            return -4;
        }
        let cancel = unsafe { &*cancel };
        // Deterministic external-source stall, released by the test or cancellation.
        while (body.starts_with('A') || body.starts_with('é'))
            && !stream.state.release.load(Ordering::Acquire)
        {
            if unsafe { cancel.is_cancelled.unwrap()(cancel.context) } != 0 {
                return -6;
            }
            thread::park_timeout(Duration::from_millis(1));
        }
        let frames = if body.starts_with("empty") {
            0
        } else if body.starts_with("large") {
            if stream.read < 20 { capacity } else { 0 }
        } else {
            usize::from(stream.read == 0)
        };
        unsafe {
            *count = frames;
            std::slice::from_raw_parts_mut(output, frames).fill(if body.starts_with('B') {
                0.5
            } else {
                0.25
            });
        }
        stream.read += 1;
        0
    }
    unsafe extern "C" fn close(handle: *mut c_void) {
        let stream = unsafe { Box::from_raw(handle.cast::<StreamFixture>()) };
        stream.state.active.fetch_sub(1, Ordering::AcqRel);
    }
    unsafe extern "C" fn destroy(handle: *mut c_void) {
        drop(unsafe { Box::from_raw(handle.cast::<Arc<SpeechFixture>>()) });
    }
    let mut preparer =
        NativeMediaPreparer::new(Path::new("unused"), Path::new("unused"), 30000).unwrap();
    preparer.speech = Arc::new(super::super::Context {
        handle: NonNull::new(Box::into_raw(Box::new(state)).cast()).unwrap(),
        destroy,
        read_stream: read,
        close_stream: close,
        open_file: None,
        open_speech: Some(open),
    });
    preparer
}

fn message(peer: usize, body: String) -> HumanMessage {
    HumanMessage {
        origin: TextOrigin {
            station_generation: 1,
            connection_generation: peer as u64 + 1,
            peer: peer.to_string(),
            protocol: 1,
        },
        source: peer.to_string(),
        audience: TextAudience::Broadcast,
        body: Arc::from(body),
    }
}
fn speech() -> SpeechSource {
    SpeechSource {
        text: String::new(),
        model: "voice.onnx".into(),
        speed_percent: 100,
        level_db: -6,
    }
}

#[test]
fn text_admission_counts_utf8_bytes_and_reserves_origin_and_node_quotas() {
    let mut executor = TextExecutor::new(1, preparer(Arc::default())).unwrap();
    executor.start().unwrap();
    // Keep consumers alive without rendering: admitted includes queued/active PCM.
    let mut readers = Vec::new();
    for peer in 0..17 {
        readers.push(
            executor
                .register("1000", message(peer, String::new()).origin)
                .unwrap(),
        );
    }
    assert!(
        executor
            .register("1000", message(0, String::new()).origin)
            .is_err()
    );
    assert!(
        executor
            .register("2000", message(99, String::new()).origin)
            .is_err()
    );
    assert_eq!(
        executor.admit(message(0, "é".repeat(2048)), speech()),
        Admission::Accepted
    );
    assert_eq!(
        executor.admit(message(0, "é".repeat(2048) + "x"), speech()),
        Admission::TooLarge
    );
    for _ in 0..3 {
        assert_eq!(
            executor.admit(message(0, "A waiting".into()), speech()),
            Admission::Accepted
        );
    }
    assert_eq!(
        executor.admit(message(0, "fifth".into()), speech()),
        Admission::OriginFull
    );
    for peer in 1..16 {
        for _ in 0..4 {
            assert_eq!(
                executor.admit(message(peer, "A waiting".into()), speech()),
                Admission::Accepted
            );
        }
    }
    assert_eq!(
        executor.admit(message(16, "full node".into()), speech()),
        Admission::NodeFull
    );
    executor.cancel(&message(0, String::new()).origin);
    executor.stop();
    assert_eq!(executor.start(), Err(MediaError::Cancelled));
    assert!(matches!(
        executor.register("1000", message(99, String::new()).origin),
        Err(MediaError::Cancelled)
    ));
    assert_eq!(
        executor.admit(message(16, "stopped".into()), speech()),
        Admission::Stopping
    );
    drop(readers);
}

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "bounded worker did not make progress"
        );
        thread::yield_now();
    }
}

#[test]
fn text_admission_fairness_keeps_same_origin_order_while_another_stalls() {
    let state = Arc::new(SpeechFixture::default());
    let mut executor = TextExecutor::new(1, preparer(Arc::clone(&state))).unwrap();
    let mut first = executor
        .register("1000", message(1, String::new()).origin)
        .unwrap();
    let mut second = executor
        .register("1000", message(2, String::new()).origin)
        .unwrap();
    for body in ["A first", "A second", "A third"] {
        assert_eq!(
            executor.admit(message(1, body.into()), speech()),
            Admission::Accepted
        );
    }
    until(|| state.opened.lock().unwrap().as_slice() == ["A first"]);
    assert_eq!(
        executor.admit(message(2, "B independent".into()), speech()),
        Admission::Accepted
    );
    let mut heard = Vec::new();
    until(|| {
        let mut output = [0.0; 32];
        if second.render(&mut output) {
            heard.extend(output.into_iter().filter(|v| *v != 0.0));
        }
        !heard.is_empty()
    });
    assert_eq!(heard, [0.5]);
    assert_eq!(state.maximum.load(Ordering::Acquire), 2);
    assert_eq!(
        state.opened.lock().unwrap().as_slice(),
        ["A first", "B independent"]
    );
    state.release.store(true, Ordering::Release);
    until(|| {
        first.render(&mut [0.0; 32]);
        second.render(&mut [0.0; 32]);
        state.opened.lock().unwrap().len() == 4
    });
    let opened = state.opened.lock().unwrap();
    assert_eq!(
        opened
            .iter()
            .filter(|s| s.starts_with('A'))
            .cloned()
            .collect::<Vec<_>>(),
        ["A first", "A second", "A third"]
    );
    drop(opened);
    executor.stop();
}

#[test]
fn text_admission_full_ring_obeys_process_budget_and_releases_quota_after_retirement() {
    let mut preparer = preparer(Arc::default());
    preparer.timeout_ms = 20;
    let mut executor = TextExecutor::new(1, preparer).unwrap();
    let mut reader = executor
        .register("1000", message(1, String::new()).origin)
        .unwrap();
    assert_eq!(
        executor.admit(message(1, "large stream".into()), speech()),
        Admission::Accepted
    );
    until(|| {
        let guard = reader.origin.ready.lock().unwrap();
        guard.slots() < 4
    });
    // Do not consume until the producer has exhausted its original full-ring budget.
    until(|| {
        if reader.current.is_none() {
            reader.current = reader.ready.pop().ok();
        }
        reader
            .current
            .as_ref()
            .is_some_and(|item| item.ticket.failed.load(Ordering::Acquire))
    });
    let mut output = [0.0; 16];
    assert!(!reader.render(&mut output));
    assert_eq!(output, [0.0; 16]);
    until(|| reader.origin.pending.load(Ordering::Acquire) == 0);
    assert_eq!(
        executor.admit(message(1, "B recovered".into()), speech()),
        Admission::Accepted
    );
    until(|| reader.render(&mut output));
    executor.cancel(reader.origin());
    assert!(!reader.render(&mut [0.0; 16]));
    executor.stop();
}

#[test]
fn text_lifecycle_ready_endpoint_does_not_retain_its_own_origin() {
    let mut executor = TextExecutor::new(1, preparer(Arc::default())).unwrap();
    let reader = executor
        .register("1000", message(1, String::new()).origin)
        .unwrap();
    let weak = Arc::downgrade(&reader.origin);
    assert_eq!(
        executor.admit(message(1, "B ready".into()), speech()),
        Admission::Accepted
    );
    until(|| reader.origin.ready.lock().unwrap().slots() < 4);
    executor.stop();
    drop(reader);
    drop(executor);
    assert!(
        weak.upgrade().is_none(),
        "ready PCM must not form an endpoint-owner cycle"
    );
}

#[test]
fn text_generation_fences_reject_wrong_origin_and_observe_only_metadata() {
    let mut executor = TextExecutor::new(1, preparer(Arc::default())).unwrap();
    let mut reader = executor
        .register("1000", message(1, String::new()).origin)
        .unwrap();
    assert_eq!(
        executor.admit(message(1, "B secret body never logged".into()), speech()),
        Admission::Accepted
    );
    until(|| {
        if reader.current.is_none() {
            reader.current = reader.ready.pop().ok();
        }
        reader.current.is_some()
    });
    reader.current.as_mut().unwrap().origin = Arc::new(message(2, String::new()).origin);
    assert!(!reader.render(&mut [0.0; 16]));
    assert_eq!(
        executor.admit(message(1, "B current generation".into()), speech()),
        Admission::Accepted
    );
    until(|| reader.render(&mut [0.0; 16]));
    executor.cancel(reader.origin());
    assert_eq!(
        executor.admit(message(1, "B retired".into()), speech()),
        Admission::Stopping
    );
    let observation = executor.observation();
    assert_eq!(observation.admitted, 2);
    assert_eq!(observation.rejected[3], 1);
    assert_eq!(observation.cancelled_origins, 1);
    executor.stop();
}

#[test]
fn text_reader_fences_stale_cancelled_delayed_empty_and_failed_segments() {
    for case in 0..5 {
        let mut executor = TextExecutor::new(1, preparer(Arc::default())).unwrap();
        let mut reader = executor
            .register("1000", message(1, String::new()).origin)
            .unwrap();
        assert_eq!(
            executor.admit(message(1, "B ready".into()), speech()),
            Admission::Accepted
        );
        until(|| {
            if reader.current.is_none() {
                reader.current = reader.ready.pop().ok();
            }
            reader.current.is_some()
        });
        let mut held_producer = None;
        let item = reader.current.as_mut().unwrap();
        let ticket = Arc::clone(&item.ticket);
        match case {
            0 => reader.completed_sequence = item.sequence,
            1 => reader.origin.cancel(),
            2 => item.delay += 1,
            3 => {
                let (producer, ring) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
                held_producer = Some(producer);
                item.reader.ring = ring;
                item.reader.progress.budget.store(1, Ordering::Release);
            }
            _ => item.reader.ring = crate::link::ring::tests::failed_sample_consumer(),
        }
        let mut output = [0.0; 16];
        assert!(!reader.render(&mut output), "case={case}");
        assert_eq!(output, [0.0; 16]);
        if case == 3 {
            assert!(reader.current.is_some());
        }
        if case == 4 {
            assert!(ticket.failed.load(Ordering::Acquire));
        }
        drop(held_producer);
        executor.stop();
    }
}

#[test]
fn text_producer_cancellation_and_empty_speech_do_not_publish_pcm() {
    for case in 0..3 {
        let mut executor = TextExecutor::prepare(1, preparer(Arc::default())).unwrap();
        let reader = executor
            .register("1000", message(1, String::new()).origin)
            .unwrap();
        let body = if case == 2 { "empty" } else { "A waiting" };
        assert_eq!(
            executor.admit(message(1, body.into()), speech()),
            Admission::Accepted
        );
        let job = executor.state.jobs.pop().unwrap();
        match case {
            0 => job.origin.cancel(),
            1 => job.ticket.failed.store(true, Ordering::Release),
            _ => {}
        }
        assert_eq!(
            produce(&executor.state, &job),
            Err(if case == 2 {
                MediaError::InvalidOutput
            } else {
                MediaError::Cancelled
            })
        );
        assert!(reader.ready.is_empty());
        drop(job);
        executor.stop();
    }
}

#[test]
fn text_cancelled_jobs_release_admission_without_failure_reports() {
    for started in [false, true] {
        let state = Arc::new(SpeechFixture::default());
        let mut executor = TextExecutor::prepare(1, preparer(Arc::clone(&state))).unwrap();
        let reader = executor
            .register("1000", message(1, String::new()).origin)
            .unwrap();
        assert_eq!(
            executor.admit(message(1, "A waiting".into()), speech()),
            Admission::Accepted
        );
        if started {
            executor.start().unwrap();
            until(|| !state.opened.lock().unwrap().is_empty());
        }
        executor.cancel(reader.origin());
        executor.start().unwrap();
        until(|| reader.origin.pending.load(Ordering::Acquire) == 0);
        assert_eq!(executor.observation().failures, 0);
        assert!(executor.take_failure().is_none());
        if !started {
            assert!(state.opened.lock().unwrap().is_empty());
        }
        executor.stop();
    }
}

#[test]
fn text_job_claim_waits_for_both_sequence_and_previous_worker_release() {
    let mut executor = TextExecutor::prepare(1, preparer(Arc::default())).unwrap();
    let reader = executor
        .register("1000", message(1, String::new()).origin)
        .unwrap();
    assert!(!reader.origin.claim(2));
    assert!(reader.origin.claim(1));
    reader.origin.next.store(2, Ordering::Release);
    assert!(
        !reader.origin.claim(2),
        "previous producer has not released its claim"
    );
    reader.origin.running.store(false, Ordering::Release);
    assert!(reader.origin.claim(2));
    executor.stop();
}
