//! C5 hardware-free combined lifecycle qualification; never a physical bound.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

/// Owns the sole fake audio pump. Callback contexts must outlive its joined destruction.
struct Pump {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Pump {
    fn spawn(callback: impl Fn() + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::Builder::new()
            .name("c5-native-audio".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    callback();
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
            .unwrap();
        Self {
            stop,
            worker: Some(worker),
        }
    }
    fn join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
    fn destroy(&mut self, _stop_error: bool) {
        self.join();
    }
}

#[test]
fn stage_c5_fake_audio_destroy_joins_after_stop_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = calls.clone();
    let mut pump = Pump::spawn(move || {
        callback_calls.fetch_add(1, Ordering::Relaxed);
    });
    while calls.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    pump.destroy(true);
    let joined = pump.worker.is_none();
    // Contract-only negative control uses no SDK/context memory; do not leak its thread on RED.
    pump.join();
    assert!(
        joined,
        "destroy must join independently of the injected ordinary stop error"
    );
    let before = calls.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(calls.load(Ordering::Acquire), before);
    fake_audio_contract();
}

use super::{ProductRuntime, ProviderSet, SecretsFile, resolve_radio_nodes, vox};
use crate::abi;
use std::{
    ffi::c_void,
    mem::size_of,
    path::Path,
    ptr,
    sync::{Mutex, atomic::AtomicU64},
    time::Instant,
};
#[path = "c5_fake_hardware.rs"]
mod hardware;
#[path = "c5_historical_consumer.rs"]
mod historical;
static HISTORICAL_BINDING: AtomicUsize = AtomicUsize::new(0);

static CLOCK_READS: AtomicUsize = AtomicUsize::new(0);
static SOURCE_READS: AtomicUsize = AtomicUsize::new(0);
static SDK_READS: AtomicUsize = AtomicUsize::new(0);
static IO_READS: AtomicUsize = AtomicUsize::new(0);
static UNLOADS: AtomicUsize = AtomicUsize::new(0);
static NATIVE_LIVE: AtomicUsize = AtomicUsize::new(0);
static NATIVE_DESTROYED: AtomicUsize = AtomicUsize::new(0);
static OBSERVED: AtomicUsize = AtomicUsize::new(0);
static VALID_CLOCK: AtomicUsize = AtomicUsize::new(0);
static LAST_OBSERVATION: AtomicU64 = AtomicU64::new(0);
static VIOLATION: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicBool = AtomicBool::new(false);
static EXPECTED_READER: AtomicUsize = AtomicUsize::new(0);
static RAW_ORIGIN: AtomicU64 = AtomicU64::new(0);
static NATIVE_HANDLE: AtomicUsize = AtomicUsize::new(0);
static BINDS: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct Observation {
    struct_size: u32,
    abi_version: u32,
    radio_generation: u64,
    frame_count: u32,
    reserved: u32,
    retirement: u64,
    buffering: [u64; 8],
    clock_flags: u32,
    clock_domain: u32,
    observed_ns: u64,
}
#[repr(C)]
struct BindingV2 {
    base: abi::UrpProgrammeFenceBinding,
    observed: Option<unsafe extern "C" fn(*mut c_void, *const Observation) -> u32>,
}
struct Native {
    snapshot: unsafe extern "C" fn(*mut c_void, u32, *mut abi::UrpAstSiteSnapshot) -> i32,
    create: unsafe extern "C" fn(*const abi::UrpNativeCreateArgs, *mut *mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
    bind: unsafe extern "C" fn(*mut c_void, *mut abi::UrpProgrammeFenceBinding) -> i32,
}
static SDK: Mutex<Option<Native>> = Mutex::new(None);

// Bounded preallocated scalar observer. No allocation, locks, logging or unwinding.
unsafe extern "C" fn observe(context: *mut c_void, raw: *const Observation) -> u32 {
    if context != ptr::from_ref(&OBSERVED).cast_mut().cast() || raw.is_null() {
        VIOLATION.store(true, Ordering::Release);
        return 1;
    }
    let value = unsafe { &*raw };
    OBSERVED.fetch_add(1, Ordering::Relaxed);
    if !LIVE.load(Ordering::Acquire) || NATIVE_LIVE.load(Ordering::Acquire) == 0 {
        VIOLATION.store(true, Ordering::Release);
    }
    if value.struct_size >= 112
        && value.abi_version == 1
        && value.radio_generation != 0
        && value.frame_count != 0
        && value.reserved == 0
        && value.clock_flags == 1
        && value.clock_domain == 1
        && value.observed_ns > 0
    {
        VALID_CLOCK.fetch_add(1, Ordering::Relaxed);
        LAST_OBSERVATION.store(value.observed_ns, Ordering::Release);
    }
    1
}
extern "C" fn read_notice() {
    if !LIVE.load(Ordering::Acquire) {
        VIOLATION.store(true, Ordering::Release);
    }
    CLOCK_READS.fetch_add(1, Ordering::Relaxed);
    let mut name = [0 as libc::c_char; 16];
    if unsafe { libc::pthread_getname_np(libc::pthread_self(), name.as_mut_ptr(), name.len()) } == 0
    {
        match unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_bytes() {
            b"rpt-media" => {
                SOURCE_READS.fetch_add(1, Ordering::Relaxed);
            }
            b"rpt-iax" => {
                IO_READS.fetch_add(1, Ordering::Relaxed);
            }
            b"c5-native-audio" => {
                SDK_READS.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}
extern "C" fn unload_notice() {
    if hardware::PUMPS.load(Ordering::Acquire) != 0
        || hardware::STREAMS.load(Ordering::Acquire) != 0
        || NATIVE_LIVE.load(Ordering::Acquire) != 0
        || NATIVE_DESTROYED.load(Ordering::Acquire) == 0
    {
        VIOLATION.store(true, Ordering::Release);
    }
    LIVE.store(false, Ordering::Release);
    UNLOADS.fetch_add(1, Ordering::AcqRel);
}
unsafe extern "C" fn native_create(
    args: *const abi::UrpNativeCreateArgs,
    out: *mut *mut c_void,
) -> i32 {
    let sdk = SDK.lock().unwrap();
    let sdk = sdk.as_ref().unwrap();
    let audio = unsafe {
        &*(*(*args).providers)
            .audio
            .cast::<abi::rptadv_audio_adapter_descriptor>()
    };
    if audio.monotonic_now_ns.map(|reader| reader as usize)
        != Some(EXPECTED_READER.load(Ordering::Acquire))
    {
        VIOLATION.store(true, Ordering::Release);
    }
    let status = unsafe { (sdk.create)(args, out) };
    if unsafe { !(*out).is_null() } {
        NATIVE_LIVE.fetch_add(1, Ordering::AcqRel);
        NATIVE_HANDLE.store(unsafe { *out } as usize, Ordering::Release);
    }
    if status != 0 || unsafe { (*out).is_null() } {
        return status;
    }
    let mut binding = BindingV2 {
        base: abi::UrpProgrammeFenceBinding {
            struct_size: size_of::<BindingV2>() as u32,
            abi_version: 2,
            context: ptr::from_ref(&OBSERVED).cast_mut().cast(),
            permit: None,
            accepted_abi_version: 0,
            reserved: 0,
        },
        observed: if std::env::var_os("C5_OMIT_OBSERVER").is_some() {
            None
        } else {
            Some(observe)
        },
    };
    // The controller currently forwards V1. This wrapper alone opts the real native
    // owner into V2 after create and before start; later replacement must fail evidence.
    let historical = HISTORICAL_BINDING.load(Ordering::Acquire);
    let binding = if historical == 0 {
        &mut binding
    } else {
        unsafe { &mut *(historical as *mut BindingV2) }
    };
    if std::env::var_os("C5_OMIT_OBSERVER").is_some() {
        binding.observed = None;
    }
    let original = binding.base;
    let original_observer = binding.observed.map(|f| f as usize);
    let status = unsafe { (sdk.bind)(*out, &mut binding.base) };
    if status != 0
        || binding.base.accepted_abi_version != 2
        || binding.base.struct_size != original.struct_size
        || binding.base.abi_version != original.abi_version
        || binding.base.context != original.context
        || binding.base.permit.is_some()
        || binding.base.reserved != original.reserved
        || binding.observed.map(|f| f as usize) != original_observer
    {
        // Non-null partial handle stays with the actual caller's rollback path.
        return -7;
    }
    if historical != 0 {
        eprintln!("C5_OLD_BIND create_status=0 bind_status=0 accepted_abi=2 identity_unchanged=1");
    }
    BINDS.fetch_add(1, Ordering::AcqRel);
    status
}
unsafe extern "C" fn native_destroy(handle: *mut c_void) {
    let sdk = SDK.lock().unwrap();
    unsafe {
        (sdk.as_ref().unwrap().destroy)(handle);
    }
    // Code/context and clock remain retained until the real native owner is gone.
    if hardware::PUMPS.load(Ordering::Acquire) != 0 {
        VIOLATION.store(true, Ordering::Release);
    }
    NATIVE_HANDLE.store(0, Ordering::Release);
    NATIVE_LIVE.fetch_sub(1, Ordering::AcqRel);
    NATIVE_DESTROYED.fetch_add(1, Ordering::AcqRel);
}

fn reset() {
    assert_eq!(size_of::<Observation>(), 112);
    assert_eq!(size_of::<BindingV2>(), 40);
    for counter in [
        &CLOCK_READS,
        &SOURCE_READS,
        &IO_READS,
        &SDK_READS,
        &UNLOADS,
        &NATIVE_LIVE,
        &NATIVE_DESTROYED,
        &OBSERVED,
        &VALID_CLOCK,
        &BINDS,
        &hardware::PUMPS,
        &hardware::STREAMS,
        &hardware::DESTROYS,
        &hardware::CALLBACKS,
        &hardware::UNEXPECTED,
        &hardware::TX_MEDIA,
        &hardware::TX_TONE,
        &hardware::PARTIAL_CALLBACKS,
        &hardware::PARTIAL_OWNERS,
        &hardware::GPIO_DESIRED,
        &hardware::GPIO_SERVICED,
        &hardware::GPIO_SEQUENCE,
        &hardware::GPIO_SAFE_SEQUENCE,
        &hardware::GPIO_FAULTS,
        &hardware::GPIO_FAULT_SEQUENCE,
        &hardware::GPIO_FAULT_HIGH,
    ] {
        counter.store(0, Ordering::Release);
    }
    LIVE.store(true, Ordering::Release);
    VIOLATION.store(false, Ordering::Release);
    hardware::FAIL_SAFE_SERVICE.store(false, Ordering::Release);
    hardware::INPUT.store(false, Ordering::Release);
    hardware::FAIL_START.store(false, Ordering::Release);
    hardware::FAIL_STOP.store(false, Ordering::Release);
    LAST_OBSERVATION.store(0, Ordering::Release);
}
fn providers(directory: &Path) -> ProviderSet {
    use crate::providers::{clock_tests, tests::replace_descriptor};
    let mut providers = ProviderSet::load().unwrap();
    let audio = unsafe {
        *providers
            .descriptor(clock_tests::AUDIO)
            .unwrap()
            .as_ptr()
            .cast::<abi::rptadv_audio_adapter_descriptor>()
    };
    let gpio = unsafe {
        *providers
            .descriptor("librptadv_gpio_adapter.so.1")
            .unwrap()
            .as_ptr()
            .cast::<abi::rptadv_gpio_adapter_descriptor>()
    };
    replace_descriptor(
        &mut providers,
        clock_tests::AUDIO,
        hardware::audio(audio.capability_name, audio.monotonic_now_ns.unwrap()),
    );
    replace_descriptor(
        &mut providers,
        "librptadv_gpio_adapter.so.1",
        hardware::gpio(gpio.capability_name),
    );
    clock_tests::attach_clock_fixture(&mut providers, directory, read_notice, unload_notice, true);
    let reader = providers.media_clock_reader().unwrap().unwrap();
    EXPECTED_READER.store(reader as usize, Ordering::Release);
    RAW_ORIGIN.store(unsafe { reader() }, Ordering::Release);
    let mut native = unsafe {
        *providers
            .descriptor("libusbradioplus_product.so.1")
            .unwrap()
            .as_ptr()
            .cast::<abi::UrpAstDescriptor>()
    };
    *SDK.lock().unwrap() = Some(Native {
        snapshot: native.native_site_snapshot.unwrap(),
        create: native.native_create.unwrap(),
        destroy: native.native_destroy.unwrap(),
        bind: native.native_programme_fence_bind.unwrap(),
    });
    native.native_create = Some(native_create);
    native.native_destroy = Some(native_destroy);
    replace_descriptor(&mut providers, "libusbradioplus_product.so.1", native);
    providers
}
fn assert_drained() {
    eprintln!(
        "C5_DRAIN pumps={} streams={} destroyed={} native={} observations={} valid={} source={} io={} unexpected={} last_operation={}",
        hardware::PUMPS.load(Ordering::Acquire),
        hardware::STREAMS.load(Ordering::Acquire),
        hardware::DESTROYS.load(Ordering::Acquire),
        NATIVE_DESTROYED.load(Ordering::Acquire),
        OBSERVED.load(Ordering::Acquire),
        VALID_CLOCK.load(Ordering::Acquire),
        SOURCE_READS.load(Ordering::Acquire),
        IO_READS.load(Ordering::Acquire),
        hardware::UNEXPECTED.load(Ordering::Acquire),
        hardware::LAST_OPERATION.load(Ordering::Acquire)
    );
    assert_eq!(
        hardware::PUMPS.load(Ordering::Acquire),
        0,
        "audio pump must join"
    );
    assert_eq!(
        hardware::STREAMS.load(Ordering::Acquire),
        0,
        "stream owner must destroy"
    );
    assert_eq!(
        NATIVE_LIVE.load(Ordering::Acquire),
        0,
        "native owner must destroy"
    );
    assert_eq!(NATIVE_DESTROYED.load(Ordering::Acquire), 1);
    assert_eq!(hardware::DESTROYS.load(Ordering::Acquire), 1);
    assert_eq!(
        UNLOADS.load(Ordering::Acquire),
        1,
        "exactly one fixture DSO unload"
    );
    assert!(
        !VIOLATION.load(Ordering::Acquire),
        "owner/unload ordering violation"
    );
    assert_eq!(
        hardware::UNEXPECTED.load(Ordering::Acquire),
        0,
        "unimplemented physical operation or callback failure"
    );
    let reads = CLOCK_READS.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(CLOCK_READS.load(Ordering::Acquire), reads);
    *SDK.lock().unwrap() = None;
}

#[test]
#[ignore = "C5 exact external cohort required; use workflows/tools/run_c5_combined_lifecycle_tests.py"]
fn stage_c5_combined_native_product_success() {
    combined(false, None);
}

#[test]
#[ignore = "C5 exact external cohort required; use workflows/tools/run_c5_combined_lifecycle_tests.py"]
fn stage_c5_combined_native_product_failed_stop_retry() {
    combined(true, None);
}

#[test]
#[ignore = "C5 exact historical row required; use workflows historical runner"]
fn stage_c5_old_consumer_new_sdk() {
    combined(false, Some(112));
}

#[test]
#[ignore = "C5 exact historical row required; use workflows historical runner"]
fn stage_c5_old_consumer_stage_b_sdk() {
    combined(false, Some(96));
}

fn combined(failed_stop: bool, old_size: Option<u32>) {
    reset();
    let historical = old_size.map(historical::Consumer::load);
    let directory = std::env::temp_dir().join(format!("rpt-c5-combined-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let mut remote = vox::Remote::new();
    let nodes = directory.join("nodes.conf");
    std::fs::write(
        &nodes,
        format!("[extnodes]\n2000=radio@{}/2000,127.0.0.1\n", remote.address),
    )
    .unwrap();
    let config = format!(
        "[general]\nnode_enabled=yes\n[1000]\nradio_channel=fixture\nduplex=full\nlink_lookup_method=file\nlink_static_directory_file={}\n[radio 1000]\ndevice_identifier=3-1:1.0\n",
        nodes.display()
    );
    let config = if failed_stop {
        format!(
            "{config}cm119_gpio_7_mode=out0\n[output 1000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=7\nstartup_state=on\nsafe_state=off\n"
        )
    } else {
        config
    };
    let providers = providers(&directory.join("clock"));
    let mut runtime = ProductRuntime::start_with_providers(
        Path::new("rpt_advanced.conf"),
        &config,
        resolve_radio_nodes(&config).unwrap(),
        SecretsFile::parse("").unwrap(),
        providers,
    )
    .unwrap();
    runtime.enable_listener(&mut remote.server).unwrap();
    let command = runtime.product_descriptor().link_command.unwrap();
    let link =
        std::thread::spawn(move || unsafe { command(c"1000".as_ptr(), 4, c"2000".as_ptr(), 4, 1) });
    let deadline = Instant::now() + Duration::from_secs(12);
    while !link.is_finished() && Instant::now() < deadline {
        remote.pump();
        std::thread::sleep(Duration::from_millis(1));
    }
    let admitted = link.join().unwrap();
    let mut quiet = Instant::now();
    let mut previous = remote.voiced_samples();
    while quiet.elapsed() < Duration::from_millis(750) && Instant::now() < deadline {
        remote.pump();
        let current = remote.voiced_samples();
        if current != previous {
            previous = current;
            quiet = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    remote.clear_voice_observation();
    let mut received_pattern = RxPattern::default();
    hardware::INPUT_FREQUENCY_HZ.store(
        if std::env::var_os("C5_WRONG_RX_PATTERN").is_some() {
            2000
        } else {
            1000
        },
        Ordering::Release,
    );
    hardware::INPUT.store(true, Ordering::Release);
    let receive_deadline = (Instant::now() + Duration::from_millis(600)).min(deadline);
    while received_pattern.matched_windows < 2 && Instant::now() < receive_deadline {
        remote.pump_with_audio(|pcm| received_pattern.observe(pcm));
        std::thread::sleep(Duration::from_millis(1));
    }
    hardware::INPUT.store(false, Ordering::Release);
    let received = remote.voiced_samples();
    let mut quiet = Instant::now();
    let mut previous = hardware::TX_MEDIA.load(Ordering::Acquire);
    while quiet.elapsed() < Duration::from_millis(750) && Instant::now() < deadline {
        remote.pump();
        let current = hardware::TX_MEDIA.load(Ordering::Acquire);
        if current != previous {
            previous = current;
            quiet = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let drained = quiet.elapsed() >= Duration::from_millis(750);
    let before = hardware::TX_TONE.load(Ordering::Acquire);
    remote.request_return();
    while (hardware::TX_TONE.load(Ordering::Acquire) == before || remote.sent_return_packets() == 0)
        && Instant::now() < deadline
    {
        remote.pump();
        std::thread::sleep(Duration::from_millis(1));
    }
    let sent = remote.sent_return_packets();
    let output = hardware::TX_TONE.load(Ordering::Acquire);
    let stop_evidence = failed_stop.then(|| failed_stop_retry(&mut runtime));
    if !failed_stop {
        runtime.stop().unwrap();
    }
    drop(runtime);
    drop(remote);
    assert_drained();
    std::fs::remove_dir_all(directory).unwrap();
    if let Some(evidence) = stop_evidence {
        evidence.assert_retention();
    }
    assert_eq!(admitted, 0);
    eprintln!(
        "C5_RX_PATTERN voiced={received} matched_windows={}",
        received_pattern.matched_windows
    );
    assert!(
        received > 0 && received_pattern.matched_windows >= 2,
        "fresh 1-kHz input must be recognized at remote receive endpoint"
    );
    assert!(
        drained && sent > 0 && output > before,
        "fresh remote media must traverse product and native TX"
    );
    assert!(SOURCE_READS.load(Ordering::Acquire) > 0);
    assert!(IO_READS.load(Ordering::Acquire) > 0);
    assert_eq!(BINDS.load(Ordering::Acquire), 1);
    if let Some(consumer) = historical.as_ref() {
        consumer.verify(old_size.unwrap());
    } else {
        assert!(SDK_READS.load(Ordering::Acquire) >= OBSERVED.load(Ordering::Acquire));
        assert!(
            OBSERVED.load(Ordering::Acquire) > 0,
            "missing or replaced native observer"
        );
        assert_eq!(
            OBSERVED.load(Ordering::Acquire),
            VALID_CLOCK.load(Ordering::Acquire)
        );
        assert!(LAST_OBSERVATION.load(Ordering::Acquire) >= RAW_ORIGIN.load(Ordering::Acquire));
    }
    eprintln!(
        "C5_CLOCK reads={} sdk_reads={} origin={} last_observed={} fresh_tx_tone_blocks={}",
        CLOCK_READS.load(Ordering::Acquire),
        SDK_READS.load(Ordering::Acquire),
        RAW_ORIGIN.load(Ordering::Acquire),
        LAST_OBSERVATION.load(Ordering::Acquire),
        output - before
    );
    eprintln!(
        "C5_COMBINED source={} io={} observations={} received={} sent={} native_destroyed=1 pumps=0 unloads=1 physical=Unknown",
        SOURCE_READS.load(Ordering::Acquire),
        IO_READS.load(Ordering::Acquire),
        OBSERVED.load(Ordering::Acquire),
        received,
        sent
    );
}

#[test]
#[ignore = "C5 exact external cohort required; use workflows/tools/run_c5_combined_lifecycle_tests.py"]
fn stage_c5_combined_native_product_partial_start_rollback() {
    reset();
    hardware::FAIL_START.store(
        std::env::var_os("C5_NO_START_FAILURE").is_none(),
        Ordering::Release,
    );
    let directory = std::env::temp_dir().join(format!("rpt-c5-rollback-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let config = "[general]\nnode_enabled=yes\n[1000]\nradio_channel=fixture\n[radio 1000]\ndevice_identifier=3-1:1.0\n";
    let providers = providers(&directory.join("clock"));
    let result = ProductRuntime::start_with_providers(
        Path::new("rpt_advanced.conf"),
        config,
        resolve_radio_nodes(config).unwrap(),
        SecretsFile::parse("").unwrap(),
        providers,
    );
    // No fixture cleanup: the real failed-start path must drain its transferred partial owner.
    assert!(
        matches!(result, Err(crate::runtime::RuntimeError::Start)),
        "startup must report the injected partial-start failure"
    );
    assert!(
        hardware::PARTIAL_CALLBACKS.load(Ordering::Acquire) > 0,
        "failure must follow admitted callbacks"
    );
    assert_eq!(
        hardware::PARTIAL_OWNERS.load(Ordering::Acquire),
        7,
        "failure must retain pump, stream and native owner"
    );
    assert_eq!(BINDS.load(Ordering::Acquire), 1);
    assert_drained();
    std::fs::remove_dir_all(directory).unwrap();
    eprintln!(
        "C5_PARTIAL_START callbacks={} source={} io={} native_destroyed=1 pumps=0 unloads=1 physical=Unknown",
        hardware::CALLBACKS.load(Ordering::Acquire),
        SOURCE_READS.load(Ordering::Acquire),
        IO_READS.load(Ordering::Acquire)
    );
}

fn fake_audio_contract() {
    unsafe extern "C" fn receive(_: *mut c_void, _: *const f32, _: u32) -> i32 {
        0
    }
    unsafe extern "C" fn transmit(_: *mut c_void, _: *mut f32, _: u32) -> i32 {
        0
    }
    unsafe extern "C" fn reader() -> u64 {
        1
    }
    reset();
    let audio = hardware::audio(c"contract-only".as_ptr(), reader);
    let config = abi::rptadv_audio_stream_config {
        struct_size: size_of::<abi::rptadv_audio_stream_config>() as u32,
        abi_version: 2,
        native_sample_rate_hz: 48000,
        maximum_receive_frame_count: 960,
        maximum_transmit_frame_count: 960,
        input_device_channels: 2,
        output_device_channels: 2,
        receive_worker: Some(receive),
        transmit_worker: Some(transmit),
        ..unsafe { std::mem::zeroed() }
    };
    let mut stream = ptr::null_mut();
    assert_eq!(
        unsafe { audio.stream_create.unwrap()(&config, &mut stream) },
        0
    );
    assert_eq!(unsafe { audio.stream_start.unwrap()(stream) }, 0);
    hardware::FAIL_STOP.store(true, Ordering::Release);
    assert_eq!(unsafe { audio.stream_stop.unwrap()(stream) }, -1);
    assert_eq!(hardware::PUMPS.load(Ordering::Acquire), 1);
    unsafe {
        audio.stream_destroy.unwrap()(stream);
    }
    assert_eq!(hardware::PUMPS.load(Ordering::Acquire), 0);
    assert_eq!(hardware::STREAMS.load(Ordering::Acquire), 0);
    assert_eq!(hardware::DESTROYS.load(Ordering::Acquire), 1);
    let calls = hardware::CALLBACKS.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(hardware::CALLBACKS.load(Ordering::Acquire), calls);
}

/// Observe decoded 8-kHz remote PCM, not local provider input or activity notices.
/// Each complete 160-sample window must concentrate at least 70% of its energy
/// at 1 kHz. Quadrature correlation tolerates phase, gain and packet boundaries.
#[derive(Default)]
struct RxPattern {
    samples: usize,
    energy: f64,
    sine: f64,
    cosine: f64,
    matched_windows: usize,
}
impl RxPattern {
    fn observe(&mut self, samples: &[f32]) {
        use std::f64::consts::FRAC_1_SQRT_2;
        const SINE: [f64; 8] = [
            0.0,
            FRAC_1_SQRT_2,
            1.0,
            FRAC_1_SQRT_2,
            0.0,
            -FRAC_1_SQRT_2,
            -1.0,
            -FRAC_1_SQRT_2,
        ];
        for &sample in samples {
            let value = f64::from(sample);
            self.energy += value * value;
            self.sine += value * SINE[self.samples % 8];
            self.cosine += value * SINE[(self.samples + 2) % 8];
            self.samples += 1;
            if self.samples == 160 {
                if self.energy > 0.000001
                    && self.sine * self.sine + self.cosine * self.cosine
                        > self.energy * 160.0 * 0.35
                {
                    self.matched_windows += 1;
                }
                self.samples = 0;
                self.energy = 0.0;
                self.sine = 0.0;
                self.cosine = 0.0;
            }
        }
    }
}

#[test]
fn stage_c5_rx_pattern_recognizes_only_injected_tone() {
    for (frequency, expected) in [(1000.0, 4), (2000.0, 0), (0.0, 0), (-1.0, 0)] {
        for offset in [0, 37] {
            let mut pattern = RxPattern::default();
            let pcm: [f32; 640] = std::array::from_fn(|i| {
                if frequency == 0.0 {
                    0.25
                } else if frequency < 0.0 {
                    0.0
                } else {
                    0.25 * ((i + offset) as f32 * std::f32::consts::TAU * frequency / 8000.0).sin()
                }
            });
            // Deliberately split recognition windows across unrelated packet boundaries.
            for chunk in pcm.chunks(137) {
                pattern.observe(chunk);
            }
            assert_eq!(
                pattern.matched_windows, expected,
                "frequency {frequency}, offset {offset}"
            );
        }
    }
}

/// Read the real SDK cache only while the actual native owner is retained.
fn site_snapshot() -> abi::UrpAstSiteSnapshot {
    let handle = NATIVE_HANDLE.load(Ordering::Acquire);
    assert_ne!(handle, 0);
    let mut snapshot: abi::UrpAstSiteSnapshot = unsafe { std::mem::zeroed() };
    snapshot.struct_size = size_of::<abi::UrpAstSiteSnapshot>() as u32;
    snapshot.abi_version = 1;
    assert_eq!(
        unsafe {
            (SDK.lock().unwrap().as_ref().unwrap().snapshot)(
                handle as *mut c_void,
                0,
                &mut snapshot,
            )
        },
        0
    );
    snapshot
}

struct FailedStopEvidence {
    stopped: Result<(), crate::runtime::RuntimeError>,
    claimed: bool,
    retained: bool,
    reader_usable: bool,
    native_progress: bool,
    backend_faults: usize,
    safe_sequence: usize,
    before_sequence: usize,
    snapshot_fault: bool,
    fault_sequence: usize,
    fault_high: usize,
    recovered: bool,
}
impl FailedStopEvidence {
    fn assert_retention(self) {
        assert!(
            self.claimed,
            "non-safe output must have a real SDK service acknowledgment before injection"
        );
        assert_eq!(
            self.stopped,
            Err(crate::runtime::RuntimeError::Stop),
            "actual outer stop must fail on the new safe-output service failure"
        );
        assert!(
            self.backend_faults > 0 && self.safe_sequence > self.before_sequence,
            "fault must follow a fresh non-safe to safe publication"
        );
        assert!(
            self.snapshot_fault,
            "real SDK must publish failed safe-output service"
        );
        assert!(
            self.fault_sequence >= self.safe_sequence && self.fault_high & 0x40 == 0,
            "backend failure must service the new safe demand, not an earlier phase"
        );
        assert!(
            self.recovered,
            "clear fault must produce fresh successful safe service before retry"
        );
        assert!(
            self.retained && self.reader_usable && self.native_progress,
            "failed stop must retain usable provider code and native callback owners"
        );
    }
}

fn failed_stop_retry(runtime: &mut ProductRuntime) -> FailedStopEvidence {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut initial = site_snapshot();
    while !(initial.claimed_mask == 0x40
        && initial.serviced_generation == initial.generation
        && initial.serviced_high_mask & 0x40 != 0)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(1));
        initial = site_snapshot();
    }
    eprintln!(
        "C5_INITIAL_SITE {initial:?} backend_desired={} backend_serviced={}",
        hardware::GPIO_DESIRED.load(Ordering::Acquire),
        hardware::GPIO_SERVICED.load(Ordering::Acquire)
    );
    let claimed = initial.claimed_mask == 0x40
        && initial.generation != 0
        && initial.submitted_generation == initial.generation
        && initial.serviced_generation == initial.generation
        && initial.requested_high_mask & 0x40 != 0
        && initial.serviced_high_mask & 0x40 != 0
        && hardware::GPIO_SERVICED.load(Ordering::Acquire) & 0x40 != 0;
    let before_sequence = hardware::GPIO_SEQUENCE.load(Ordering::Acquire);
    hardware::GPIO_FAULT_MODE.store(
        if std::env::var_os("C5_NO_SAFE_FAILURE").is_some() {
            0
        } else if std::env::var_os("C5_WRONG_FAILURE_PHASE").is_some() {
            2
        } else {
            1
        },
        Ordering::Release,
    );
    hardware::FAIL_SAFE_SERVICE.store(true, Ordering::Release);
    // Make the wrong-phase control actually fail a non-safe service before shutdown.
    if hardware::GPIO_FAULT_MODE.load(Ordering::Acquire) == 2 {
        let deadline = Instant::now() + Duration::from_secs(1);
        while hardware::GPIO_FAULTS.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
    }
    let stopped = runtime.stop();
    let retained = runtime.c5_retained_reader().is_some()
        && UNLOADS.load(Ordering::Acquire) == 0
        && NATIVE_LIVE.load(Ordering::Acquire) == 1
        && NATIVE_DESTROYED.load(Ordering::Acquire) == 0
        && hardware::PUMPS.load(Ordering::Acquire) == 1
        && hardware::STREAMS.load(Ordering::Acquire) == 1;
    let mut reader_usable = false;
    let mut native_progress = false;
    let mut snapshot_fault = false;
    if retained {
        let reader = runtime.c5_retained_reader().unwrap();
        reader_usable = reader as usize == EXPECTED_READER.load(Ordering::Acquire)
            && unsafe { reader() } >= RAW_ORIGIN.load(Ordering::Acquire);
        let snapshot = site_snapshot();
        eprintln!("C5_FAILED_SITE {snapshot:?}");
        snapshot_fault = snapshot.claimed_mask == 0x40
            && snapshot.requested_high_mask & 0x40 == 0
            && (snapshot.observed_at_valid == 0 || snapshot.input_quality != 1)
            && (snapshot.serviced_generation != snapshot.generation
                || snapshot.serviced_high_mask & 0x40 != 0);
        let observed = OBSERVED.load(Ordering::Acquire);
        let deadline = Instant::now() + Duration::from_secs(1);
        while OBSERVED.load(Ordering::Acquire) == observed && Instant::now() < deadline {
            std::thread::yield_now();
        }
        native_progress = OBSERVED.load(Ordering::Acquire) > observed;
    }
    let mut evidence = FailedStopEvidence {
        stopped,
        claimed,
        retained,
        reader_usable,
        native_progress,
        backend_faults: hardware::GPIO_FAULTS.load(Ordering::Acquire),
        safe_sequence: hardware::GPIO_SAFE_SEQUENCE.load(Ordering::Acquire),
        before_sequence,
        snapshot_fault,
        fault_sequence: hardware::GPIO_FAULT_SEQUENCE.load(Ordering::Acquire),
        fault_high: hardware::GPIO_FAULT_HIGH.load(Ordering::Acquire),
        recovered: false,
    };
    eprintln!(
        "C5_FAILED_STOP stop={:?} claimed={} retained={} reader={} progress={} faults={} before_sequence={} safe_sequence={} sdk_fault={} fault_sequence={} fault_high={}",
        evidence.stopped,
        claimed,
        retained,
        reader_usable,
        native_progress,
        evidence.backend_faults,
        before_sequence,
        evidence.safe_sequence,
        snapshot_fault,
        evidence.fault_sequence,
        evidence.fault_high
    );
    // Clear only the physical backend fault and retry the actual production stop.
    // Even a negative control cleans up before asserting; no fixture joins or owner drops.
    hardware::FAIL_SAFE_SERVICE.store(false, Ordering::Release);
    if stopped.is_err() {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut recovered = site_snapshot();
        while !(recovered.input_quality == 1
            && recovered.serviced_generation == recovered.generation
            && recovered.serviced_high_mask & 0x40 == 0)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
            recovered = site_snapshot();
        }
        evidence.recovered = recovered.input_quality == 1
            && recovered.claimed_mask == 0x40
            && recovered.generation == initial.generation
            && recovered.submitted_generation == recovered.generation
            && recovered.serviced_generation == recovered.generation
            && recovered.serviced_high_mask & 0x40 == 0
            && recovered.observed_at_valid == 1
            && recovered.observed_at_ms > initial.observed_at_ms;
        eprintln!(
            "C5_RECOVERED_SITE {recovered:?} fresh={}",
            evidence.recovered
        );
        runtime.stop().unwrap();
    }
    evidence
}
