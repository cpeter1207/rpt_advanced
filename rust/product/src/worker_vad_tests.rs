use super::*;
use crate::abi as ffi;
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

fn setup() -> (Runtime<Audio>, RadioWorker) {
    let (runtime, owners) = audio_owners_with_config(
        4096,
        "[1000]\nradio_channel=usb\nduplex=full\nsquelch_delay_ms=160\n",
    );
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
    // SAFETY: the fixture descriptor and callbacks live for the test process.
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let mut worker = RadioWorker::prepare(
        services.radio("1000", "usb", 4096).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        160,
        50,
    )
    .unwrap_or_else(|_| panic!("prepare VAD callback fixture"));
    assert!(worker.attach(owners).is_ok());
    (runtime, worker)
}
fn metadata(
    first: u64,
    count: usize,
    raw: bool,
    admitted: bool,
    burst: u64,
    voice: Option<u64>,
    mode: u32,
) -> crate::abi::UrpNativeReceiveMetadata {
    let mut metadata = crate::fixture::receive_metadata(admitted, first, count as u32);
    metadata.raw_receiver_keyed = u32::from(raw);
    metadata.receiver_keyed = u32::from(admitted);
    metadata.vad_mode = mode;
    metadata.rf_burst_id = burst;
    metadata.vad_preroll_frames = 7680;
    metadata.vad_voice_sample_index = voice.unwrap_or(u64::MAX);
    metadata.vad_voice_active = u32::from(voice.is_some_and(|index| index >= first) && admitted);
    metadata.vad_score = if !raw {
        0.0
    } else if metadata.vad_voice_active != 0 {
        0.9
    } else {
        0.1
    };
    metadata
}
fn deliver(
    worker: &RadioWorker,
    metadata: &crate::abi::UrpNativeReceiveMetadata,
    samples: &mut [f32],
) -> i32 {
    // SAFETY: stable contexts and the complete metadata/PCM spans remain alive for this serial call.
    unsafe {
        receive_callback(
            std::ptr::from_ref(&*worker.receive).cast_mut().cast(),
            metadata,
            samples.as_mut_ptr(),
            samples.len() as u32,
        )
    }
}
fn grant(worker: &RadioWorker, clock: &mut u64, mode: u32) {
    for _ in 0..16 {
        let mut input = [0.25; 960];
        let metadata = metadata(*clock, 960, true, true, 1, Some(*clock + 959), mode);
        assert_eq!(deliver(worker, &metadata, &mut input), 0);
        *clock += 960;
        assert_eq!(tx(worker, &mut [0.0; 960]).0, 0);
    }
}

fn replace_local_provider(worker: &mut RadioWorker, api: ffi::rpcr3_descriptor) {
    let api = Box::leak(Box::new(api));
    // SAFETY: the copied descriptor and its real provider callbacks remain live for both endpoints.
    let (producer, consumer) = unsafe { crate::link::ring::tests::local_with_descriptor(api, 160) };
    let mut update = LocalRingUpdate::prepare(160).unwrap();
    update.transmit.observer = producer.observer();
    update.receive.producer = producer;
    update.transmit.consumer = consumer;
    worker.observer = update.transmit.observer.clone();
    worker.handoff = update.receive.handoff.clone();
    worker.receive.state.get_mut().ring.current = update.receive;
    worker.transmit.ring.get_mut().current = update.transmit;
}

#[test]
fn tagged_provider_write_failure_cancels_receive_admission() {
    unsafe extern "C" fn reject(
        _: *mut ffi::rpcr3_ring,
        _: *const f32,
        _: u64,
        _: u64,
        _: u64,
        _: *const ffi::rpcr3_mute_span,
        _: u32,
        accepted: *mut u64,
    ) -> i32 {
        unsafe { *accepted = 0 };
        -1
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_runtime, mut worker) = setup();
    // SAFETY: replace one operation while retaining the released allocation/render lifecycle.
    let mut api = unsafe { *ffi::rpcr3_descriptor() };
    api.ring_producer_push_tagged = Some(reject);
    replace_local_provider(&mut worker, api);
    let capture = metadata(0, 960, true, true, 1, Some(959), 1);
    assert_eq!(deliver(&worker, &capture, &mut [0.25; 960]), 0);
    assert_eq!(
        worker.handoff.write_failed_samples.load(Ordering::Acquire),
        960
    );
    assert_eq!(worker.handoff.admission.load(Ordering::Acquire), 0);
    let mut output = [9.0; 960];
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    drop(worker.stop());
}

#[test]
fn released_tagged_local_constructor_retains_the_vad_reserve() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (producer, mut consumer) =
        // SAFETY: the linked provider retains its descriptor and callback code for this process.
        unsafe { crate::link::ring::tests::local_with_descriptor(ffi::rpcr3_descriptor(), 160) };
    assert_eq!(producer.rate(), 48000);
    assert_eq!(producer.observer().observe().unwrap().reserve_samples, 7680);
    consumer.reset().unwrap();
}
#[test]
fn first_pending_voice_marker_survives_preroll_without_early_keying() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for controls in [0, 7] {
        let (_runtime, worker) = setup();
        let metadata_context = std::ptr::from_ref(&*worker.transmit).cast_mut().cast();
        let mut linked = 99;
        let mut clock = 0;
        let mut played = Vec::new();
        for block in 0..24 {
            assert_eq!(
                // SAFETY: the fixture invokes both endpoints serially with stable contexts.
                unsafe { metadata_callback(metadata_context, controls, &mut linked) },
                0
            );
            assert_eq!(linked, 0);
            assert_eq!(
                // SAFETY: the fixture clock is parked and no receive callback is running.
                unsafe { (*worker.receive.state.get()).elapsed_samples },
                clock
            );
            let granted = block >= 3;
            let mut input = [if block == 0 { 0.375 } else { 0.125 }; 960];
            let metadata = metadata(clock, 960, true, granted, 1, granted.then_some(3839), 1);
            assert_eq!(deliver(&worker, &metadata, &mut input), 0);
            clock += 960;
            {
                // SAFETY: receive has returned and no callback runs concurrently in this fixture.
                let state = unsafe { &*worker.receive.state.get() };
                assert_eq!(state.elapsed_samples, clock);
                assert_eq!(state.next_source_sample, clock);
                assert_eq!(state.source_generation, metadata.generation_id);
                assert_eq!(state.rf_burst_id, 1);
            }
            let mut output = [9.0; 960];
            let (status, keyed) = tx(&worker, &mut output);
            assert_eq!(status, 0);
            if !granted {
                assert_eq!(keyed, 0);
                assert_eq!(output, [0.0; 960]);
                assert!(!worker.receive.status.snapshot().0);
            }
            played.extend_from_slice(&output);
        }
        let first = played
            .iter()
            .position(|s| s.abs() > 0.05)
            .expect("pending onset should play after admission");
        assert!(
            played
                .iter()
                .skip(first)
                .take(64)
                .any(|sample| *sample > 0.3),
            "first retained block must be the onset marker"
        );
        drop(worker.stop());
    }
}

#[test]
fn rapid_rekey_mutes_old_tail_and_retains_new_onset_before_tx_runs() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_runtime, worker) = setup();
    let mut clock = 0;
    for _ in 0..4 {
        let metadata = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
        assert_eq!(deliver(&worker, &metadata, &mut [0.5; 960]), 0);
        clock += 960;
    }
    let closed = metadata(clock, 480, false, false, 1, None, 1);
    assert_eq!(deliver(&worker, &closed, &mut [0.0; 480]), 0);
    clock += 480;
    let first_new = metadata(clock, 960, true, true, 2, Some(clock + 959), 1);
    assert_eq!(deliver(&worker, &first_new, &mut [-0.375; 960]), 0);
    clock += 960;
    assert_eq!(
        worker.observer.observe().unwrap().available_samples,
        5280,
        "normal RF rekey must not drop initial capture while awaiting TX"
    );
    let mut played = Vec::new();
    for block in 0..24 {
        if block != 0 {
            let metadata = metadata(clock, 960, true, true, 2, Some(clock + 959), 1);
            assert_eq!(deliver(&worker, &metadata, &mut [-0.125; 960]), 0);
            clock += 960;
        }
        let mut output = [9.0; 960];
        assert_eq!(tx(&worker, &mut output).0, 0);
        played.extend_from_slice(&output);
    }
    assert!(
        played.iter().all(|sample| *sample < 0.02),
        "old positive tail must stay muted after rapid rekey"
    );
    assert!(
        played.iter().any(|sample| *sample < -0.3),
        "new burst's first negative marker must survive"
    );
    drop(worker.stop());
}
#[test]
fn partial_and_zero_credit_capture_require_reset_and_a_post_fault_voice() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for zero_credit in [false, true] {
        let (_runtime, worker) = setup();
        let mut clock = 0;
        for count in if zero_credit {
            vec![4096, 4096, 4096, 2112]
        } else {
            vec![4096, 4096, 4096]
        } {
            let metadata = metadata(clock, count, true, false, 1, None, 1);
            assert_eq!(deliver(&worker, &metadata, &mut vec![0.25; count]), 0);
            clock += count as u64;
        }
        let count = if zero_credit { 960 } else { 4096 };
        let failed_end = clock + count as u64;
        let failed_metadata = metadata(clock, count, true, true, 1, Some(failed_end - 1), 1);
        assert_eq!(deliver(&worker, &failed_metadata, &mut vec![0.5; count]), 0);
        assert_eq!(
            worker.handoff.write_failed_samples.load(Ordering::Acquire),
            count as u64
        );
        assert_eq!(worker.handoff.admission.load(Ordering::Acquire), 0);
        assert_eq!(worker.observer.observe().unwrap().available_samples, 14400);
        clock = failed_end;
        let mut output = [9.0; 960];
        assert_eq!(tx(&worker, &mut output), (0, 0));
        assert_eq!(output, [0.0; 960]);
        let stale = metadata(clock, 960, true, true, 1, Some(failed_end - 1), 1);
        assert_eq!(deliver(&worker, &stale, &mut [0.0; 960]), 0);
        clock += 960;
        assert_eq!(
            worker.handoff.admission.load(Ordering::Acquire),
            0,
            "old speech grant cannot admit the new capture epoch"
        );
        let fresh = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
        assert_eq!(deliver(&worker, &fresh, &mut [0.0; 960]), 0);
        assert_eq!(
            worker.handoff.admission.load(Ordering::Acquire),
            worker.handoff.token.load(Ordering::Acquire)
        );
        assert_ne!(worker.handoff.admission.load(Ordering::Acquire), 0);
        drop(worker.stop());
    }
}
#[test]
fn malformed_source_and_identity_free_error_records_cancel_prior_admission() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for bad in 0..19 {
        let (_runtime, worker) = setup();
        let mut clock = 0;
        grant(&worker, &mut clock, 1);
        let mut metadata = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
        let mut input = [0.25; 960];
        match bad {
            0 => input[0] = f32::NAN,
            1 => metadata.vad_score = f32::NAN,
            2 => metadata.struct_size -= 1,
            3 => metadata.abi_version = 0,
            4 => metadata.frame_count -= 1,
            5 => metadata.generation_id = 0,
            6 => metadata.first_sample_index += 960,
            7 => {
                metadata.status = -1;
                metadata.generation_id = 0;
                metadata.frame_count = 0;
                metadata.first_sample_index = 0;
            }
            8 => metadata.status = 1,
            9 => metadata.first_sample_index = u64::MAX,
            10 => metadata.raw_receiver_keyed = 2,
            11 => metadata.raw_receiver_keyed = 0,
            12 => metadata.vad_mode = 3,
            13 => metadata.vad_voice_active = 2,
            14 => metadata.vad_score = -0.1,
            15 => metadata.vad_preroll_frames = 0,
            16 => metadata.rf_burst_id = 0,
            17 => metadata.vad_voice_sample_index = clock + 960,
            _ => metadata.vad_score = 1.1,
        }
        let status = if bad == 7 {
            // SAFETY: negative status is the sized cancellation record; it requires no PCM or identity.
            unsafe {
                receive_callback(
                    std::ptr::from_ref(&*worker.receive).cast_mut().cast(),
                    &metadata,
                    std::ptr::null_mut(),
                    0,
                )
            }
        } else {
            deliver(&worker, &metadata, &mut input)
        };
        assert_eq!(status, if matches!(bad, 6 | 7) { 0 } else { -1 });
        let mut output = [9.0; 960];
        assert_eq!(tx(&worker, &mut output), (0, 0), "bad source case {bad}");
        assert_eq!(output, [0.0; 960]);
        drop(worker.stop());
    }
}
#[test]
fn on_hang_admits_zero_pcm_without_an_energy_gate() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_runtime, worker) = setup();
    let first = metadata(0, 960, true, true, 1, Some(959), 2);
    assert_eq!(deliver(&worker, &first, &mut [0.25; 960]), 0);
    let mut clock = 960;
    let mut output = [9.0; 960];
    for _ in 0..20 {
        let mut hang = metadata(clock, 960, true, true, 1, Some(959), 2);
        hang.vad_voice_active = 0;
        hang.vad_score = 0.1;
        assert_eq!(deliver(&worker, &hang, &mut [0.0; 960]), 0);
        clock += 960;
        assert_eq!(tx(&worker, &mut output), (0, 1));
    }
    assert!(output.iter().all(|sample| sample.abs() < 0.000001));
    drop(worker.stop());
}

type Render = unsafe extern "C" fn(*mut ffi::rpcr3_ring, *mut f32, u64, *mut u64) -> i32;
static ORIGINAL_RENDER: OnceLock<Render> = OnceLock::new();
struct RenderGate {
    reached: mpsc::SyncSender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}
static RENDER_GATE: OnceLock<RenderGate> = OnceLock::new();
static PAUSE_RENDER: AtomicBool = AtomicBool::new(false);
static RENDER_HAD_AUDIO: AtomicBool = AtomicBool::new(false);
unsafe extern "C" fn paused_render(
    ring: *mut ffi::rpcr3_ring,
    output: *mut f32,
    count: u64,
    real: *mut u64,
) -> i32 {
    // SAFETY: the copied descriptor retains the real provider and the unchanged writable spans.
    let status = unsafe { ORIGINAL_RENDER.get().unwrap()(ring, output, count, real) };
    if PAUSE_RENDER.swap(false, Ordering::AcqRel) {
        // SAFETY: the caller owns count writable samples; the real render has returned.
        let samples = unsafe { std::slice::from_raw_parts(output, count as usize) };
        RENDER_HAD_AUDIO.store(
            samples.iter().any(|sample| sample.abs() > 0.05),
            Ordering::Release,
        );
        let gate = RENDER_GATE.get().unwrap();
        if gate.reached.try_send(()).is_err() {
            return -1;
        }
        if gate
            .resume
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv_timeout(Duration::from_secs(5))
            .is_err()
        {
            return -1;
        }
    }
    status
}
#[test]
fn old_tx_render_is_cancelled_after_rx_adopts_and_cancels_a_new_handoff() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, mut worker) = setup();
    let mut api =
        // SAFETY: copy only the checked readable provider table; callback code has process lifetime.
        unsafe { crate::link::ring::checked_descriptor(ffi::rpcr3_descriptor()) }.unwrap();
    assert!(
        ORIGINAL_RENDER
            .set(api.ring_consumer_render.unwrap())
            .is_ok()
    );
    api.ring_consumer_render = Some(paused_render);
    let api = Box::leak(Box::new(api));
    // SAFETY: the retained descriptor preserves real allocation and all operations except the render gate.
    let (producer, consumer) = unsafe { crate::link::ring::tests::local_with_descriptor(api, 160) };
    let mut update = LocalRingUpdate::prepare(160).unwrap();
    update.transmit.observer = producer.observer();
    update.receive.producer = producer;
    update.transmit.consumer = consumer;
    worker.observer = update.transmit.observer.clone();
    worker.handoff = update.receive.handoff.clone();
    worker.receive.state.get_mut().ring.current = update.receive;
    worker.transmit.ring.get_mut().current = update.transmit;
    let mut clock = 0;
    grant(&worker, &mut clock, 1);
    let next = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
    assert_eq!(deliver(&worker, &next, &mut [0.25; 960]), 0);
    clock += 960;
    let (reached_send, reached_receive) = mpsc::sync_channel(1);
    let (resume_send, resume_receive) = mpsc::sync_channel(1);
    assert!(
        RENDER_GATE
            .set(RenderGate {
                reached: reached_send,
                resume: Mutex::new(resume_receive)
            })
            .is_ok()
    );
    PAUSE_RENDER.store(true, Ordering::Release);
    let transmit_context = std::ptr::from_ref(&*worker.transmit).cast_mut() as usize;
    std::thread::scope(|scope| {
        let transmit = scope.spawn(move || {
            let mut output = [9.0; 960];
            let mut keyed = 99;
            let mut ctcss = 99;
            // SAFETY: the worker keeps this box alive; this thread alone owns the serial TX callback.
            let status = unsafe {
                transmit_callback(
                    transmit_context as *mut std::ffi::c_void,
                    output.as_mut_ptr(),
                    960,
                    &mut keyed,
                    &mut ctcss,
                )
            };
            (status, keyed, output)
        });
        struct Release(mpsc::SyncSender<()>);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = self.0.try_send(());
            }
        }
        let release = Release(resume_send);
        reached_receive
            .recv_timeout(Duration::from_secs(5))
            .expect("old TX must pause after rendering");
        assert!(
            RENDER_HAD_AUDIO.load(Ordering::Acquire),
            "the paused old frame must contain real queued audio"
        );
        let generation = runtime.status(0)[0].1.active.unwrap();
        assert!(
            worker
                .update_local_ring(generation, LocalRingUpdate::prepare(160).unwrap())
                .is_ok()
        );
        let next = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
        assert_eq!(deliver(&worker, &next, &mut [0.125; 960]), 0);
        let mut cancel = crate::fixture::receive_metadata(false, 0, 0);
        cancel.status = -1;
        cancel.generation_id = 0;
        assert_eq!(
            // SAFETY: sized cancellation is valid without a PCM span or source identity.
            unsafe {
                receive_callback(
                    std::ptr::from_ref(&*worker.receive).cast_mut().cast(),
                    &cancel,
                    std::ptr::null_mut(),
                    0,
                )
            },
            0
        );
        drop(release);
        let (status, keyed, output) = transmit
            .join()
            .expect("old TX callback finishes after release");
        assert_eq!((status, keyed), (0, 0));
        assert_eq!(output, [0.0; 960]);
    });
    drop(worker.stop());
}

#[test]
fn raw_false_partial_or_zero_credit_is_a_fault_and_recovers_while_still_unkeyed() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for zero_credit in [false, true] {
        let (_runtime, worker) = setup();
        let mut clock = 0;
        for count in if zero_credit {
            vec![4096, 4096, 4096, 2112]
        } else {
            vec![4096, 4096, 4096]
        } {
            let capture = metadata(clock, count, true, false, 1, None, 1);
            assert_eq!(deliver(&worker, &capture, &mut vec![0.25; count]), 0);
            clock += count as u64;
        }
        let count = if zero_credit { 960 } else { 4096 };
        let falling = metadata(clock, count, false, false, 1, None, 1);
        assert_eq!(deliver(&worker, &falling, &mut vec![0.0; count]), 0);
        clock += count as u64;
        let required = worker.handoff.reset_required.load(Ordering::Acquire);
        assert!(
            required > worker.handoff.reset_ack.load(Ordering::Acquire),
            "capture failure must request reset even after the RF falling edge closed its token"
        );
        assert_eq!(worker.handoff.token.load(Ordering::Acquire) & 1, 0);
        let mut cancel = crate::fixture::receive_metadata(false, 0, 0);
        cancel.status = -1;
        cancel.generation_id = 0;
        assert_eq!(deliver(&worker, &cancel, &mut []), 0);
        assert_eq!(
            worker.handoff.reset_required.load(Ordering::Acquire),
            required,
            "repeated cancellation must not outrun the pending reset acknowledgment"
        );
        assert_eq!(tx(&worker, &mut [9.0; 960]), (0, 0));
        assert_eq!(worker.handoff.reset_ack.load(Ordering::Acquire), required);
        let failed_samples = worker.handoff.write_failed_samples.load(Ordering::Acquire);
        let healthy = metadata(clock, 960, false, false, 1, None, 1);
        assert_eq!(deliver(&worker, &healthy, &mut [0.0; 960]), 0);
        assert_eq!(
            worker.handoff.write_failed_samples.load(Ordering::Acquire),
            failed_samples,
            "unkeyed capture must recover without waiting for another RF rise"
        );
        assert_eq!(worker.observer.observe().unwrap().available_samples, 960);
        drop(worker.stop());
    }
}

#[test]
fn source_generation_change_while_already_unkeyed_requires_quiescent_reset() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_runtime, worker) = setup();
    let first = metadata(0, 960, false, false, 0, None, 1);
    assert_eq!(deliver(&worker, &first, &mut [0.0; 960]), 0);
    let mut changed = metadata(960, 960, false, false, 0, None, 1);
    changed.generation_id = 2;
    assert_eq!(deliver(&worker, &changed, &mut [0.0; 960]), 0);
    let required = worker.handoff.reset_required.load(Ordering::Acquire);
    assert!(
        required > worker.handoff.reset_ack.load(Ordering::Acquire),
        "a changed tagged source generation requires reset even when RF was already closed"
    );
    assert_eq!(tx(&worker, &mut [9.0; 960]), (0, 0));
    assert_eq!(worker.handoff.reset_ack.load(Ordering::Acquire), required);
    let mut healthy = metadata(1920, 960, false, false, 0, None, 1);
    healthy.generation_id = 2;
    assert_eq!(deliver(&worker, &healthy, &mut [0.0; 960]), 0);
    assert_eq!(
        worker.handoff.write_failed_samples.load(Ordering::Acquire),
        0
    );
    assert_eq!(worker.observer.observe().unwrap().available_samples, 960);
    drop(worker.stop());
}

#[test]
fn reopened_radio_stream_cannot_inherit_previous_speech_admission() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_runtime, worker) = setup();
    let mut clock = 0;
    grant(&worker, &mut clock, 1);
    assert_ne!(worker.handoff.admission.load(Ordering::Acquire), 0);
    let status = worker.receive.status.clone();
    let owners = worker.stop().expect("stopped stream returns both owners");
    // SAFETY: the fixture descriptor and callbacks live for the test process.
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let mut reopened = RadioWorker::prepare(
        services.radio("1000", "usb", 4096).unwrap(),
        Instant::now(),
        status,
        160,
        50,
    )
    .unwrap_or_else(|_| panic!("prepare reopened VAD stream"));
    assert!(reopened.attach(owners).is_ok());
    let mut output = [9.0; 960];
    assert_eq!(tx(&reopened, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    let pending = metadata(0, 960, true, false, 1, None, 1);
    assert_eq!(deliver(&reopened, &pending, &mut [0.375; 960]), 0);
    assert_eq!(tx(&reopened, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    assert_eq!(reopened.observer.observe().unwrap().available_samples, 960);
    drop(reopened.stop());
}

#[test]
fn absent_callback_context_or_metadata_never_retains_an_admitted_tail() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        // SAFETY: absent context/metadata and an empty PCM span require no dereference.
        unsafe {
            receive_callback(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        },
        -1
    );
    let (_runtime, worker) = setup();
    let mut clock = 0;
    grant(&worker, &mut clock, 1);
    let mut input = [0.25; 960];
    // SAFETY: the live callback context and writable PCM are valid; metadata is deliberately absent.
    let status = unsafe {
        receive_callback(
            std::ptr::from_ref(&*worker.receive).cast_mut().cast(),
            std::ptr::null(),
            input.as_mut_ptr(),
            960,
        )
    };
    assert_eq!(status, -1);
    let mut output = [9.0; 960];
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    drop(worker.stop());
}

#[test]
fn vad_fault_waits_for_successful_reset_and_cannot_grant_without_a_voice_stamp() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_runtime, worker) = setup();
    let mut clock = 0;
    for count in [4096, 4096, 4096, 2112, 960] {
        let capture = metadata(clock, count, true, false, 1, None, 1);
        assert_eq!(deliver(&worker, &capture, &mut vec![0.25; count]), 0);
        clock += count as u64;
    }
    assert_eq!(
        worker.handoff.write_failed_samples.load(Ordering::Acquire),
        960
    );
    let pending = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
    assert_eq!(deliver(&worker, &pending, &mut [0.25; 960]), 0);
    clock += 960;
    assert_eq!(
        worker.handoff.pending_reset_dropped.load(Ordering::Acquire),
        960
    );
    let required = worker.handoff.reset_required.load(Ordering::Acquire);
    RadioWorker::fail_next_for_test(TestFailure::Reset);
    let mut output = [9.0; 960];
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    assert!(worker.handoff.reset_ack.load(Ordering::Acquire) < required);
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(worker.handoff.reset_ack.load(Ordering::Acquire), required);
    let falling = metadata(clock, 960, false, false, 1, None, 1);
    assert_eq!(deliver(&worker, &falling, &mut [0.0; 960]), 0);
    clock += 960;
    assert_eq!(worker.observer.observe().unwrap().available_samples, 960);
    let unstamped = metadata(clock, 960, true, true, 2, None, 1);
    assert_eq!(deliver(&worker, &unstamped, &mut [0.25; 960]), 0);
    assert_eq!(worker.handoff.admission.load(Ordering::Acquire), 0);
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    drop(worker.stop());
}

thread_local! {
    static MUTE_FAILURE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
unsafe extern "C" fn rejected_mute(
    _: *mut ffi::rpcr3_ring,
    _: u64,
    _: *const crate::link::ring::MuteSpan,
    _: u32,
    accepted: *mut u32,
) -> i32 {
    // SAFETY: the typed ring client supplies a complete writable interval-credit output.
    unsafe { *accepted = 0 };
    if MUTE_FAILURE.get() == 1 { -1 } else { 0 }
}

#[test]
fn falling_edge_mute_errors_and_missing_credit_cancel_the_grant_and_reset() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for failure in [1, 2] {
        let (_runtime, mut worker) = setup();
        // SAFETY: the immutable real provider table remains live for the test process.
        let mut api = unsafe { *ffi::rpcr3_descriptor() };
        api.ring_producer_mute = Some(rejected_mute);
        replace_local_provider(&mut worker, api);
        let mut clock = 0;
        grant(&worker, &mut clock, 1);
        MUTE_FAILURE.set(failure);
        let falling = metadata(clock, 960, false, false, 1, None, 1);
        assert_eq!(deliver(&worker, &falling, &mut [0.0; 960]), 0);
        assert_eq!(worker.handoff.admission.load(Ordering::Acquire), 0);
        assert!(
            worker.handoff.reset_required.load(Ordering::Acquire)
                > worker.handoff.reset_ack.load(Ordering::Acquire)
        );
        assert_eq!(
            worker.handoff.write_failed_samples.load(Ordering::Acquire),
            960
        );
        let mut output = [9.0; 960];
        assert_eq!(tx(&worker, &mut output), (0, 0));
        assert_eq!(output, [0.0; 960]);
        drop(worker.stop());
    }
}

#[test]
fn failed_receive_policy_query_returns_the_unactivated_radio_reservation() {
    unsafe extern "C" fn rejected_query(
        _: *mut c_void,
        _: *mut c_void,
        _: *mut ffi::rptadv_radio_control_v1,
        _: ffi::rptadv_text_sink_v1,
        _: *mut c_void,
    ) -> i32 {
        -1
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // SAFETY: copy the complete immutable fixture table, retaining all original callbacks.
    let mut table = unsafe { *crate::fixture::host_descriptor() };
    table.radio_control = Some(rejected_query);
    // SAFETY: the complete table remains alive until its copied typed service and radio drop.
    let services = unsafe { HostServices::open(&table) }.unwrap();
    let before = crate::fixture::RADIO_DROPS.load(Ordering::Acquire);
    let failure = RadioWorker::prepare(
        services.radio("1000", "usb", 4096).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        160,
        50,
    )
    .err()
    .expect("invalid policy must prevent activation");
    assert!(matches!(
        &failure,
        PrepareFailure::Reservation(Error::Operation, _)
    ));
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Acquire), before);
    drop(failure);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(Ordering::Acquire),
        before + 1
    );
}

static CANCELLED_RENDER: OnceLock<Render> = OnceLock::new();
thread_local! {
    static CANCEL_DURING_RENDER: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}
unsafe extern "C" fn cancelling_render(
    ring: *mut ffi::rpcr3_ring,
    output: *mut f32,
    count: u64,
    real: *mut u64,
) -> i32 {
    // SAFETY: preserve the real provider handle and complete writable render span.
    let status = unsafe { CANCELLED_RENDER.get().unwrap()(ring, output, count, real) };
    let context = CANCEL_DURING_RENDER.replace(std::ptr::null_mut());
    if !context.is_null() {
        let mut cancellation = crate::fixture::receive_metadata(false, 0, 0);
        cancellation.status = -1;
        cancellation.generation_id = 0;
        // SAFETY: the test keeps the separate RX context alive through this serial cancellation.
        let result = unsafe { receive_callback(context, &cancellation, std::ptr::null_mut(), 0) };
        if result != 0 {
            return result;
        }
    }
    status
}

#[test]
fn same_ring_rx_cancellation_during_render_mutes_the_completed_tx_frame() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_runtime, mut worker) = setup();
    // SAFETY: copy the immutable provider descriptor and retain its real callback code.
    let mut api = unsafe { *ffi::rpcr3_descriptor() };
    assert!(
        CANCELLED_RENDER
            .set(api.ring_consumer_render.unwrap())
            .is_ok()
    );
    api.ring_consumer_render = Some(cancelling_render);
    replace_local_provider(&mut worker, api);
    let mut clock = 0;
    grant(&worker, &mut clock, 1);
    let capture = metadata(clock, 960, true, true, 1, Some(clock + 959), 1);
    assert_eq!(deliver(&worker, &capture, &mut [0.25; 960]), 0);
    CANCEL_DURING_RENDER.set(std::ptr::from_ref(&*worker.receive).cast_mut().cast());
    let mut output = [9.0; 960];
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(output, [0.0; 960]);
    assert!(CANCEL_DURING_RENDER.get().is_null());
    drop(worker.stop());
}
