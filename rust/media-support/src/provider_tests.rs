use super::*;

extern "C" fn no_op() {}
extern "C" fn not_cancelled(_: *const c_void) -> u32 {
    0
}

fn raw_config() -> RawConfig {
    RawConfig {
        struct_size: size_of::<RawConfig>() as u32,
        abi_version: ABI_VERSION,
        executable: c"ffmpeg".as_ptr(),
        timeout_ms: 100,
        reaper_acquire: None,
        reaper_release: None,
    }
}

#[cfg(file_adapter)]
fn test_stream() -> PcmStream {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(1),
        child_reaper: None,
    };
    let mut wave = Vec::from(
        *b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x03\0\x01\0\x80\xbb\0\0\0\xee\x02\0\x04\0\x20\0data\x04\0\0\0",
    );
    wave.extend_from_slice(&0.0_f32.to_le_bytes());
    let mut command = std::process::Command::new("cat");
    PcmStream::f32_wave(
        crate::process::ChildStream::spawn(&config, &mut command, Some(&wave), &|| false).unwrap(),
        &|| false,
    )
    .unwrap()
}

#[cfg(file_adapter)]
#[test]
fn truncated_finite_wave_data_is_rejected_after_complete_samples() {
    let config = Config {
        ffmpeg: "ffmpeg".into(),
        process_timeout: Duration::from_secs(1),
        child_reaper: None,
    };
    let mut wave = Vec::from(
        *b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x03\0\x01\0\x80\xbb\0\0\0\xee\x02\0\x04\0\x20\0data\x04\0\0\0",
    );
    wave[40..44].copy_from_slice(&8_u32.to_le_bytes());
    wave.extend_from_slice(&0.0_f32.to_le_bytes());
    let mut command = std::process::Command::new("cat");
    let child =
        crate::process::ChildStream::spawn(&config, &mut command, Some(&wave), &|| false).unwrap();
    let mut stream = PcmStream::f32_wave(child, &|| false).unwrap();
    let mut samples = [0.0; 4];
    assert_eq!(stream.read(&mut samples, &|| false).unwrap(), 1);
    assert_eq!(
        stream.read(&mut samples, &|| false),
        Err(MediaError::InvalidOutput)
    );
}

#[cfg(speech_adapter)]
fn test_stream() -> PcmStream {
    let config = Config {
        piper: "piper".into(),
        process_timeout: Duration::from_secs(1),
        child_reaper: None,
    };
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "printf '\\000\\000'"]);
    PcmStream::s16_raw(
        crate::process::ChildStream::spawn(&config, &mut command, None, &|| false).unwrap(),
        48_000,
        1.0,
    )
}

#[test]
fn truncated_config_is_rejected_before_full_config_access() {
    let mut truncated = Box::new([0_u8; size_of::<AbiHeader>()]);
    // SAFETY: unaligned write fills the complete allocated ABI prefix.
    unsafe {
        truncated
            .as_mut_ptr()
            .cast::<AbiHeader>()
            .write_unaligned(AbiHeader {
                struct_size: size_of::<AbiHeader>() as u32,
                abi_version: ABI_VERSION,
            });
    }
    let mut context = 1_usize as *mut c_void;
    assert_eq!(
        // SAFETY: the allocation contains exactly the readable ABI prefix.
        unsafe { create(truncated.as_ptr().cast(), &mut context) },
        -1
    );
    assert!(context.is_null());
}

#[test]
fn abi_rejects_invalid_creation_and_stream_arguments() {
    let mut context = 1_usize as *mut c_void;
    assert_eq!(unsafe { create(std::ptr::null(), &mut context) }, -1);
    assert!(context.is_null());
    assert_eq!(unsafe { create(&raw_config(), std::ptr::null_mut()) }, -1);

    let mut invalid = raw_config();
    invalid.timeout_ms = 0;
    assert_eq!(unsafe { create(&invalid, &mut context) }, -1);
    invalid = raw_config();
    invalid.executable = c"".as_ptr();
    assert_eq!(unsafe { create(&invalid, &mut context) }, -1);
    invalid = raw_config();
    invalid.reaper_acquire = Some(no_op);
    assert_eq!(unsafe { create(&invalid, &mut context) }, -1);
    invalid.reaper_release = Some(no_op);
    assert_eq!(unsafe { create(&invalid, &mut context) }, 0);
    unsafe { destroy(context) };
    context = std::ptr::null_mut();

    assert_eq!(unsafe { create(&raw_config(), &mut context) }, 0);
    #[cfg(file_adapter)]
    {
        let cancellation = RawCancellation {
            context: std::ptr::null(),
            is_cancelled: not_cancelled,
        };
        let mut audio = empty_stream();
        assert_eq!(
            unsafe {
                open_file(
                    std::ptr::null(),
                    c"/missing".as_ptr(),
                    &cancellation,
                    &mut audio,
                )
            },
            -1
        );
        assert_eq!(
            unsafe { open_file(context, c"/missing".as_ptr(), std::ptr::null(), &mut audio,) },
            -1
        );
        assert_eq!(
            unsafe { open_file(context, std::ptr::null(), &cancellation, &mut audio) },
            -1
        );
        assert_eq!(
            unsafe {
                open_file(
                    context,
                    c"/missing".as_ptr(),
                    &cancellation,
                    std::ptr::null_mut(),
                )
            },
            -1
        );
    }
    unsafe {
        close_stream(std::ptr::null_mut());
        destroy(context);
        destroy(std::ptr::null_mut());
    }
}

#[test]
#[cfg(speech_adapter)]
fn abi_rejects_invalid_speech_streams() {
    let mut context = std::ptr::null_mut();
    let mut config = raw_config();
    config.executable = c"/definitely/missing/piper".as_ptr();
    assert_eq!(unsafe { create(&config, &mut context) }, 0);
    let cancellation = RawCancellation {
        context: std::ptr::null(),
        is_cancelled: not_cancelled,
    };
    let mut audio = empty_stream();
    let valid = RawSpeechRequest {
        text: c"text".as_ptr(),
        model: c"model".as_ptr(),
        speed_percent: 100,
        level_db: 0,
    };
    assert_eq!(
        unsafe { open_speech(std::ptr::null(), &valid, &cancellation, &mut audio,) },
        -1
    );
    assert_eq!(
        unsafe { open_speech(context, &valid, std::ptr::null(), &mut audio) },
        -1
    );
    assert_eq!(
        unsafe { open_speech(context, std::ptr::null(), &cancellation, &mut audio) },
        -1
    );
    let invalid_utf8 = std::ffi::CString::new([0xff]).unwrap();
    let invalid = RawSpeechRequest {
        text: invalid_utf8.as_ptr(),
        model: c"model".as_ptr(),
        speed_percent: 100,
        level_db: 0,
    };
    assert_eq!(
        unsafe { open_speech(context, &invalid, &cancellation, &mut audio) },
        -1
    );
    let invalid = RawSpeechRequest {
        text: c"text".as_ptr(),
        model: std::ptr::null(),
        speed_percent: 100,
        level_db: 0,
    };
    assert_eq!(
        unsafe { open_speech(context, &invalid, &cancellation, &mut audio) },
        -1
    );
    assert_eq!(
        unsafe { open_speech(context, &valid, &cancellation, &mut audio) },
        -2
    );
    unsafe { destroy(context) };
}

#[test]
fn boundary_maps_internal_output_and_panic_failures() {
    for (error, status) in [
        (MediaError::InvalidRequest, -1),
        (MediaError::Unavailable, -2),
        (MediaError::Io, -3),
        (MediaError::ProcessFailed, -4),
        (MediaError::TimedOut, -5),
        (MediaError::Cancelled, -6),
        (MediaError::InvalidOutput, -7),
        (MediaError::IncompatibleAdapter, -3),
    ] {
        assert_eq!(boundary(Ok(Err(error))), status);
    }
    assert_eq!(
        boundary(catch_unwind(|| panic!("contained test panic"))),
        -3
    );
}

#[test]
fn abi_opens_and_streams_source_rate_media() {
    use std::os::unix::ffi::OsStrExt;

    let directory =
        std::env::temp_dir().join(format!("rptadv-provider-test-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    #[cfg(speech_adapter)]
    let piper_path = std::env::current_exe()
        .unwrap()
        .with_file_name(format!("rptadv-piper-fixture-{}", std::process::id()));
    #[cfg(speech_adapter)]
    assert!(
        std::process::Command::new("rustc")
            .args([
                "--edition=2024",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../media-support/tests/fixtures/piper.rs"
                ),
                "-o"
            ])
            .arg(&piper_path)
            .status()
            .unwrap()
            .success()
    );
    let source = directory.join("source.wav");
    let mut wave = Vec::from(
        *b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0data\x04\0\0\0",
    );
    wave.extend_from_slice(&[0, 128, 255, 127]);
    std::fs::write(&source, wave).unwrap();

    #[cfg(speech_adapter)]
    let piper = std::ffi::CString::new(piper_path.as_os_str().as_bytes()).unwrap();
    #[cfg(speech_adapter)]
    let model_path = directory.join("001.000000.onnx");
    #[cfg(speech_adapter)]
    let model = std::ffi::CString::new(model_path.as_os_str().as_bytes()).unwrap();
    #[cfg(speech_adapter)]
    {
        let mut model_config = model_path.as_os_str().to_os_string();
        model_config.push(".json");
        std::fs::write(model_config, br#"{"audio":{"sample_rate":22050}}"#).unwrap();
    }
    #[cfg(file_adapter)]
    let source_path = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
    let config = RawConfig {
        #[cfg(speech_adapter)]
        executable: piper.as_ptr(),
        timeout_ms: 30_000,
        ..raw_config()
    };
    let cancellation = RawCancellation {
        context: std::ptr::null(),
        is_cancelled: not_cancelled,
    };
    let mut context = std::ptr::null_mut();
    assert_eq!(unsafe { create(&config, &mut context) }, 0);
    let mut stream = empty_stream();
    #[cfg(file_adapter)]
    assert_eq!(
        unsafe { open_file(context, source_path.as_ptr(), &cancellation, &mut stream) },
        0
    );
    #[cfg(speech_adapter)]
    {
        let request = RawSpeechRequest {
            text: c"Identifier; $(not a command)\n".as_ptr(),
            model: model.as_ptr(),
            speed_percent: 100,
            level_db: 0,
        };
        assert_eq!(
            unsafe { open_speech(context, &request, &cancellation, &mut stream) },
            0
        );
    }
    assert_eq!(stream.sample_rate_hz, 22050);
    let mut samples = Vec::new();
    let mut chunk = [0.0; 8];
    loop {
        let mut count = usize::MAX;
        assert_eq!(
            unsafe {
                read_stream(
                    stream.handle,
                    &cancellation,
                    chunk.as_mut_ptr(),
                    chunk.len(),
                    &mut count,
                )
            },
            0
        );
        assert!(count <= chunk.len());
        if count == 0 {
            break;
        }
        samples.extend_from_slice(&chunk[..count]);
    }
    #[cfg(file_adapter)]
    assert_eq!(samples.len(), 2);
    #[cfg(speech_adapter)]
    assert_eq!(samples.len(), 2205);
    unsafe {
        close_stream(stream.handle);
        destroy(context);
    }
    std::fs::remove_file(source).unwrap();
    #[cfg(speech_adapter)]
    std::fs::remove_file(piper_path).unwrap();
    #[cfg(speech_adapter)]
    {
        let mut model_config = model_path.as_os_str().to_os_string();
        model_config.push(".json");
        std::fs::remove_file(model_config).unwrap();
    }
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn cancellation_after_stream_open_discards_the_owned_handle() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    extern "C" fn cancel_on_second_call(context: *const c_void) -> u32 {
        let calls = unsafe { &*context.cast::<AtomicUsize>() };
        u32::from(calls.fetch_add(1, Ordering::Relaxed) != 0)
    }

    let preparation = Preparation::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_millis(100),
        child_reaper: None,
    });
    let calls = AtomicUsize::new(0);
    let cancellation = RawCancellation {
        context: std::ptr::from_ref(&calls).cast(),
        is_cancelled: cancel_on_second_call,
    };
    let mut output = RawStream {
        handle: 1_usize as *mut c_void,
        sample_rate_hz: 1,
    };
    let (owner, cancellation) = unsafe {
        open_arguments(
            std::ptr::from_ref(&preparation).cast(),
            &cancellation,
            &mut output,
        )
    }
    .unwrap();
    assert!(std::ptr::eq(owner, &preparation));
    assert_eq!(cancel_on_second_call(cancellation.context), 0);
    let result = unsafe { publish(cancellation, &mut output, test_stream()) };
    assert_eq!(result, Err(MediaError::Cancelled));
    assert!(output.handle.is_null());

    let cancellation = RawCancellation {
        context: std::ptr::null(),
        is_cancelled: not_cancelled,
    };
    unsafe { publish(&cancellation, &mut output, test_stream()) }.unwrap();
    assert!(!output.handle.is_null());
    unsafe { close_stream(output.handle) };
}

#[test]
fn read_stream_rejects_invalid_output_and_cancellation_arguments() {
    let cancellation = RawCancellation {
        context: std::ptr::null(),
        is_cancelled: not_cancelled,
    };
    let mut stream = test_stream();
    let handle = std::ptr::from_mut(&mut stream).cast();
    let mut output = [0.0_f32; 2];
    let mut count = usize::MAX;

    assert_eq!(
        unsafe {
            read_stream(
                handle,
                &cancellation,
                output.as_mut_ptr(),
                2,
                std::ptr::null_mut(),
            )
        },
        -1
    );
    assert_eq!(
        unsafe { read_stream(handle, &cancellation, std::ptr::null_mut(), 2, &mut count) },
        -1
    );
    assert_eq!(
        unsafe { read_stream(handle, &cancellation, output.as_mut_ptr(), 0, &mut count) },
        -1
    );
    assert_eq!(
        unsafe { read_stream(handle, std::ptr::null(), output.as_mut_ptr(), 2, &mut count) },
        -1
    );

    extern "C" fn cancelled(_: *const c_void) -> u32 {
        1
    }
    let cancellation = RawCancellation {
        context: std::ptr::null(),
        is_cancelled: cancelled,
    };
    assert_eq!(
        unsafe { read_stream(handle, &cancellation, output.as_mut_ptr(), 2, &mut count) },
        -6
    );
    assert_eq!(
        unsafe {
            read_stream(
                std::ptr::null_mut(),
                &RawCancellation {
                    context: std::ptr::null(),
                    is_cancelled: not_cancelled,
                },
                output.as_mut_ptr(),
                2,
                &mut count,
            )
        },
        -1
    );
}
