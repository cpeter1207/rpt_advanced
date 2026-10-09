use super::*;
use std::sync::OnceLock;

type PushCallback = unsafe extern "C" fn(*mut ffi::rpcr3_ring, *const f32, u64, *mut u64) -> i32;
static PUSH_CALLBACK: OnceLock<PushCallback> = OnceLock::new();
thread_local! {
    static PUSH_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PUSH_SCRIPT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PUSH_CANCEL: std::cell::Cell<Option<(*const std::sync::atomic::AtomicU64, u64, usize)>> = const { std::cell::Cell::new(None) };
}

#[test]
fn peer_preparation_reserves_one_maximum_callback_and_producer_write() {
    for (rate, reserve, target, capacity) in [(8000, 685, 2080, 6176), (48000, 4102, 12480, 16576)]
    {
        let (mut producer, mut consumer) = InboundRing::open(rate, InboundPolicy::Peer).unwrap();
        let observation = consumer.observe().unwrap();
        assert_eq!(observation.reserve_samples, reserve);
        assert_eq!(observation.target_samples, target);
        assert_eq!(observation.capacity_samples, capacity);
        assert_eq!(producer.write(&[0.25; 4096]), Ok(4096));
        assert_eq!(producer.write(&[0.5; 4097]), Err(RingError));
        assert_eq!(consumer.observe().unwrap().available_samples, 4096);
        let mut output = [0.5; 4097];
        assert_eq!(consumer.render(&mut output), Err(RingError));
        assert_eq!(consumer.observe().unwrap().available_samples, 4096);
        assert_eq!(output, [0.0; 4097]);
    }
}

#[test]
fn local_shortfall_is_silence_without_concealment_or_plc_delay() {
    let (mut producer, mut consumer) = InboundRing::open(
        48000,
        InboundPolicy::Local {
            squelch_delay_ms: 0,
        },
    )
    .unwrap();
    assert_eq!(producer.write(&[0.25; 4096]), Ok(4096));
    let mut output = [0.0; 4096];
    assert_eq!(consumer.render(&mut output), Ok(4096));
    // Intrinsic converter history starts at zero, independently of PLC.
    assert!(output[0].abs() < 0.001);
    assert!(
        output[3584..]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.001)
    );
    assert_eq!(consumer.render(&mut output), Ok(0));
    assert_eq!(output, [0.0; 4096]);
}

#[test]
fn finite_media_ring_accepts_source_rates_above_native_without_latency_target_or_plc() {
    let (mut producer, mut consumer) = InboundRing::open(96000, InboundPolicy::Media).unwrap();
    let observation = consumer.observe().unwrap();
    assert_eq!(observation.reserve_samples, 0);
    assert_eq!(observation.target_samples, 0);
    assert_eq!(observation.capacity_samples, 8192);
    assert_eq!(producer.write(&[0.25; 2048]), Ok(2048));

    let mut rendered = Vec::new();
    for _ in 0..2048 {
        if let Some(sample) = consumer.render_sample().unwrap() {
            rendered.push(sample);
        }
    }
    assert!(rendered.len() >= 900);
    assert!(
        rendered[256..]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.01)
    );
}

#[test]
fn peer_playout_has_plc_lookahead_without_counting_it_as_loss() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    assert_eq!(producer.write(&[0.25; 2048]), Ok(2048));
    let mut output = [0.0; 960];
    assert_eq!(consumer.render(&mut output), Ok(780));
    assert_eq!(output[..180], [0.0; 180]);
    // Converter FIR startup is additional to the 180-sample PLC lookahead.
    // Continue real input long enough to observe settled audio.
    assert_eq!(consumer.render(&mut output), Ok(960));
    assert_eq!(consumer.render(&mut output), Ok(960));
    assert!(
        output[832..]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.001)
    );
    assert_eq!(consumer.observe().unwrap().missing_samples, 0);
}

unsafe extern "C" fn failed_create(
    _: *const ffi::rpcr3_config,
    _: *mut *mut ffi::rpcr3_ring,
) -> i32 {
    -1
}
unsafe extern "C" fn empty_create(
    _: *const ffi::rpcr3_config,
    _: *mut *mut ffi::rpcr3_ring,
) -> i32 {
    0
}
unsafe extern "C" fn failed_push(
    _: *mut ffi::rpcr3_ring,
    _: *const f32,
    _: u64,
    _: *mut u64,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_render(
    _: *mut ffi::rpcr3_ring,
    _: *mut f32,
    _: u64,
    _: *mut u64,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_render_sample(
    _: *mut ffi::rpcr3_ring,
    _: *mut f32,
    _: *mut bool,
) -> i32 {
    -1
}

static SAMPLE_THEN_FAIL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
unsafe extern "C" fn sample_then_fail_render(
    _: *mut ffi::rpcr3_ring,
    sample: *mut f32,
    real: *mut bool,
) -> i32 {
    if SAMPLE_THEN_FAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
        unsafe {
            *sample = 0.25;
            *real = true;
        }
        0
    } else {
        -1
    }
}
unsafe extern "C" fn failed_observe(
    _: *const ffi::rpcr3_ring,
    _: *mut ffi::rpcr3_observation,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_output_delay(_: *const ffi::rpcr3_ring, _: *mut u64) -> i32 {
    -1
}
unsafe extern "C" fn scripted_push(
    ring: *mut ffi::rpcr3_ring,
    samples: *const f32,
    count: u64,
    accepted: *mut u64,
) -> i32 {
    let call = PUSH_CALLS.with(|calls| {
        let call = calls.get();
        calls.set(call + 1);
        call
    });
    let mode = PUSH_SCRIPT.with(std::cell::Cell::get);
    let code = match (mode, call) {
        (1, 0) => {
            unsafe { *accepted = 0 };
            0
        }
        (2, 0) => unsafe { PUSH_CALLBACK.get().unwrap()(ring, samples, count / 2, accepted) },
        (3, 1) => -1,
        (5, 1) => unsafe { PUSH_CALLBACK.get().unwrap()(ring, samples, count / 2, accepted) },
        _ => unsafe { PUSH_CALLBACK.get().unwrap()(ring, samples, count, accepted) },
    };
    PUSH_CANCEL.with(|cancel| {
        if let Some((target, generation, target_call)) = cancel.get() {
            if target_call == call {
                // SAFETY: the test installs this pointer for a synchronous run_job call and clears it here.
                unsafe { (*target).store(generation, std::sync::atomic::Ordering::Release) };
                cancel.set(None);
            }
        }
    });
    code
}

pub(crate) fn cancel_on_push_open(
    _: u32,
    _: InboundPolicy,
) -> Result<(InboundProducer, InboundConsumer), RingError> {
    scripted_cancel_on_push_endpoints()
}

pub(crate) fn set_cancel_on_push(
    cancelled: &std::sync::atomic::AtomicU64,
    generation: u64,
    call: usize,
    partial: bool,
) {
    let mode = if partial { 5 } else { 4 };
    PUSH_CALLS.with(|calls| calls.set(0));
    PUSH_SCRIPT.with(|script| script.set(mode));
    PUSH_CANCEL.with(|cancel| cancel.set(Some((cancelled, generation, call))));
}

pub(crate) fn set_cancel_on_failed_push(
    cancelled: &std::sync::atomic::AtomicU64,
    generation: u64,
    call: usize,
) {
    PUSH_CALLS.with(|calls| calls.set(0));
    PUSH_SCRIPT.with(|script| script.set(3));
    PUSH_CANCEL.with(|cancel| cancel.set(Some((cancelled, generation, call))));
}

fn scripted_cancel_on_push_endpoints() -> Result<(InboundProducer, InboundConsumer), RingError> {
    // SAFETY: install a copied immutable descriptor; the thread-local cancellation target remains
    // valid for the synchronous producer callback in the owning worker test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        let _ = PUSH_CALLBACK.set(api.ring_producer_push.unwrap());
        api.ring_producer_push = Some(scripted_push);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api)))
    }
}

#[test]
fn malformed_ring_tables_and_failed_operations_never_publish_or_retain_invalid_audio() {
    // SAFETY: real immutable released descriptor; modified copies are retained for process lifetime.
    unsafe {
        assert!(InboundRing::from_descriptor(8000, InboundPolicy::Peer, ptr::null()).is_err());
        for rate in [0, 48001] {
            assert!(InboundRing::open(rate, InboundPolicy::Peer).is_err());
        }
        for rate in [0, 7999, 192001] {
            assert!(InboundRing::open(rate, InboundPolicy::Media).is_err());
        }
        for squelch_delay_ms in [301, u64::MAX] {
            assert!(InboundRing::open(48000, InboundPolicy::Local { squelch_delay_ms }).is_err());
        }
        let original = *ffi::rpcr3_descriptor();
        for case in 0..14 {
            let mut api = original;
            match case {
                0 => api.struct_size = 8,
                1 => api.abi_version = 2,
                2 => api.capability_name = ptr::null(),
                3 => api.capability_name = c"other".as_ptr(),
                4 => api.ring_create = None,
                5 => api.ring_destroy = None,
                6 => api.ring_producer_push = None,
                7 => api.ring_consumer_render = None,
                8 => api.ring_observe = None,
                9 => api.ring_create = Some(failed_create),
                10 => api.ring_create = Some(empty_create),
                11 => api.ring_consumer_reset = None,
                12 => api.ring_consumer_render_sample = None,
                _ => api.ring_output_delay = None,
            }
            assert!(
                InboundRing::from_descriptor(8000, InboundPolicy::Peer, Box::leak(Box::new(api)))
                    .is_err()
            );
        }
        let mut api = original;
        api.ring_producer_push = Some(failed_push);
        api.ring_consumer_render = Some(failed_render);
        api.ring_observe = Some(failed_observe);
        let (mut producer, mut consumer) =
            InboundRing::from_descriptor(8000, InboundPolicy::Peer, Box::leak(Box::new(api)))
                .unwrap();
        assert!(producer.write(&[0.5]).is_err());
        let mut samples = [0.5; 8];
        assert!(consumer.render(&mut samples).is_err());
        assert_eq!(samples, [0.0; 8]);
        assert!(consumer.observe().is_err());
        assert_eq!(PeerInput::available(&consumer), 0);
        assert!(!PeerInput::render(&mut consumer, &mut samples));
        assert_eq!(PeerInput::source_rate(&consumer), 8000);
        assert!(std::ptr::eq(
            PeerInput::signals(&consumer),
            producer.signals()
        ));
        assert!(producer.observer().observe().is_err());
        let (mut producer, consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
        assert_eq!(producer.write(&[]), Ok(0));
        assert_eq!(PeerInput::available(&consumer), 0);
    }
}

pub(crate) fn failed_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: retain a copied released descriptor and its real create/destroy pair.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_producer_push = Some(failed_push);
        api.ring_consumer_render = Some(failed_render);
        api.ring_observe = Some(failed_observe);
        InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn failed_output_delay_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: retain a copied descriptor and its real allocation lifecycle for this test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_output_delay = Some(failed_output_delay);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api))).unwrap()
    }
}

fn scripted_push_endpoints(mode: usize) -> (InboundProducer, InboundConsumer) {
    // SAFETY: copy the process-lifetime provider table and delegate successful writes to it.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        let _ = PUSH_CALLBACK.set(api.ring_producer_push.unwrap());
        PUSH_CALLS.with(|calls| calls.set(0));
        PUSH_SCRIPT.with(|script| script.set(mode));
        api.ring_producer_push = Some(scripted_push);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn zero_then_push_endpoints() -> (InboundProducer, InboundConsumer) {
    scripted_push_endpoints(1)
}

pub(crate) fn partial_then_push_endpoints() -> (InboundProducer, InboundConsumer) {
    scripted_push_endpoints(2)
}

pub(crate) fn fail_after_initial_push_endpoints() -> (InboundProducer, InboundConsumer) {
    scripted_push_endpoints(3)
}

pub(crate) fn sample_then_fail_consumer() -> InboundConsumer {
    // SAFETY: retain the descriptor and use its real allocation lifecycle around the test callback.
    unsafe {
        SAMPLE_THEN_FAIL.store(0, std::sync::atomic::Ordering::Relaxed);
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(sample_then_fail_render);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api)))
            .unwrap()
            .1
    }
}

pub(crate) fn failed_sample_consumer() -> InboundConsumer {
    // SAFETY: retain a copied released descriptor and its real create/destroy pair.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(failed_render_sample);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api)))
            .unwrap()
            .1
    }
}

#[test]
fn released_ring_endpoints_publish_render_and_report_their_generation() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    let observer = producer.observer();
    let same = producer.observer();
    let (other, _) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    assert!(observer.same_generation(&same));
    assert!(!observer.same_generation(&other.observer()));
    assert_eq!(observer.rate(), 8000);
    assert_eq!(producer.rate(), 8000);
    assert!(std::ptr::eq(observer.signals(), producer.signals()));

    assert_eq!(producer.write(&[0.25; 160]), Ok(160));
    let mut output = [0.0; 960];
    assert!(consumer.render(&mut output).is_ok());
}

#[test]
fn sample_render_reports_shared_ring_failure() {
    // SAFETY: the copied descriptor and its callback remain alive through the endpoint drop.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(failed_render_sample);
        let (_, mut consumer) =
            InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api)))
                .unwrap();
        assert!(consumer.render_sample().is_err());
    }
}
