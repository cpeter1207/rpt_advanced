use super::*;
use crate::abi;
use std::ffi::c_void;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::{cell::Cell, mem::size_of, ptr};

thread_local! {
    static MODE: Cell<u8> = const { Cell::new(0) };
    static PREPARES: Cell<usize> = const { Cell::new(0) };
    static PROCESSES: Cell<usize> = const { Cell::new(0) };
    static DESTROYS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn create(
    args: *const abi::UrpNativeProcessingArgs,
    ffmpeg: *const c_void,
    maximum: u32,
    output: *mut *mut c_void,
) -> i32 {
    // SAFETY: preparation retains the request and its text for this synchronous call.
    let args = unsafe { &*args };
    assert_eq!(
        args.struct_size as usize,
        size_of::<abi::UrpNativeProcessingArgs>()
    );
    assert_eq!(args.abi_version, 1);
    // SAFETY: the request advertises both retained UTF-8 spans.
    assert_eq!(
        unsafe {
            std::slice::from_raw_parts(args.configuration, args.configuration_length as usize)
        },
        b"[link]\nlimiter_enabled=yes\n"
    );
    // SAFETY: the request advertises the retained node-name span.
    assert_eq!(
        unsafe { std::slice::from_raw_parts(args.node, args.node_length as usize) },
        b"524950"
    );
    assert_eq!(ffmpeg, ptr::NonNull::<u8>::dangling().as_ptr().cast());
    assert_eq!(maximum, 4);
    PREPARES.set(PREPARES.get() + 1);
    let mode = MODE.get();
    // SAFETY: the caller provides one writable handle destination.
    unsafe {
        *output = if matches!(mode, 1 | 2) {
            ptr::null_mut()
        } else {
            Box::into_raw(Box::new(Arc::new(AtomicU64::new(0)))).cast()
        }
    };
    if matches!(mode, 2 | 3) { -7 } else { 0 }
}

unsafe extern "C" fn process(
    context: *mut c_void,
    direction: u32,
    rate: u32,
    samples: *mut f32,
    count: u32,
) -> i32 {
    assert!(!context.is_null());
    assert_eq!(direction, abi::URP_AST_LINK_DIRECTION_READ);
    assert_eq!(rate, 48_000);
    assert!(count <= 4);
    PROCESSES.set(PROCESSES.get() + 1);
    // SAFETY: create transferred this graph's retained atomic fixture counter.
    unsafe { &*context.cast::<Arc<AtomicU64>>() }.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the wrapper forwards the caller's complete exclusive PCM span.
    for sample in unsafe { std::slice::from_raw_parts_mut(samples, count as usize) } {
        *sample *= 0.5;
    }
    if MODE.get() == 4 { -9 } else { 0 }
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    // SAFETY: every non-null handle was allocated by create and is released once.
    drop(unsafe { Box::from_raw(context.cast::<Arc<AtomicU64>>()) });
    DESTROYS.set(DESTROYS.get() + 1);
}

fn fixture() -> abi::UrpAstDescriptor {
    MODE.set(0);
    PREPARES.set(0);
    PROCESSES.set(0);
    DESTROYS.set(0);
    // SAFETY: this descriptor contains only scalar metadata and nullable function pointers.
    let mut api: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
    api.native_link_prepare = Some(create);
    api.link_process_f32 = Some(process);
    api.link_destroy = Some(destroy);
    api
}

fn output() -> abi::rptadv_pcm_processor_v1 {
    abi::rptadv_pcm_processor_v1 {
        struct_size: size_of::<abi::rptadv_pcm_processor_v1>() as u32,
        abi_version: 1,
        context: ptr::null_mut(),
        process: None,
        destroy: None,
        programme_buffering: None,
    }
}

fn prepared(
    api: &abi::UrpAstDescriptor,
    output: &mut abi::rptadv_pcm_processor_v1,
) -> Result<bool, String> {
    prepare(
        api,
        ptr::NonNull::<u8>::dangling().as_ptr().cast(),
        "[link]\nlimiter_enabled=yes\n",
        "524950",
        4,
        None,
        output,
    )
}

#[test]
fn native_peer_processor_owns_one_handle_and_preserves_bounds_and_provider_status() {
    let api = fixture();
    let mut output = output();
    assert_eq!(prepared(&api, &mut output), Ok(true));
    assert!(!output.context.is_null());
    let mut samples = [0.8_f32, -0.4];
    // SAFETY: preparation transferred this unique callback context and complete PCM span.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), samples.len()) },
        0
    );
    assert_eq!(samples, [0.4, -0.2]);
    MODE.set(4);
    // SAFETY: the same serial owner retains its context and PCM workspace.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), samples.len()) },
        -9
    );
    // SAFETY: malformed spans are rejected before a provider can dereference them.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, ptr::null_mut(), 1) },
        -1
    );
    // SAFETY: the advertised oversized count must be rejected before provider access.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), 5) },
        -1
    );
    // SAFETY: a null context is rejected before provider access.
    assert_eq!(
        unsafe { output.process.unwrap()(ptr::null_mut(), samples.as_mut_ptr(), 1) },
        -1
    );
    assert_eq!(PROCESSES.get(), 2);
    // SAFETY: the unique context is released once after callback use has ended.
    unsafe { output.destroy.unwrap()(output.context) };
    // SAFETY: null destruction is a no-op and transfers no ownership.
    unsafe { output.destroy.unwrap()(ptr::null_mut()) };
    assert_eq!(DESTROYS.get(), 1);
}

#[test]
fn preparation_bypass_and_failure_clean_up_only_acquired_handles() {
    let api = fixture();
    for mode in [1, 2, 3] {
        MODE.set(mode);
        let mut output = output();
        let result = prepared(&api, &mut output);
        if mode == 1 {
            assert_eq!(result, Ok(false));
        } else {
            assert!(result.unwrap_err().contains("-7"));
        }
        assert!(output.context.is_null());
        assert!(output.process.is_none());
        assert!(output.destroy.is_none());
    }
    assert_eq!(DESTROYS.get(), 1);
}

#[test]
fn invalid_preparation_is_rejected_before_acquiring_a_handle() {
    let api = fixture();
    for missing in 0..3 {
        let mut api = api;
        match missing {
            0 => api.native_link_prepare = None,
            1 => api.link_process_f32 = None,
            _ => api.link_destroy = None,
        }
        assert!(prepared(&api, &mut output()).is_err());
    }
    for (node, maximum) in [("", 4), ("524950", 0), ("524950", usize::MAX)] {
        assert!(prepare(&api, ptr::null(), "", node, maximum, None, &mut output()).is_err());
    }
    assert_eq!(PREPARES.get(), 0);
    assert!(length(usize::MAX).is_err());
    assert_eq!(length(0), Ok(0));
}

#[test]
fn retained_processor_observations_expire_with_the_processor_and_report_missing_support() {
    let registry = Observations::default();
    let api = fixture();
    let mut output = output();
    assert_eq!(
        prepare(
            &api,
            ptr::NonNull::<u8>::dangling().as_ptr().cast(),
            "[link]\nlimiter_enabled=yes\n",
            "524950",
            4,
            Some(&registry),
            &mut output
        ),
        Ok(true)
    );
    assert_eq!(registry.text("other"), "link_processors_retained=0\n");
    assert!(
        registry
            .text("524950")
            .contains("observation_status=unavailable")
    );
    // SAFETY: the transferred processor is quiescent and destroyed exactly once.
    unsafe { output.destroy.unwrap()(output.context) };
    assert_eq!(registry.text("524950"), "link_processors_retained=0\n");
}

#[test]
fn immutable_observer_lease_reads_actual_counters_while_audio_owns_the_graph() {
    unsafe extern "C" fn observe_create(graph: *mut c_void, output: *mut *mut c_void) -> i32 {
        // SAFETY: this synchronous preparation precedes audio ownership.
        let counters = unsafe { &*graph.cast::<Arc<AtomicU64>>() }.clone();
        // SAFETY: caller provides a writable unique observer destination.
        unsafe {
            *output = if matches!(MODE.get(), 8 | 10) {
                ptr::null_mut()
            } else {
                Box::into_raw(Box::new(counters)).cast()
            }
        };
        if matches!(MODE.get(), 9 | 10) { -8 } else { 0 }
    }
    unsafe extern "C" fn observe_read(
        owner: *const c_void,
        output: *mut abi::UrpAstLinkObservation,
    ) -> i32 {
        if MODE.get() == 11 {
            return -9;
        }
        // SAFETY: the independent observer retains the counter even during PCM processing.
        unsafe {
            let output = &mut *output;
            output.processed_blocks = (*owner.cast::<Arc<AtomicU64>>()).load(Ordering::Relaxed);
            output.loudness_epoch = 9;
            output.loudness_sample_count = 48_000;
            output.loudness_valid = u32::from(MODE.get() != 16);
            output.true_peak_valid = u32::from(MODE.get() != 16);
            output.integrated_lufs = if MODE.get() == 15 { f64::NAN } else { -23.25 };
            output.true_peak_dbtp = if MODE.get() == 15 {
                f64::INFINITY
            } else {
                -2.5
            };
            match MODE.get() {
                12 => output.struct_size = 32,
                13 => output.abi_version = 2,
                17 => output.struct_size += 1,
                18 => {
                    output.loudness_valid = 7;
                    output.true_peak_valid = 9;
                }
                _ => {}
            }
        }
        0
    }
    unsafe extern "C" fn observe_destroy(owner: *mut c_void) {
        // SAFETY: this independent observer allocation is destroyed exactly once.
        drop(unsafe { Box::from_raw(owner.cast::<Arc<AtomicU64>>()) });
    }
    let mut api = fixture();
    api.struct_size = std::mem::size_of_val(&api) as u32;
    api.link_observer_create = Some(observe_create);
    api.link_observer_read = Some(observe_read);
    api.link_observer_destroy = Some(observe_destroy);
    let registry = Observations::default();
    let mut output = output();
    assert_eq!(
        prepare(
            &api,
            ptr::NonNull::<u8>::dangling().as_ptr().cast(),
            "[link]\nlimiter_enabled=yes\n",
            "524950",
            4,
            Some(&registry),
            &mut output
        ),
        Ok(true)
    );
    let context = output.context as usize;
    let callback = output.process.unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for _ in 0..100 {
                let mut samples = [0.8_f32];
                // SAFETY: this sole audio owner retains the complete exclusive PCM span.
                assert_eq!(
                    unsafe { callback(context as *mut c_void, samples.as_mut_ptr(), 1) },
                    0
                );
            }
        });
        for _ in 0..100 {
            assert!(registry.text("524950").contains("processed_blocks="));
        }
    });
    assert!(
        registry
            .text("524950")
            .contains("processed_blocks=100 bypassed_blocks=0 failed_blocks=0")
    );
    // SAFETY: preparation transferred this processor; control borrows it only
    // after the sole audio thread joined, then retains the independent observer.
    let observer = unsafe { &*output.context.cast::<Processor>() }
        ._observation
        .clone();
    // SAFETY: the audio thread joined; this transferred context is released once.
    unsafe { output.destroy.unwrap()(output.context) };
    assert!(registry.text("524950").contains("processed_blocks=100"));
    drop(observer);
    assert_eq!(registry.text("524950"), "link_processors_retained=0\n");

    let boundary = std::mem::offset_of!(abi::UrpAstDescriptor, link_observer_destroy)
        + std::mem::size_of_val(&api.link_observer_destroy);
    for (short, missing, mode, expected) in [
        (false, 0, 0, "processed_blocks=0"),
        (true, 0, 0, "observation_status=unavailable"),
        (false, 1, 0, "observation_status=unavailable"),
        (false, 2, 0, "observation_status=unavailable"),
        (false, 3, 0, "observation_status=unavailable"),
        (false, 0, 8, "observation_status=unavailable status=-1"),
        (false, 0, 9, "observation_status=unavailable status=-8"),
        (false, 0, 10, "observation_status=unavailable status=-8"),
        (false, 0, 11, "observation_status=failed status=-9"),
        (false, 0, 12, "observation_status=failed status=-7"),
        (false, 0, 13, "observation_status=failed status=-7"),
        (
            false,
            0,
            14,
            "loudness_epoch=9 loudness_sample_count=48000 integrated_lufs=-23.25 true_peak_dbtp=-2.50 loudness_valid=1 true_peak_valid=1",
        ),
        (
            false,
            0,
            15,
            "integrated_lufs=unmeasured true_peak_dbtp=unmeasured loudness_valid=0 true_peak_valid=0",
        ),
        (
            false,
            0,
            16,
            "integrated_lufs=unmeasured true_peak_dbtp=unmeasured loudness_valid=0 true_peak_valid=0",
        ),
        (false, 0, 17, "observation_status=failed status=-7"),
        (
            false,
            0,
            18,
            "integrated_lufs=-23.25 true_peak_dbtp=-2.50 loudness_valid=1 true_peak_valid=1",
        ),
    ] {
        let mut api = api;
        api.struct_size = (boundary - usize::from(short)) as u32;
        match missing {
            1 => api.link_observer_create = None,
            2 => api.link_observer_read = None,
            3 => api.link_observer_destroy = None,
            _ => {}
        }
        MODE.set(mode);
        let mut output = self::output();
        assert_eq!(
            prepare(
                &api,
                ptr::NonNull::<u8>::dangling().as_ptr().cast(),
                "[link]\nlimiter_enabled=yes\n",
                "524950",
                4,
                Some(&registry),
                &mut output
            ),
            Ok(true)
        );
        assert!(registry.text("524950").contains(expected));
        // SAFETY: this sole serial test retains the prepared fixture graph.
        let graph = unsafe { &*output.context.cast::<Processor>() };
        // SAFETY: the fixture allocated this shared atomic counter in create.
        let counter = unsafe { &*graph.handle.as_ptr().cast::<Arc<AtomicU64>>() }.clone();
        // SAFETY: the test processor is quiescent and destroyed once.
        unsafe { output.destroy.unwrap()(output.context) };
        assert_eq!(
            Arc::strong_count(&counter),
            1,
            "observer leaked in mode {mode}"
        );
        assert_eq!(registry.text("524950"), "link_processors_retained=0\n");
    }
}

#[test]
fn shared_sdk_peer_observer_reaches_control_after_the_graph_is_destroyed() {
    let providers = crate::providers::ProviderSet::load().unwrap();
    let product = providers
        .descriptor("libusbradioplus_product.so.1")
        .unwrap();
    // SAFETY: the retained provider exposes its readable descriptor size prefix.
    let raw = unsafe { product.as_ptr() };
    // SAFETY: descriptor admission validated the readable metadata prefix.
    let bytes = unsafe { raw.cast::<u32>().read() } as usize;
    assert!(bytes >= size_of::<abi::UrpAstDescriptor>());
    // SAFETY: full readable size was admitted before forming a typed reference.
    let api = unsafe { &*raw.cast::<abi::UrpAstDescriptor>() };
    let ffmpeg = providers
        .descriptor("librptadv_ffmpeg_adapter.so.1")
        .unwrap();
    let registry = Observations::default();
    let mut output = output();
    // SAFETY: the provider stays loaded through processor and observer destruction.
    let ffmpeg = unsafe { ffmpeg.as_ptr() };
    assert_eq!(
        prepare(
            api,
            ffmpeg,
            "[524950]\n[link]\nequalizer_enabled=no\noutput_gain_db=-6\nagc_enabled=no\n",
            "524950",
            960,
            Some(&registry),
            &mut output
        ),
        Ok(true)
    );
    assert!(
        registry
            .text("524950")
            .contains("integrated_lufs=unmeasured true_peak_dbtp=unmeasured")
    );
    let mut samples = [0.0_f32; 960];
    for block in 0..50 {
        for (offset, sample) in samples.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU * 1_000.0 * (block * 960 + offset) as f64 / 48_000.0;
            *sample = (0.1 * phase.sin()) as f32;
        }
        // SAFETY: the sole audio owner supplies this complete exclusive PCM block.
        assert_eq!(
            unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), samples.len()) },
            0
        );
    }
    let measured = registry.text("524950");
    assert!(
        measured.contains("processed_blocks=50 bypassed_blocks=0 failed_blocks=0"),
        "{measured}"
    );
    assert!(
        measured.contains("loudness_sample_count=48000"),
        "{measured}"
    );
    let metric = |key: &str| {
        measured
            .split_ascii_whitespace()
            .find_map(|field| field.strip_prefix(key))
            .unwrap()
            .parse::<f64>()
            .unwrap()
    };
    // The 1kHz sine is about -23.05 LUFS/-20dBTP before the graph's -6dB gain.
    assert!(
        (metric("integrated_lufs=") + 29.05).abs() < 0.2,
        "{measured}"
    );
    assert!((metric("true_peak_dbtp=") + 26.0).abs() < 0.2, "{measured}");
    // SAFETY: processing has ended; only the immutable observer crosses to control.
    let observer = unsafe { &*output.context.cast::<Processor>() }
        ._observation
        .clone();
    // SAFETY: this quiescent transferred graph is destroyed once, before its observer.
    unsafe { output.destroy.unwrap()(output.context) };
    assert_eq!(registry.text("524950"), measured);
    drop(observer);
    assert_eq!(registry.text("524950"), "link_processors_retained=0\n");
}

#[test]
fn prepared_processor_exposes_released_programme_bound_on_its_serial_owner() {
    unsafe extern "C" fn buffering(
        context: *mut c_void,
        output: *mut abi::UrpProgrammeBuffering,
    ) -> i32 {
        assert!(!context.is_null());
        let output = unsafe { &mut *output };
        assert_eq!(output.struct_size, 64);
        assert_eq!(output.abi_version, 1);
        assert_eq!(output.reserved, 0);
        output.flags = 2;
        output.prepared_max_delay_frames = 480;
        -9
    }
    let mut api = fixture();
    api.struct_size = size_of::<abi::UrpAstDescriptor>() as u32;
    api.link_programme_buffering = Some(buffering);
    let mut output = output();
    assert_eq!(prepared(&api, &mut output), Ok(true));
    let operation = output.programme_buffering;
    let context = output.context;
    let destroy = output.destroy.unwrap();
    let mut bound: abi::UrpProgrammeBuffering = unsafe { std::mem::zeroed() };
    bound.struct_size = size_of_val(&bound) as u32;
    bound.abi_version = 1;
    let status = operation.map(|query| unsafe { query(context, &mut bound) });
    assert_eq!(
        unsafe { operation.unwrap()(ptr::null_mut(), &mut bound) },
        -1
    );
    assert_eq!(unsafe { operation.unwrap()(context, ptr::null_mut()) }, -1);
    unsafe { destroy(context) };
    assert_eq!(
        status,
        Some(-9),
        "the serial query must preserve provider status"
    );
    assert_eq!(bound.prepared_max_delay_frames, 480);
    assert_eq!(DESTROYS.get(), 1);
}

#[test]
fn programme_query_requires_complete_advertised_sdk_tail() {
    unsafe extern "C" fn unreachable(_: *mut c_void, _: *mut abi::UrpProgrammeBuffering) -> i32 {
        panic!("unadvertised query must never run");
    }
    for declared in [
        0,
        std::mem::offset_of!(abi::UrpAstDescriptor, link_programme_buffering),
        std::mem::offset_of!(abi::UrpAstDescriptor, link_programme_buffering)
            + size_of::<Option<Buffering>>()
            - 1,
    ] {
        let mut api = fixture();
        api.struct_size = declared as u32;
        api.link_programme_buffering = Some(unreachable);
        let mut output = output();
        assert_eq!(prepared(&api, &mut output), Ok(true));
        assert!(output.programme_buffering.is_none());
        unsafe { output.destroy.unwrap()(output.context) };
        assert_eq!(DESTROYS.get(), 1);
    }
    let mut api = fixture();
    api.struct_size = size_of::<abi::UrpAstDescriptor>() as u32;
    let mut output = output();
    assert_eq!(prepared(&api, &mut output), Ok(true));
    assert!(output.programme_buffering.is_none());
    unsafe { output.destroy.unwrap()(output.context) };
}
