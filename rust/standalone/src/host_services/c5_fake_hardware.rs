//! Physical-only test implementations. No entry delegates to hardware.
use super::*;
use crate::abi::*;
use std::ffi::{c_char, c_void};
use std::mem::size_of;

pub(super) static PUMPS: AtomicUsize = AtomicUsize::new(0);
pub(super) static STREAMS: AtomicUsize = AtomicUsize::new(0);
pub(super) static DESTROYS: AtomicUsize = AtomicUsize::new(0);
pub(super) static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
pub(super) static UNEXPECTED: AtomicUsize = AtomicUsize::new(0);
pub(super) static INPUT: AtomicBool = AtomicBool::new(false);
pub(super) static FAIL_START: AtomicBool = AtomicBool::new(false);
pub(super) static FAIL_STOP: AtomicBool = AtomicBool::new(false);
pub(super) static TX_MEDIA: AtomicUsize = AtomicUsize::new(0);

struct Stream {
    config: rptadv_audio_stream_config,
    pump: Option<Pump>,
}
struct Mixer {
    level: u32,
    enabled: u32,
}

unsafe extern "C" fn stream_create(
    config: *const rptadv_audio_stream_config,
    out: *mut *mut rptadv_audio_stream,
) -> i32 {
    let config = unsafe { *config };
    if config.receive_worker.is_none() || config.transmit_worker.is_none() {
        return -1;
    }
    STREAMS.fetch_add(1, Ordering::AcqRel);
    unsafe {
        *out = Box::into_raw(Box::new(Stream { config, pump: None })).cast();
    }
    0
}
unsafe extern "C" fn stream_start(raw: *mut rptadv_audio_stream) -> i32 {
    let stream = unsafe { &mut *raw.cast::<Stream>() };
    let rx = stream.config.receive_worker.unwrap();
    let tx = stream.config.transmit_worker.unwrap();
    let rx_context = stream.config.receive_worker_context as usize;
    let tx_context = stream.config.transmit_worker_context as usize;
    let frames = stream
        .config
        .maximum_receive_frame_count
        .min(stream.config.maximum_transmit_frame_count)
        .min(960);
    let ready = Arc::new(AtomicBool::new(false));
    let started = ready.clone();
    PUMPS.fetch_add(1, Ordering::AcqRel);
    stream.pump = Some(Pump::spawn(move || {
        let mut input = [0_f32; 1920];
        let mut output = [0_f32; 1920];
        if INPUT.load(Ordering::Acquire) {
            // Default fresh 1-kHz stimulus; the negative control injects nonzero 2 kHz.
            let frequency = INPUT_FREQUENCY_HZ.load(Ordering::Acquire) as f32;
            for (i, pair) in input.chunks_exact_mut(2).enumerate() {
                let value = ((i as f32) * std::f32::consts::TAU * frequency / 48000.0).sin() * 0.25;
                pair.fill(value);
            }
        }
        let rx_result = unsafe { rx(rx_context as *mut c_void, input.as_ptr(), frames) };
        let tx_result = unsafe { tx(tx_context as *mut c_void, output.as_mut_ptr(), frames) };
        if rx_result != 0 || tx_result != 0 {
            UNEXPECTED.fetch_add(1, Ordering::Relaxed);
        }
        if output.iter().any(|sample| sample.abs() > 0.0001) {
            TX_MEDIA.fetch_add(1, Ordering::Relaxed);
        }
        let (mut energy, mut sine, mut cosine) = (0_f32, 0_f32, 0_f32);
        for (i, pair) in output[..frames as usize * 2].chunks_exact(2).enumerate() {
            let phase = i as f32 * std::f32::consts::TAU / 48.0;
            energy += pair[0] * pair[0];
            sine += pair[0] * phase.sin();
            cosine += pair[0] * phase.cos();
        }
        if energy > 0.000001 && sine * sine + cosine * cosine > energy * frames as f32 * 0.1 {
            TX_TONE.fetch_add(1, Ordering::Relaxed);
        }
        CALLBACKS.fetch_add(1, Ordering::Release);
        started.store(true, Ordering::Release);
    }));
    while !ready.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    if FAIL_START.load(Ordering::Acquire) {
        PARTIAL_CALLBACKS.store(CALLBACKS.load(Ordering::Acquire), Ordering::Release);
        PARTIAL_OWNERS.store(
            usize::from(stream.pump.is_some())
                | (usize::from(STREAMS.load(Ordering::Acquire) == 1) << 1)
                | (usize::from(super::NATIVE_LIVE.load(Ordering::Acquire) == 1) << 2),
            Ordering::Release,
        );
        -1
    } else {
        0
    }
}
unsafe extern "C" fn stream_stop(raw: *mut rptadv_audio_stream) -> i32 {
    if FAIL_STOP.load(Ordering::Acquire) {
        return -1;
    }
    let stream = unsafe { &mut *raw.cast::<Stream>() };
    if let Some(mut pump) = stream.pump.take() {
        pump.join();
        PUMPS.fetch_sub(1, Ordering::AcqRel);
    }
    0
}
unsafe extern "C" fn stream_destroy(raw: *mut rptadv_audio_stream) {
    let mut stream = unsafe { Box::from_raw(raw.cast::<Stream>()) };
    if let Some(mut pump) = stream.pump.take() {
        // Destruction is stronger than ordinary stop: unconditional synchronous detach.
        pump.destroy(FAIL_STOP.load(Ordering::Acquire));
        PUMPS.fetch_sub(1, Ordering::AcqRel);
    }
    STREAMS.fetch_sub(1, Ordering::AcqRel);
    DESTROYS.fetch_add(1, Ordering::AcqRel);
}
unsafe extern "C" fn stream_stats(
    _: *const rptadv_audio_stream,
    out: *mut rptadv_audio_stream_stats,
) -> i32 {
    unsafe {
        (*out).abi_version = 2;
        (*out).callback_count = CALLBACKS.load(Ordering::Relaxed) as u64;
    }
    0
}
unsafe extern "C" fn timing(
    _: *const rptadv_audio_stream,
    out: *mut rptadv_audio_stream_timing,
) -> i32 {
    unsafe {
        (*out).abi_version = 2;
        (*out).sample_rate_hz = 48000.0;
        (*out).input_latency_seconds = 0.01;
        (*out).output_latency_seconds = 0.02;
    }
    0
}
fn copy_string<const N: usize>(out: &mut [c_char; N], bytes: &[u8]) {
    out.fill(0);
    for (out, byte) in out.iter_mut().zip(bytes) {
        *out = *byte as c_char;
    }
}
unsafe extern "C" fn select(
    _: *const rptadv_audio_usb_device_selector,
    out: *mut rptadv_audio_usb_device_match,
) -> i32 {
    let out = unsafe { &mut *out };
    out.abi_version = 2;
    copy_string(&mut out.usb_interface_path, b"3-1:1.0");
    copy_string(&mut out.usb_serial, b"C5");
    out.selection = rptadv_audio_usb_device_selection {
        struct_size: size_of::<rptadv_audio_usb_device_selection>() as u32,
        abi_version: 2,
        alsa_card_index: 1,
        input_device_index: 2,
        output_device_index: 3,
    };
    0
}
unsafe extern "C" fn mixer_create(
    _: *const rptadv_audio_usb_mixer_config,
    out: *mut *mut rptadv_audio_mixer,
) -> i32 {
    unsafe {
        *out = Box::into_raw(Box::new(Mixer {
            level: 500,
            enabled: 1,
        }))
        .cast();
    }
    0
}
unsafe extern "C" fn mixer_destroy(raw: *mut rptadv_audio_mixer) {
    drop(unsafe { Box::from_raw(raw.cast::<Mixer>()) });
}
unsafe extern "C" fn range(_: *const rptadv_audio_mixer, min: *mut i64, max: *mut i64) -> i32 {
    unsafe {
        *min = 0;
        *max = 2000;
    }
    0
}
unsafe extern "C" fn get_i64(raw: *const rptadv_audio_mixer, value: *mut i64) -> i32 {
    unsafe {
        *value = i64::from((*raw.cast::<Mixer>()).level) * 2;
    }
    0
}
unsafe extern "C" fn set_i64(raw: *mut rptadv_audio_mixer, value: i64) -> i32 {
    unsafe {
        (*raw.cast::<Mixer>()).level = (value / 2) as u32;
    }
    0
}
unsafe extern "C" fn get(raw: *const rptadv_audio_mixer, value: *mut u32) -> i32 {
    unsafe {
        *value = (*raw.cast::<Mixer>()).level;
    }
    0
}
unsafe extern "C" fn set(raw: *mut rptadv_audio_mixer, value: u32) -> i32 {
    unsafe {
        (*raw.cast::<Mixer>()).level = value;
    }
    0
}
unsafe extern "C" fn get_switch(raw: *const rptadv_audio_mixer, value: *mut u32) -> i32 {
    unsafe {
        *value = (*raw.cast::<Mixer>()).enabled;
    }
    0
}
unsafe extern "C" fn set_switch(raw: *mut rptadv_audio_mixer, value: u32) -> i32 {
    unsafe {
        (*raw.cast::<Mixer>()).enabled = value;
    }
    0
}
unsafe extern "C" fn paths(_: *const c_char, out: *mut rptadv_audio_cm119_mixer_paths) -> i32 {
    let out = unsafe { &mut *out };
    out.abi_version = 2;
    out.rx_capture_path_count = 1;
    out.tx_playback_path_count = 2;
    out.sidetone_path_count = 1;
    out.rx_compatibility_switch_path_count = 1;
    for (path, name, direction, capabilities) in [
        (&mut out.rx_capture_paths[0], &b"rx"[..], 0, 3),
        (&mut out.tx_playback_paths[0], &b"tx-a"[..], 1, 3),
        (&mut out.sidetone_paths[0], &b"side"[..], 1, 3),
        (
            &mut out.rx_compatibility_switch_paths[0],
            &b"compat"[..],
            1,
            2,
        ),
    ] {
        copy_string(&mut path.element, name);
        path.direction = direction;
        path.capabilities = capabilities;
    }
    out.tx_playback_paths[1] = out.tx_playback_paths[0];
    copy_string(&mut out.tx_playback_paths[1].element, b"tx-b");
    0
}
unsafe extern "C" fn probe(
    _: *const rptadv_gpio_device_config,
    out: *mut rptadv_gpio_device_info,
) -> i32 {
    unsafe {
        (*out).present = 1;
    }
    0
}
unsafe extern "C" fn gpio_open(
    _: *const rptadv_gpio_device_config,
    out: *mut *mut rptadv_gpio_device,
) -> i32 {
    unsafe {
        *out = Box::into_raw(Box::new(0_u8)).cast();
    }
    0
}
unsafe extern "C" fn gpio_close(raw: *mut rptadv_gpio_device) {
    drop(unsafe { Box::from_raw(raw.cast::<u8>()) });
}
// Scalar fake-backend receipts, distinct from the real SDK site acknowledgment.
pub(super) static GPIO_DESIRED: AtomicUsize = AtomicUsize::new(0);
pub(super) static GPIO_SERVICED: AtomicUsize = AtomicUsize::new(0);
pub(super) static GPIO_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
pub(super) static GPIO_SAFE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
pub(super) static GPIO_FAULT_MODE: AtomicUsize = AtomicUsize::new(1);
pub(super) static GPIO_FAULT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
pub(super) static GPIO_FAULT_HIGH: AtomicUsize = AtomicUsize::new(0);
pub(super) static GPIO_FAULTS: AtomicUsize = AtomicUsize::new(0);
pub(super) static FAIL_SAFE_SERVICE: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn gpio_service(_: *mut rptadv_gpio_device) -> i32 {
    let high = GPIO_DESIRED.load(Ordering::Acquire);
    let safe = GPIO_SAFE_SEQUENCE.load(Ordering::Acquire);
    let fail = match GPIO_FAULT_MODE.load(Ordering::Acquire) {
        1 => safe != 0 && high & 0x40 == 0,
        2 => safe == 0 && high & 0x40 != 0,
        _ => false,
    };
    if FAIL_SAFE_SERVICE.load(Ordering::Acquire) && fail {
        GPIO_FAULT_SEQUENCE.store(GPIO_SEQUENCE.load(Ordering::Acquire), Ordering::Release);
        GPIO_FAULT_HIGH.store(high, Ordering::Release);
        GPIO_FAULTS.fetch_add(1, Ordering::AcqRel);
        return -1;
    }
    GPIO_SERVICED.store(GPIO_DESIRED.load(Ordering::Acquire), Ordering::Release);
    0
}
unsafe extern "C" fn gpio_publish(
    _: *mut rptadv_gpio_device,
    action: *const rptadv_gpio_output_action,
) -> i32 {
    let high = unsafe { (*action).gpio_output_mask } as usize;
    let prior = GPIO_DESIRED.swap(high, Ordering::AcqRel);
    let sequence = GPIO_SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1;
    if FAIL_SAFE_SERVICE.load(Ordering::Acquire) && prior & 0x40 != 0 && high & 0x40 == 0 {
        GPIO_SAFE_SEQUENCE.store(sequence, Ordering::Release);
    }
    0
}
unsafe extern "C" fn inputs(
    _: *const rptadv_gpio_device,
    out: *mut rptadv_gpio_input_snapshot,
) -> i32 {
    unsafe {
        (*out).abi_version = 1;
        (*out).online = 1;
        (*out).cor_active = u32::from(INPUT.load(Ordering::Acquire));
        (*out).ctcss_active = 1;
    }
    0
}
unsafe extern "C" fn gpio_stats(
    _: *const rptadv_gpio_device,
    out: *mut rptadv_gpio_device_stats,
) -> i32 {
    unsafe {
        (*out).abi_version = 1;
        (*out).online = 1;
        (*out).input_read_count = 1;
    }
    0
}
unsafe extern "C" fn discover(_: *mut rptadv_gpio_device_list) -> i32 {
    0
}
unsafe extern "C" fn eeprom(_: *mut rptadv_gpio_device, out: *mut rptadv_gpio_eeprom_image) -> i32 {
    unsafe {
        (*out).abi_version = 1;
        (*out).checksum_valid = 1;
        (*out).magic_valid = 1;
    }
    0
}
unsafe extern "C" fn capabilities(
    _: *const rptadv_gpio_device,
    out: *mut rptadv_gpio_port_capabilities,
) -> i32 {
    unsafe {
        (*out).input_pin_mask = 0xfb;
        (*out).output_pin_mask = 0xfb;
    }
    0
}

unsafe extern "C" fn unsupported_audio_mixer_create(
    _config: *const rptadv_audio_mixer_config,
    _mixer: *mut *mut rptadv_audio_mixer,
) -> rptadv_audio_result {
    LAST_OPERATION.store(1, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_audio_usb_device_resolve(
    _identity: *const rptadv_audio_usb_device_identity,
    _selection: *mut rptadv_audio_usb_device_selection,
) -> rptadv_audio_result {
    LAST_OPERATION.store(2, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_audio_alsa_device_resolve(
    _native_name: *const ::std::os::raw::c_char,
    _input_channels: u32,
    _output_channels: u32,
    _selection: *mut rptadv_audio_usb_device_selection,
) -> rptadv_audio_result {
    LAST_OPERATION.store(3, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_audio_stream_create_with_output_timing(
    _config: *const rptadv_audio_stream_config,
    _timed_worker: rptadv_audio_timed_transmit_worker,
    _stream: *mut *mut rptadv_audio_stream,
) -> rptadv_audio_result {
    LAST_OPERATION.store(4, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

pub(super) fn audio(
    capability: *const c_char,
    reader: unsafe extern "C" fn() -> u64,
) -> rptadv_audio_adapter_descriptor {
    rptadv_audio_adapter_descriptor {
        struct_size: size_of::<rptadv_audio_adapter_descriptor>() as u32,
        abi_version: 2,
        capability_name: capability,
        stream_destroy: Some(stream_destroy),
        stream_create: Some(stream_create),
        stream_start: Some(stream_start),
        stream_stop: Some(stream_stop),
        stream_get_stats: Some(stream_stats),
        mixer_create: Some(unsupported_audio_mixer_create),
        mixer_get_range_centibels: Some(range),
        mixer_get_centibels: Some(get_i64),
        mixer_set_centibels: Some(set_i64),
        mixer_destroy: Some(mixer_destroy),
        mixer_create_for_usb_interface: Some(mixer_create),
        mixer_get_range_steps: Some(range),
        mixer_get_steps: Some(get_i64),
        mixer_set_steps: Some(set_i64),
        mixer_get_normalized: Some(get),
        mixer_set_normalized: Some(set),
        mixer_get_switch: Some(get_switch),
        mixer_set_switch: Some(set_switch),
        usb_device_resolve: Some(unsupported_audio_usb_device_resolve),
        usb_device_select: Some(select),
        stream_get_timing: Some(timing),
        cm119_mixer_paths_resolve: Some(paths),
        alsa_device_resolve: Some(unsupported_audio_alsa_device_resolve),
        stream_create_with_output_timing: Some(unsupported_audio_stream_create_with_output_timing),
        monotonic_now_ns: Some(reader),
    }
}

unsafe extern "C" fn unsupported_gpio_device_write_eeprom(
    _device: *mut rptadv_gpio_device,
    _image: *mut rptadv_gpio_eeprom_image,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(5, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_open(
    _config: *const rptadv_gpio_parallel_config,
    _device: *mut *mut rptadv_gpio_parallel_device,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(6, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_publish_outputs(
    _device: *mut rptadv_gpio_parallel_device,
    _action: *const rptadv_gpio_parallel_output_action,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(7, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_service(
    _device: *mut rptadv_gpio_parallel_device,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(8, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_control_write_data(
    _device: *mut rptadv_gpio_parallel_device,
    _data: u32,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(9, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_get_inputs(
    _device: *const rptadv_gpio_parallel_device,
    _snapshot: *mut rptadv_gpio_parallel_input_snapshot,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(10, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_get_stats(
    _device: *const rptadv_gpio_parallel_device,
    _stats: *mut rptadv_gpio_parallel_stats,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(11, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_device_publish_inverting_pulse(
    _device: *mut rptadv_gpio_device,
    _action: *const rptadv_gpio_cm119_inverting_pulse_action,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(12, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_publish_inverting_pulse(
    _device: *mut rptadv_gpio_parallel_device,
    _action: *const rptadv_gpio_parallel_inverting_pulse_action,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(13, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn gpio_schedule_inverting_pulse(
    _device: *mut rptadv_gpio_device,
    _action: *const rptadv_gpio_cm119_scheduled_inverting_pulse_action,
) -> rptadv_gpio_result {
    0
}

unsafe extern "C" fn unsupported_gpio_parallel_schedule_inverting_pulse(
    _device: *mut rptadv_gpio_parallel_device,
    _action: *const rptadv_gpio_parallel_scheduled_inverting_pulse_action,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(15, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_set_binary_channel(
    _device: *mut rptadv_gpio_parallel_device,
    _channel: u8,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(16, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_program_rtx(
    _device: *mut rptadv_gpio_parallel_device,
    _rx_frequency_hz: u32,
    _tx_frequency_hz: u32,
    _transmitting: u32,
    _high_power: u32,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(17, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_clear_rtx_transmit(
    _device: *mut rptadv_gpio_parallel_device,
) -> rptadv_gpio_result {
    LAST_OPERATION.store(18, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_create(
    _arg1: *const rptadv_gpio_context_config,
    _arg2: *const rptadv_gpio_transport,
    _arg3: *mut *mut rptadv_gpio_context,
) -> i32 {
    LAST_OPERATION.store(19, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_get_port(
    _arg1: *const rptadv_gpio_context,
    _arg2: u64,
    _arg3: *mut *const rptadv_gpio_port,
) -> i32 {
    LAST_OPERATION.store(20, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_snapshot(
    _arg1: *const rptadv_gpio_context,
    _arg2: *mut rptadv_gpio_safety_snapshot,
) -> i32 {
    LAST_OPERATION.store(21, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_port_snapshot(
    _arg1: *const rptadv_gpio_port,
    _arg2: *mut rptadv_gpio_port_snapshot,
) -> i32 {
    LAST_OPERATION.store(22, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_capabilities(
    _arg1: *const rptadv_gpio_parallel_device,
    _arg2: *mut rptadv_gpio_port_capabilities,
) -> i32 {
    LAST_OPERATION.store(23, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_initialize(
    _arg1: *mut rptadv_gpio_context,
    _arg2: u64,
) -> i32 {
    LAST_OPERATION.store(24, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_port_ready(
    _arg1: *const rptadv_gpio_port,
    _arg2: u32,
) -> i32 {
    LAST_OPERATION.store(25, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_request_arm(
    _arg1: *mut rptadv_gpio_context,
    _arg2: u32,
    _arg3: u64,
) -> i32 {
    LAST_OPERATION.store(26, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_recover(
    _arg1: *mut rptadv_gpio_context,
    _arg2: u64,
) -> i32 {
    LAST_OPERATION.store(27, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_context_service(
    _arg1: *mut rptadv_gpio_context,
    _arg2: u64,
) -> i32 {
    LAST_OPERATION.store(28, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_port_publish(
    _arg1: *const rptadv_gpio_port,
    _arg2: *const rptadv_gpio_output_intent,
) -> i32 {
    LAST_OPERATION.store(29, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_port_retire(
    _arg1: *mut rptadv_gpio_context,
    _arg2: u64,
    _arg3: u32,
) -> i32 {
    LAST_OPERATION.store(30, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

unsafe extern "C" fn unsupported_gpio_parallel_close_outputs(
    _device: *mut rptadv_gpio_parallel_device,
    _outputs: u32,
) -> i32 {
    LAST_OPERATION.store(31, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    -1
}

pub(super) fn gpio(capability: *const c_char) -> rptadv_gpio_adapter_descriptor {
    rptadv_gpio_adapter_descriptor {
        struct_size: size_of::<rptadv_gpio_adapter_descriptor>() as u32,
        abi_version: 1,
        capability_name: capability,
        context_destroy: None,
        context_disarm: None,
        parallel_close: Some(unsupported_parallel_close),
        device_probe: Some(probe),
        device_open: Some(gpio_open),
        device_publish_outputs: Some(gpio_publish),
        device_service: Some(gpio_service),
        device_get_inputs: Some(inputs),
        device_get_stats: Some(gpio_stats),
        device_close: Some(gpio_close),
        device_discover: Some(discover),
        device_read_eeprom: Some(eeprom),
        device_write_eeprom: Some(unsupported_gpio_device_write_eeprom),
        parallel_open: Some(unsupported_gpio_parallel_open),
        parallel_publish_outputs: Some(unsupported_gpio_parallel_publish_outputs),
        parallel_service: Some(unsupported_gpio_parallel_service),
        parallel_control_write_data: Some(unsupported_gpio_parallel_control_write_data),
        parallel_get_inputs: Some(unsupported_gpio_parallel_get_inputs),
        parallel_get_stats: Some(unsupported_gpio_parallel_get_stats),
        device_publish_inverting_pulse: Some(unsupported_gpio_device_publish_inverting_pulse),
        parallel_publish_inverting_pulse: Some(unsupported_gpio_parallel_publish_inverting_pulse),
        device_schedule_inverting_pulse: Some(gpio_schedule_inverting_pulse),
        parallel_schedule_inverting_pulse: Some(unsupported_gpio_parallel_schedule_inverting_pulse),
        parallel_set_binary_channel: Some(unsupported_gpio_parallel_set_binary_channel),
        parallel_program_rtx: Some(unsupported_gpio_parallel_program_rtx),
        parallel_clear_rtx_transmit: Some(unsupported_gpio_parallel_clear_rtx_transmit),
        context_create: Some(unsupported_gpio_context_create),
        context_get_port: Some(unsupported_gpio_context_get_port),
        context_snapshot: Some(unsupported_gpio_context_snapshot),
        port_snapshot: Some(unsupported_gpio_port_snapshot),
        device_capabilities: Some(capabilities),
        parallel_capabilities: Some(unsupported_gpio_parallel_capabilities),
        context_initialize: Some(unsupported_gpio_context_initialize),
        port_ready: Some(unsupported_gpio_port_ready),
        context_request_arm: Some(unsupported_gpio_context_request_arm),
        context_recover: Some(unsupported_gpio_context_recover),
        context_service: Some(unsupported_gpio_context_service),
        port_publish: Some(unsupported_gpio_port_publish),
        port_retire: Some(unsupported_gpio_port_retire),
        parallel_close_outputs: Some(unsupported_gpio_parallel_close_outputs),
    }
}

unsafe extern "C" fn unsupported_parallel_close(_: *mut rptadv_gpio_parallel_device) {
    LAST_OPERATION.store(32, Ordering::Release);
    UNEXPECTED.fetch_add(1, Ordering::Relaxed);
}

pub(super) static LAST_OPERATION: AtomicUsize = AtomicUsize::new(0);

pub(super) static TX_TONE: AtomicUsize = AtomicUsize::new(0);

pub(super) static PARTIAL_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
pub(super) static PARTIAL_OWNERS: AtomicUsize = AtomicUsize::new(0);

pub(super) static INPUT_FREQUENCY_HZ: AtomicUsize = AtomicUsize::new(1000);
