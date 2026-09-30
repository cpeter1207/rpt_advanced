use super::*;
use rpt_advanced_core::{
    media::Cancellation,
    runtime::{NativeFilePreparer as _, NativeSpeechPreparer as _},
};
use rptadv_file_adapter::abi as file_abi;
use rptadv_file_adapter::rptadv_file_adapter_descriptor;
use rptadv_speech_adapter::abi as speech_abi;
use rptadv_speech_adapter::rptadv_speech_adapter_descriptor;
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

impl NativeMediaPreparer {
    fn new(
        ffmpeg: &Path,
        piper: &Path,
        temporary_directory: &Path,
        timeout_ms: u32,
    ) -> Result<Self, MediaError> {
        // SAFETY: the linked descriptor has a process-lifetime readable ABI prefix.
        unsafe {
            Self::from_descriptors(
                rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter_descriptor().cast(),
                ffmpeg,
                piper,
                temporary_directory,
                timeout_ms,
                (ast_replace_sigchld, ast_unreplace_sigchld),
            )
        }
    }
}

static ACQUIRED: AtomicUsize = AtomicUsize::new(0);
static RELEASED: AtomicUsize = AtomicUsize::new(0);
#[unsafe(no_mangle)]
extern "C" fn ast_replace_sigchld() {
    ACQUIRED.fetch_add(1, Ordering::Relaxed);
}
#[unsafe(no_mangle)]
extern "C" fn ast_unreplace_sigchld() {
    RELEASED.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn finite_ring_conversion_preserves_duration_and_short_impulse_tail() {
    for (rate, input, length) in [
        (22050, vec![0.5; 2205], 4800),
        (44100, vec![0.5; 7], 8),
        (48000, vec![0.5; 100], 100),
        (8000, vec![0.5], 6),
        (96000, vec![0.5; 960], 480),
        (192000, vec![0.5; 1920], 480),
    ] {
        let result = native(rate, &input, &Cancellation::default()).unwrap();
        assert_eq!(result.sample_rate_hz(), 48000);
        assert_eq!(result.samples().len(), length);
        assert!(result.samples().last().unwrap().abs() > 0.0001);
        assert!(
            result
                .samples()
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 1.0)
        );
    }
    let token = Cancellation::default();
    token.cancel();
    assert_eq!(native(22050, &[0.5], &token), Err(MediaError::Cancelled));
}

#[test]
fn incompatible_media_tables_are_rejected_before_context_creation() {
    let path = Path::new("unused");
    let file = unsafe { rptadv_file_adapter_descriptor().read() };
    let speech = unsafe { rptadv_speech_adapter_descriptor().read() };
    let open = |file: *const file_abi::Descriptor, speech: *const speech_abi::Descriptor| unsafe {
        NativeMediaPreparer::from_descriptors(
            file.cast(),
            speech.cast(),
            path,
            path,
            path,
            1,
            (ast_replace_sigchld, ast_unreplace_sigchld),
        )
    };
    let prefix = [8_u32, ffi::RPTADV_MEDIA_ABI_VERSION];
    for (f, s) in [
        (ptr::null(), ptr::from_ref(&speech)),
        (ptr::from_ref(&file), ptr::null()),
        (prefix.as_ptr().cast(), ptr::from_ref(&speech)),
        (ptr::from_ref(&file), prefix.as_ptr().cast()),
    ] {
        assert!(matches!(open(f, s), Err(MediaError::IncompatibleAdapter)));
    }
    let mut invalid = [file; 8];
    invalid[0].abi_version = 99;
    invalid[1].capability[0] = 0;
    invalid[2].create = None;
    invalid[3].destroy = None;
    invalid[4].open_file = None;
    invalid[5].read_stream = None;
    invalid[6].close_stream = None;
    invalid[7].struct_size = 8;
    for table in invalid {
        assert!(matches!(
            open(ptr::from_ref(&table), ptr::from_ref(&speech)),
            Err(MediaError::IncompatibleAdapter)
        ));
    }
    let mut invalid = [speech; 8];
    invalid[0].abi_version = 99;
    invalid[1].capability[0] = 0;
    invalid[2].create = None;
    invalid[3].destroy = None;
    invalid[4].open_speech = None;
    invalid[5].read_stream = None;
    invalid[6].close_stream = None;
    invalid[7].struct_size = 8;
    for table in invalid {
        assert!(matches!(
            open(ptr::from_ref(&file), ptr::from_ref(&table)),
            Err(MediaError::IncompatibleAdapter)
        ));
    }
    for (ffmpeg, piper, temporary, timeout) in [
        (Path::new("bad\0path"), path, path, 1),
        (path, Path::new("bad\0piper"), path, 1),
        (path, path, Path::new("bad\0directory"), 1),
        (path, path, path, 0),
    ] {
        assert!(matches!(
            NativeMediaPreparer::new(ffmpeg, piper, temporary, timeout),
            Err(MediaError::InvalidRequest)
        ));
    }
    unsafe extern "C" fn empty_file_context(
        _: *const file_abi::RawConfig,
        _: *mut *mut c_void,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn empty_speech_context(
        _: *const speech_abi::RawConfig,
        _: *mut *mut c_void,
    ) -> i32 {
        0
    }
    let mut f = file;
    f.create = Some(empty_file_context);
    assert!(matches!(
        open(ptr::from_ref(&f), ptr::from_ref(&speech)),
        Err(MediaError::IncompatibleAdapter)
    ));
    let mut s = speech;
    s.create = Some(empty_speech_context);
    assert!(matches!(
        open(ptr::from_ref(&file), ptr::from_ref(&s)),
        Err(MediaError::IncompatibleAdapter)
    ));
}

#[test]
fn second_provider_creation_failure_destroys_only_the_first_owned_context() {
    static DESTROYED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn create(
        config: *const file_abi::RawConfig,
        output: *mut *mut c_void,
    ) -> i32 {
        assert_eq!(
            unsafe { CStr::from_ptr((*config).executable) },
            c"file-only"
        );
        unsafe {
            *output = 1_usize as *mut c_void;
        }
        0
    }
    unsafe extern "C" fn destroy(context: *mut c_void) {
        assert_eq!(context as usize, 1);
        DESTROYED.fetch_add(1, Ordering::Relaxed);
    }
    unsafe extern "C" fn reject(config: *const speech_abi::RawConfig, _: *mut *mut c_void) -> i32 {
        assert_eq!(
            unsafe { CStr::from_ptr((*config).executable) },
            c"speech-only"
        );
        -3
    }
    let mut file = unsafe { rptadv_file_adapter_descriptor().read() };
    let mut speech = unsafe { rptadv_speech_adapter_descriptor().read() };
    file.create = Some(create);
    file.destroy = Some(destroy);
    speech.create = Some(reject);
    assert!(matches!(
        unsafe {
            NativeMediaPreparer::from_descriptors(
                ptr::from_ref(&file).cast(),
                ptr::from_ref(&speech).cast(),
                Path::new("file-only"),
                Path::new("speech-only"),
                Path::new("unused"),
                1,
                (ast_replace_sigchld, ast_unreplace_sigchld),
            )
        },
        Err(MediaError::Io)
    ));
    assert_eq!(DESTROYED.load(Ordering::Relaxed), 1);
}

thread_local! {
    static RING_FAILURE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static CANCEL_DURING_RENDER: std::cell::Cell<*const Cancellation> = const { std::cell::Cell::new(ptr::null()) };
}
unsafe extern "C" fn ring_create(_: *const ffi::rpcr3_config, _: *mut *mut ffi::rpcr3_ring) -> i32 {
    if RING_FAILURE.get() == 0 { -1 } else { 0 }
}
unsafe extern "C" fn ring_push(_: *mut ffi::rpcr3_ring, _: *const f32, _: u64, _: *mut u64) -> i32 {
    if RING_FAILURE.get() == 0 { -1 } else { 0 }
}
unsafe extern "C" fn ring_render(_: *mut ffi::rpcr3_ring, _: *mut f32, _: *mut bool) -> i32 {
    let token = CANCEL_DURING_RENDER.get();
    if !token.is_null() {
        // SAFETY: the test keeps this token borrowed until synchronous conversion returns.
        unsafe { &*token }.cancel();
    }
    if RING_FAILURE.get() == 0 { -1 } else { 0 }
}

#[test]
fn finite_conversion_rejects_broken_ring_contracts_and_bounded_storage_failure() {
    let token = Cancellation::default();
    let convert = |pointer| {
        // SAFETY: every non-null table below is retained for process lifetime.
        unsafe { native_with_descriptor(48000, &[0.5], &token, pointer) }
    };
    assert_eq!(convert(ptr::null()), Err(MediaError::IncompatibleAdapter));
    // SAFETY: installed immutable descriptor has its complete current ABI layout.
    let original = unsafe { *ffi::rpcr3_descriptor() };
    for case in 0..11 {
        let mut api = original;
        match case {
            0 => api.struct_size = 8,
            1 => api.abi_version = 2,
            2 => api.capability_name = ptr::null(),
            3 => api.capability_name = c"other".as_ptr(),
            4 => api.ring_create = None,
            5 => api.ring_destroy = None,
            6 => api.ring_producer_push = None,
            7 => api.ring_consumer_render_sample = None,
            10 => api.ring_output_delay = None,
            _ => {
                RING_FAILURE.set(case - 8);
                api.ring_create = Some(ring_create);
            }
        }
        assert_eq!(
            convert(Box::leak(Box::new(api))),
            Err(MediaError::IncompatibleAdapter)
        );
    }
    for failure in 0..2 {
        RING_FAILURE.set(failure);
        for push in [false, true] {
            let mut api = original;
            if push {
                api.ring_producer_push = Some(ring_push);
            } else {
                api.ring_consumer_render_sample = Some(ring_render);
            }
            assert_eq!(
                convert(Box::leak(Box::new(api))),
                Err(MediaError::InvalidOutput)
            );
        }
    }
    let mut api = original;
    api.ring_consumer_render_sample = Some(ring_render);
    CANCEL_DURING_RENDER.set(&token);
    assert_eq!(
        convert(Box::leak(Box::new(api))),
        Err(MediaError::Cancelled)
    );
    CANCEL_DURING_RENDER.set(ptr::null());
    assert_eq!(
        crate::fixture::fail_allocation(size_of::<f32>(), || native(
            48000,
            &[0.5],
            &Cancellation::default()
        )),
        Err(MediaError::Io)
    );
}

#[test]
fn invalid_file_and_speech_strings_fail_before_provider_dispatch() {
    let path = Path::new("unused");
    let adapter = NativeMediaPreparer::new(path, path, path, 1).unwrap();
    let cancellation = Cancellation::default();
    assert_eq!(
        adapter.file(&FileRequest {
            path: Path::new("bad\0file"),
            cancellation: &cancellation
        }),
        Err(MediaError::InvalidRequest)
    );
    for (text, model) in [("bad\0speech", path), ("hello", Path::new("bad\0model"))] {
        assert_eq!(
            adapter.speech(&SpeechRequest {
                text,
                model,
                speed_percent: 100,
                level_db: 0,
                cancellation: &cancellation
            }),
            Err(MediaError::InvalidRequest)
        );
    }
}

#[test]
fn malformed_and_failed_outputs_release_handles_once_and_keep_error_meaning() {
    static RELEASES: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn close(handle: *mut c_void) {
        assert_eq!(handle as usize, 1);
        RELEASES.fetch_add(1, Ordering::Relaxed);
    }
    unsafe extern "C" fn eof(
        _: *mut c_void,
        _: *const ffi::rptadv_media_cancellation,
        _: *mut f32,
        _: usize,
        count: *mut usize,
    ) -> i32 {
        unsafe { *count = 0 };
        0
    }
    unsafe extern "C" fn bad_count(
        _: *mut c_void,
        _: *const ffi::rptadv_media_cancellation,
        _: *mut f32,
        capacity: usize,
        count: *mut usize,
    ) -> i32 {
        unsafe { *count = capacity + 1 };
        0
    }
    let path = Path::new("unused");
    let mut adapter = NativeMediaPreparer::new(path, path, path, 1).unwrap();
    adapter.file.close_stream = close;
    adapter.file.read_stream = eof;
    let token = Cancellation::default();
    for (code, expected) in [
        (-1, MediaError::InvalidRequest),
        (-2, MediaError::Unavailable),
        (-3, MediaError::Io),
        (-4, MediaError::ProcessFailed),
        (-5, MediaError::TimedOut),
        (-6, MediaError::Cancelled),
        (-7, MediaError::InvalidOutput),
        (-99, MediaError::IncompatibleAdapter),
        (0, MediaError::InvalidOutput),
    ] {
        assert_eq!(
            adapter.file.collect_stream(&token, |output| {
                unsafe {
                    (*output).handle = 1_usize as *mut c_void;
                    (*output).sample_rate_hz = 48000;
                }
                code
            }),
            Err(expected)
        );
    }
    assert_eq!(RELEASES.load(Ordering::Relaxed), 9);
    assert_eq!(
        adapter.file.collect_stream(&token, |_| 0),
        Err(MediaError::InvalidOutput)
    );
    assert_eq!(
        adapter.file.collect_stream(&token, |output| {
            unsafe {
                (*output).handle = 1_usize as *mut c_void;
            }
            0
        }),
        Err(MediaError::InvalidOutput)
    );
    assert_eq!(RELEASES.load(Ordering::Relaxed), 10);
    assert_eq!(
        adapter.file.collect_stream(&token, |output| {
            unsafe {
                (*output).handle = 1_usize as *mut c_void;
                (*output).sample_rate_hz = 48000;
            }
            0
        }),
        Err(MediaError::InvalidOutput)
    );
    adapter.file.read_stream = bad_count;
    assert_eq!(
        adapter.file.collect_stream(&token, |output| {
            unsafe {
                (*output).handle = 1_usize as *mut c_void;
                (*output).sample_rate_hz = 48000;
            }
            0
        }),
        Err(MediaError::InvalidOutput)
    );
    for (rate, source) in [
        (0, vec![0.0]),
        (48000, vec![]),
        (48000, vec![f32::NAN]),
        (48000, vec![1.1]),
    ] {
        assert_eq!(
            native(rate, &source, &token),
            Err(MediaError::InvalidOutput)
        );
    }
    assert_eq!(RELEASES.load(Ordering::Relaxed), 12);
}

#[test]
fn descriptor_file_decode_uses_host_reaper_and_retains_no_temporary_files() {
    let directory =
        std::env::temp_dir().join(format!("rptadv-native-media-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("source.wav");
    let mut wave = Vec::from(*b"RIFF\x5e\x11\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0data\x3a\x11\0\0");
    for _ in 0..2205 {
        wave.extend_from_slice(&1000_i16.to_le_bytes());
    }
    fs::write(&path, wave).unwrap();
    let adapter = NativeMediaPreparer::new(
        Path::new("ffmpeg"),
        Path::new("missing-piper"),
        &directory,
        30000,
    )
    .unwrap();
    let token = Cancellation::default();
    let audio = adapter
        .file(&FileRequest {
            path: &path,
            cancellation: &token,
        })
        .unwrap();
    assert_eq!(audio.samples().len(), 4800);
    assert!((audio.samples()[2400] - 0.030517578).abs() < 0.0001);
    assert_eq!(ACQUIRED.load(Ordering::Relaxed), 1);
    assert_eq!(RELEASED.load(Ordering::Relaxed), 1);
    assert_eq!(
        adapter.speech(&SpeechRequest {
            text: "hello",
            model: &path,
            speed_percent: 100,
            level_db: 0,
            cancellation: &token
        }),
        Err(MediaError::Unavailable)
    );
    assert_eq!(ACQUIRED.load(Ordering::Relaxed), 2);
    assert_eq!(RELEASED.load(Ordering::Relaxed), 2);
    token.cancel();
    assert_eq!(
        adapter.file(&FileRequest {
            path: &path,
            cancellation: &token
        }),
        Err(MediaError::Cancelled)
    );
    drop(adapter);
    fs::remove_file(path).unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}
