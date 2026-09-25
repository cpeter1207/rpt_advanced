use super::*;
use crate::services::HostServices;
use rpt_advanced_core::{
    config::ConfigDocument,
    media::{FileRequest, MediaError, PreparedAudio, SpeechRequest},
    runtime::{
        DeviceHandoff, GenerationSettings, NativeFilePreparer, NativeSpeechPreparer,
        PreparedAdapter, Runtime, RuntimeClock, RuntimeError,
    },
};

struct Media;
impl NativeFilePreparer for Media {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeSpeechPreparer for Media {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
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
fn audio_owners() -> (Runtime<Audio>, AudioOwners) {
    audio_owners_with_maximum(8)
}
fn audio_owners_with_maximum(maximum: usize) -> (Runtime<Audio>, AudioOwners) {
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=usb\nduplex=full\n").unwrap(),
        &Media,
        |_, _| {
            let (state, _) =
                Audio::new(Vec::new(), maximum).map_err(|_| RuntimeError::Preparation)?;
            Ok(PreparedAdapter {
                state,
                control: (),
                receive_maximum: maximum,
                transmit_maximum: maximum,
            })
        },
        |_, _| Ok(Box::new(Device) as Box<dyn DeviceHandoff>),
        RuntimeClock {
            now_ms: 0,
            wall_seconds: 0,
            civil: None,
        },
    )
    .unwrap();
    let owners = runtime.node("1000").unwrap().register_audio().unwrap();
    (runtime, owners)
}
fn worker() -> RadioWorker {
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    RadioWorker::prepare(
        services.radio("usb", 8).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        0,
    )
    .unwrap_or_else(|_| panic!("prepare callbacks"))
}

#[test]
fn local_ring_captures_squelch_delay_before_any_callback() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    for (delay, reserve) in [(0, 0), (150, 7200)] {
        let worker = RadioWorker::prepare(
            services.radio("usb", 4096).unwrap(),
            Instant::now(),
            RadioStatus::default(),
            delay,
        )
        .unwrap_or_else(|_| panic!("prepare local ring"));
        let observation = worker.observer.observe().unwrap();
        assert_eq!(observation.capacity_samples, 14400);
        assert_eq!(observation.reserve_samples, reserve);
        assert_eq!(observation.target_samples, reserve);
        drop(worker.stop());
    }
}
fn rx(worker: &RadioWorker, receiving: bool, samples: &mut [f32]) -> i32 {
    unsafe {
        receive_callback(
            std::ptr::from_ref(&*worker.receive).cast_mut().cast(),
            u32::from(receiving),
            samples.as_mut_ptr(),
            samples.len() as u32,
        )
    }
}
fn tx(worker: &RadioWorker, samples: &mut [f32]) -> (i32, u32) {
    let mut keyed = 99;
    let result = unsafe {
        transmit_callback(
            std::ptr::from_ref(&*worker.transmit).cast_mut().cast(),
            samples.as_mut_ptr(),
            samples.len() as u32,
            &mut keyed,
        )
    };
    (result, keyed)
}
#[test]
fn independent_callbacks_queue_processed_pcm_and_return_transmit_keying() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    // Exercise persistent conversion at callback cadence without creating backlog.
    for _ in 0..256 {
        assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
        let mut discard = [0.0; 8];
        assert_eq!(tx(&worker, &mut discard), (0, 1));
    }
    assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
    let mut samples = [9.0; 4];
    assert_eq!(tx(&worker, &mut samples), (0, 1));
    assert!(samples.iter().all(|sample| (*sample - 0.25).abs() < 0.0001));
    let mut samples = [9.0; 8];
    assert_eq!(tx(&worker, &mut samples), (0, 1));
    assert!(
        samples[..4]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.0001)
    );
    assert!(
        samples[4..]
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() <= 1.0)
    );
    let owners = worker.stop().unwrap();
    assert!(runtime.node("1000").unwrap().register_audio().is_none());
    drop(owners);
    assert!(runtime.stop(0));
}
#[test]
fn absent_owners_and_invalid_sizes_fail_silent_and_unkeyed() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut worker = worker();
    for count in [0, 1, 8, 9] {
        let mut samples = vec![0.25; count];
        let expected = if count == 0 || count > 8 { -1 } else { 0 };
        assert_eq!(rx(&worker, true, &mut samples), expected);
        assert!(samples.iter().all(|s| *s == 0.0));
        samples.fill(0.25);
        assert_eq!(tx(&worker, &mut samples), (expected, 0));
        assert!(samples.iter().all(|s| *s == 0.0));
    }
    let (mut runtime, owners) = audio_owners();
    assert!(worker.attach(owners).is_ok());
    for count in [0, 9] {
        let mut samples = vec![0.25; count];
        assert_eq!(rx(&worker, true, &mut samples), -1);
        assert_eq!(tx(&worker, &mut samples), (-1, 0));
        assert!(samples.iter().all(|s| *s == 0.0));
    }
    runtime.stop(0);
    let mut samples = [0.25; 8];
    assert_eq!(rx(&worker, true, &mut samples), 0);
    assert!(
        worker.receive.status.snapshot().0,
        "gated generation must retain physical COS"
    );
    assert_eq!(samples, [0.0; 8]);
    samples.fill(0.25);
    assert_eq!(tx(&worker, &mut samples), (0, 0));
    assert_eq!(samples, [0.0; 8]);
    drop(worker.stop().unwrap());
    assert!(runtime.stop(0));
}

#[test]
fn malformed_callback_pointers_fail_without_publishing_audio_or_keying() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let worker = worker();
    let null = std::ptr::null_mut();
    let receive = std::ptr::from_ref(&*worker.receive).cast_mut().cast();
    let transmit = std::ptr::from_ref(&*worker.transmit).cast_mut().cast();
    let mut output = [0.25; 8];
    let mut keyed = 99;
    unsafe {
        assert_eq!(receive_callback(receive, 1, null, 8), -1);
        assert_eq!(receive_callback(null.cast(), 1, output.as_mut_ptr(), 8), -1);
        assert_eq!(output, [0.0; 8]);
        assert_eq!(transmit_callback(transmit, null, 8, &mut keyed), -1);
        assert_eq!(transmit_callback(transmit, null, 8, null.cast()), -1);
        assert_eq!(keyed, 0);
        output.fill(0.25);
        keyed = 99;
        assert_eq!(
            transmit_callback(null.cast(), output.as_mut_ptr(), 8, &mut keyed),
            -1
        );
        assert_eq!((output, keyed), ([0.0; 8], 0));
        output.fill(0.25);
        assert_eq!(
            transmit_callback(transmit, output.as_mut_ptr(), 8, null.cast()),
            -1
        );
        assert_eq!(output, [0.0; 8]);
    }
}

#[test]
fn duplicate_attachment_preserves_both_owner_pairs_and_adapter_errors_fail_silent() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners_with_maximum(4);
    let (mut other_runtime, other_owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    let returned = worker.attach(other_owners).expect_err("already attached");
    assert!(
        other_runtime
            .node("1000")
            .unwrap()
            .register_audio()
            .is_none()
    );
    drop(returned);
    assert!(other_runtime.stop(0));
    let mut output = [0.25; 8];
    assert_eq!(tx(&worker, &mut output), (-1, 0));
    assert_eq!(output, [0.0; 8]);
    drop(worker.stop());
    assert!(runtime.stop(0));
}

#[test]
fn provider_failures_remain_silent_and_observable_without_losing_control() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    let (producer, consumer) = crate::link::ring::tests::failed_endpoints();
    worker.observer = producer.observer();
    worker.receive.state.get_mut().producer = producer;
    *worker.transmit.consumer.get_mut() = consumer;
    assert!(worker.attach(owners).is_ok());
    assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
    let mut output = [0.25; 8];
    assert_eq!(tx(&worker, &mut output), (0, 0));
    assert_eq!(output, [0.0; 8]);
    assert_eq!(
        worker
            .receive
            .status
            .handoff
            .write_failed_samples
            .load(Ordering::Relaxed),
        8
    );
    assert_eq!(
        worker
            .receive
            .status
            .handoff
            .render_failures
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(worker.local_status_text(), "  local-rx: observation failed");
    worker.report_faults("1000", 5_000);
    assert_eq!(worker.reported_faults, [0; 6]);
    let (producer, _) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    worker.observer = producer.observer();
    worker.report_faults("1000\0", 10_000);
    assert_eq!(worker.reported_faults[3], 8);
    assert_eq!(worker.reported_faults[5], 1);
    drop(worker.stop());
    assert!(runtime.stop(0));
}
#[test]
fn prepare_failure_returns_reservation_and_quiesce_destroys_before_owner_release() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let before = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    RadioWorker::fail_next_for_test(TestFailure::Prepare);
    let Err((_, radio)) = RadioWorker::prepare(
        services.radio("usb", 8).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        0,
    ) else {
        panic!("expected failure")
    };
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Relaxed), before);
    drop(radio);
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    let owners = worker.stop().unwrap();
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(Ordering::Relaxed),
        before + 2
    );
    assert!(runtime.node("1000").unwrap().register_audio().is_none());
    drop(owners);
    assert!(runtime.stop(0));
}
#[test]
fn radio_status_preserves_the_original_edge_time() {
    let status = RadioStatus::default();
    assert_eq!(status.snapshot(), (false, 0));
    status.update(true, 2);
    status.update(true, 3);
    assert_eq!(status.snapshot(), (true, 2));
    status.update(false, u64::MAX);
    assert_eq!(status.snapshot(), (false, u64::MAX >> 1));
}

#[test]
fn rapid_rekey_cannot_replay_previous_burst() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    for _ in 0..256 {
        assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
        assert_eq!(tx(&worker, &mut [0.0; 8]), (0, 1));
    }
    for _ in 0..4 {
        assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
    }
    assert_eq!(rx(&worker, false, &mut [0.0; 8]), 0);
    assert_eq!(rx(&worker, true, &mut [0.0; 8]), 0);
    assert!(
        worker
            .local_status_text()
            .contains("pending-reset-dropped=8")
    );
    let mut output = [9.0; 8];
    assert_eq!(tx(&worker, &mut output).0, 0);
    assert_eq!(
        output, [0.0; 8],
        "previous burst must not survive rapid rekey"
    );
    drop(worker.stop());
    assert!(runtime.stop(0));
}

#[test]
fn reset_failure_holds_acknowledgement_and_retries_without_old_audio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
    assert_eq!(rx(&worker, false, &mut [0.0; 8]), 0);
    assert_eq!(rx(&worker, true, &mut [0.5; 8]), 0);
    RadioWorker::fail_next_for_test(TestFailure::Reset);
    let mut output = [9.0; 8];
    assert_eq!(tx(&worker, &mut output).0, 0);
    assert_eq!(output, [0.0; 8]);
    assert_eq!(
        worker
            .receive
            .status
            .handoff
            .reset_ack
            .load(Ordering::Acquire),
        0
    );
    assert!(worker.local_status_text().contains("reset-failures=1"));
    assert_eq!(rx(&worker, true, &mut [0.5; 8]), 0);
    assert!(
        worker
            .local_status_text()
            .contains("pending-reset-dropped=16")
    );
    assert_eq!(tx(&worker, &mut output).0, 0);
    assert_eq!(output, [0.0; 8]);
    assert_eq!(
        worker
            .receive
            .status
            .handoff
            .reset_ack
            .load(Ordering::Acquire),
        2
    );
    for _ in 0..256 {
        assert_eq!(rx(&worker, true, &mut [0.5; 8]), 0);
        assert_eq!(tx(&worker, &mut output).0, 0);
    }
    assert!(output.iter().all(|sample| (*sample - 0.5).abs() < 0.0001));
    worker.report_faults("1000", 4_999);
    assert_eq!(worker.reported_faults, [0; 6]);
    worker.report_faults("1000", 5_000);
    assert_eq!(worker.reported_faults[2], 16);
    assert_eq!(worker.reported_faults[4], 1);
    worker.report_faults("1000", 10_000);
    assert_eq!(worker.last_report_ms, 10_000);
    drop(worker.stop());
    assert!(runtime.stop(0));
}

#[test]
fn idle_transmit_does_not_consume_or_conceal_and_new_prefix_is_retained() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    for _ in 0..3 {
        // All edges fit within one elapsed millisecond and precede a TX callback.
        assert_eq!(rx(&worker, true, &mut [0.25; 8]), 0);
        assert_eq!(rx(&worker, false, &mut [0.0; 8]), 0);
    }
    assert_eq!(tx(&worker, &mut [9.0; 8]).0, 0);
    assert_eq!(
        worker
            .receive
            .status
            .handoff
            .reset_ack
            .load(Ordering::Acquire),
        6
    );
    let before = worker.observer.observe().unwrap();
    for _ in 0..256 {
        assert_eq!(tx(&worker, &mut [9.0; 8]).0, 0);
    }
    let after = worker.observer.observe().unwrap();
    assert_eq!(after.missing_samples, before.missing_samples);
    assert_eq!(after.available_samples, 0);
    assert_eq!(rx(&worker, true, &mut [0.5; 8]), 0);
    assert_eq!(worker.observer.observe().unwrap().available_samples, 8);
    let mut output = [0.0; 8];
    for _ in 0..256 {
        assert_eq!(tx(&worker, &mut output).0, 0);
        assert_eq!(rx(&worker, true, &mut [0.5; 8]), 0);
    }
    assert!(output.iter().all(|sample| (*sample - 0.5).abs() < 0.0001));
    assert!(
        worker
            .local_status_text()
            .contains("pending-reset-dropped=16")
    );
    drop(worker.stop());
    assert!(runtime.stop(0));
}

#[test]
fn processing_panics_are_contained_and_silence_both_endpoints() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    RadioWorker::fail_next_for_test(TestFailure::ReceivePanic);
    let mut samples = [0.25; 8];
    assert_eq!(rx(&worker, true, &mut samples), -1);
    assert!(
        worker.receive.status.snapshot().0,
        "processing panic must retain physical COS"
    );
    assert_eq!(samples, [0.0; 8]);
    samples.fill(0.25);
    assert_eq!(rx(&worker, true, &mut samples), 0);
    RadioWorker::fail_next_for_test(TestFailure::TransmitPanic);
    assert_eq!(tx(&worker, &mut samples), (-1, 0));
    assert_eq!(samples, [0.0; 8]);
    drop(worker.stop());
    assert!(runtime.stop(0));
}

#[test]
fn receive_processing_mutes_qualified_dtmf_before_local_handoff() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let mut worker = worker();
    assert!(worker.attach(owners).is_ok());
    let mut final_output = [1.0; 8];
    for block in 0..400 {
        let mut samples = std::array::from_fn::<_, 8, _>(|i| {
            let time = (block * 8 + i) as f32 / 48000.0;
            0.25 * ((std::f32::consts::TAU * 697.0 * time).sin()
                + (std::f32::consts::TAU * 1209.0 * time).sin())
        });
        assert_eq!(rx(&worker, true, &mut samples), 0);
        assert_eq!(tx(&worker, &mut final_output), (0, 1));
    }
    assert_eq!(final_output, [0.0; 8]);
    drop(worker.stop().unwrap());
    assert!(runtime.stop(0));
}

#[test]
fn failed_host_activation_returns_reservation_without_consuming_owners() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut runtime, owners) = audio_owners();
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let before = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    crate::fixture::RADIO_ACTIVATE_RESULT.store(1, Ordering::Release);
    let Err((Error::Operation, radio)) = RadioWorker::prepare(
        services.radio("usb", 8).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        0,
    ) else {
        panic!("activation must fail")
    };
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Relaxed), before);
    let mut worker = RadioWorker::prepare(radio, Instant::now(), RadioStatus::default(), 0)
        .unwrap_or_else(|_| panic!("reservation retry"));
    assert!(worker.attach(owners).is_ok());
    drop(worker.stop().unwrap());
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(Ordering::Relaxed),
        before + 1
    );
    assert!(runtime.stop(0));
}

#[test]
fn activation_installs_both_inactive_endpoints_and_destroy_still_sees_live_owners() {
    use std::cell::Cell;
    struct Probe {
        receive: Cell<*mut c_void>,
        transmit: Cell<*mut c_void>,
        destroyed: Cell<bool>,
    }
    unsafe extern "C" fn activate(
        context: *mut c_void,
        _: *mut c_void,
        receive: crate::abi::rptadv_radio_receive_v2,
        receive_context: *mut c_void,
        transmit: crate::abi::rptadv_radio_transmit_v2,
        transmit_context: *mut c_void,
    ) -> i32 {
        let probe = unsafe { &*context.cast::<Probe>() };
        probe.receive.set(receive_context);
        probe.transmit.set(transmit_context);
        let mut samples = [0.25; 8];
        let mut keyed = 9;
        assert_eq!(
            unsafe { receive.unwrap()(receive_context, 1, samples.as_mut_ptr(), 8) },
            0
        );
        assert_eq!(
            unsafe { transmit.unwrap()(transmit_context, samples.as_mut_ptr(), 8, &mut keyed) },
            0
        );
        assert_eq!(samples, [0.0; 8]);
        assert_eq!(keyed, 0);
        0
    }
    unsafe extern "C" fn destroy(context: *mut c_void, radio: *mut c_void) {
        let probe = unsafe { &*context.cast::<Probe>() };
        let mut samples = [0.25; 8];
        let mut keyed = 0;
        assert_eq!(
            unsafe { receive_callback(probe.receive.get(), 1, samples.as_mut_ptr(), 8) },
            0
        );
        assert_eq!(
            unsafe { transmit_callback(probe.transmit.get(), samples.as_mut_ptr(), 8, &mut keyed) },
            0
        );
        // The live zero-delay local ring passes this callback's PCM immediately.
        assert_eq!(samples, [0.25; 8]);
        assert_eq!(keyed, 1);
        probe.destroyed.set(true);
        unsafe {
            (*crate::fixture::host_descriptor()).radio_destroy.unwrap()(
                std::ptr::null_mut(),
                radio,
            );
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let probe = Box::leak(Box::new(Probe {
        receive: Cell::new(std::ptr::null_mut()),
        transmit: Cell::new(std::ptr::null_mut()),
        destroyed: Cell::new(false),
    }));
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = std::ptr::from_mut(probe).cast();
    table.radio_activate = Some(activate);
    table.radio_destroy = Some(destroy);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let (mut runtime, owners) = audio_owners();
    let mut worker = RadioWorker::prepare(
        services.radio("usb", 8).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        0,
    )
    .unwrap_or_else(|_| panic!("prepare"));
    assert!(worker.attach(owners).is_ok());
    let owners = worker.stop().unwrap();
    assert!(probe.destroyed.get());
    assert!(runtime.node("1000").unwrap().register_audio().is_none());
    drop(owners);
    assert!(runtime.stop(0));
}
