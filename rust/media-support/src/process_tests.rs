use super::*;
#[cfg(file_adapter)]
use std::{fs, path::PathBuf};
use std::{path::Path, sync::atomic::Ordering};

#[cfg(file_adapter)]
fn test_directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("rptadv-{name}-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
#[cfg(file_adapter)]
fn fifo_source_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let directory = test_directory("fifo");
    let path = directory.join("input");
    let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path_bytes is a live NUL-terminated pathname.
    assert_eq!(unsafe { libc::mkfifo(path_bytes.as_ptr(), 0o600) }, 0);
    let started = Instant::now();
    assert!(matches!(open_local(&path), Err(MediaError::Unavailable)));
    assert!(started.elapsed() < Duration::from_secs(1));
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
#[cfg(file_adapter)]
fn regular_file_source_is_opened() {
    let directory = test_directory("regular");
    let path = directory.join("input");
    fs::write(&path, b"audio").unwrap();
    assert!(open_local(&path).is_ok());
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[cfg(file_adapter)]
fn wave(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut body = b"WAVE".to_vec();
    for (name, data) in chunks {
        body.extend_from_slice(*name);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
        if data.len() % 2 != 0 {
            body.push(0);
        }
    }
    let mut riff = b"RIFF".to_vec();
    riff.extend_from_slice(&(body.len() as u32).to_le_bytes());
    riff.extend_from_slice(&body);
    riff
}

#[cfg(file_adapter)]
fn format_chunk(size: usize) -> Vec<u8> {
    let mut format = vec![0; size];
    if size >= 16 {
        format[0..2].copy_from_slice(&3_u16.to_le_bytes());
        format[2..4].copy_from_slice(&1_u16.to_le_bytes());
        format[4..8].copy_from_slice(&48_000_u32.to_le_bytes());
        format[8..12].copy_from_slice(&192_000_u32.to_le_bytes());
        format[12..14].copy_from_slice(&4_u16.to_le_bytes());
        format[14..16].copy_from_slice(&32_u16.to_le_bytes());
    }
    format
}

#[cfg(file_adapter)]
fn pcm_stream(bytes: &[u8]) -> Result<PcmStream, MediaError> {
    let config = Config {
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    let mut command = Command::new("cat");
    let child = ChildStream::spawn(&config, &mut command, Some(bytes), &|| false)?;
    PcmStream::f32_wave(child, &|| false)
}

#[test]
#[cfg(file_adapter)]
fn wave_stream_decodes_incrementally_and_drains_declared_data() {
    let mut data = 0.25_f32.to_le_bytes().to_vec();
    data.extend_from_slice(&(-0.5_f32).to_le_bytes());
    let format = format_chunk(16);
    let wave = wave(&[(b"fmt ", &format), (b"data", &data)]);
    let mut stream = pcm_stream(&wave).unwrap();
    assert_eq!(stream.sample_rate_hz(), 48_000);
    let mut output = [0.0; 2];
    assert_eq!(stream.read(&mut output, &|| false).unwrap(), 2);
    assert_eq!(output, [0.25, -0.5]);
    assert_eq!(stream.read(&mut output, &|| false).unwrap(), 0);
    assert_eq!(stream.read(&mut output, &|| false).unwrap(), 0);
}

#[test]
#[cfg(file_adapter)]
fn wave_stream_skips_unknown_and_odd_sized_chunks_and_accepts_unknown_data_size() {
    let odd_format = format_chunk(17);
    let mut data = 0.75_f32.to_le_bytes().to_vec();
    let unknown_size = u32::MAX.to_le_bytes();
    let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
    file.extend_from_slice(b"JUNK");
    file.extend_from_slice(&3_u32.to_le_bytes());
    file.extend_from_slice(&[1, 2, 3, 0]);
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&(odd_format.len() as u32).to_le_bytes());
    file.extend_from_slice(&odd_format);
    file.push(0);
    file.extend_from_slice(b"data");
    file.extend_from_slice(&unknown_size);
    file.append(&mut data);
    let riff_size = (file.len() - 8) as u32;
    file[4..8].copy_from_slice(&riff_size.to_le_bytes());

    let mut stream = pcm_stream(&file).unwrap();
    let mut output = [0.0; 1];
    assert_eq!(stream.read(&mut output, &|| false).unwrap(), 1);
    assert_eq!(output[0], 0.75);
    assert_eq!(stream.read(&mut output, &|| false).unwrap(), 0);
}

#[test]
#[cfg(file_adapter)]
fn wave_stream_rejects_invalid_headers_chunks_and_sample_formats() {
    assert!(matches!(
        pcm_stream(b"short"),
        Err(MediaError::InvalidOutput)
    ));
    let mut not_riff = wave(&[]);
    not_riff[..4].copy_from_slice(b"NOPE");
    assert!(matches!(
        pcm_stream(&not_riff),
        Err(MediaError::InvalidOutput)
    ));
    let mut not_wave = wave(&[]);
    not_wave[8..12].copy_from_slice(b"NOPE");
    assert!(matches!(
        pcm_stream(&not_wave),
        Err(MediaError::InvalidOutput)
    ));

    let short_format = format_chunk(15);
    assert!(matches!(
        pcm_stream(&wave(&[(b"fmt ", &short_format), (b"data", &[])])),
        Err(MediaError::InvalidOutput)
    ));
    let mut bad_format = format_chunk(16);
    bad_format[0..2].copy_from_slice(&1_u16.to_le_bytes());
    assert!(matches!(
        pcm_stream(&wave(&[(b"fmt ", &bad_format), (b"data", &[])])),
        Err(MediaError::InvalidOutput)
    ));
    assert!(matches!(
        pcm_stream(&wave(&[(b"data", &[])])),
        Err(MediaError::InvalidOutput)
    ));
    let mut misaligned = 1_f32.to_le_bytes().to_vec();
    misaligned.pop();
    assert!(matches!(
        pcm_stream(&wave(&[
            (b"fmt ", &format_chunk(16)),
            (b"data", &misaligned)
        ])),
        Err(MediaError::InvalidOutput)
    ));
}

#[test]
#[cfg(file_adapter)]
fn wave_stream_rejects_each_incompatible_format_field_and_duplicate_format() {
    for offset in [0, 2, 4, 8, 12, 14] {
        let mut format = format_chunk(16);
        match offset {
            0 => format[0..2].copy_from_slice(&1_u16.to_le_bytes()),
            2 => format[2..4].copy_from_slice(&2_u16.to_le_bytes()),
            4 => {
                format[4..8].copy_from_slice(&0_u32.to_le_bytes());
                format[8..12].copy_from_slice(&0_u32.to_le_bytes());
            }
            8 => format[8..12].copy_from_slice(&1_u32.to_le_bytes()),
            12 => format[12..14].copy_from_slice(&2_u16.to_le_bytes()),
            _ => format[14..16].copy_from_slice(&16_u16.to_le_bytes()),
        }
        assert!(matches!(
            pcm_stream(&wave(&[(b"fmt ", &format), (b"data", &[])])),
            Err(MediaError::InvalidOutput)
        ));
    }

    let valid = format_chunk(16);
    assert!(matches!(
        pcm_stream(&wave(&[
            (b"fmt ", &valid),
            (b"fmt ", &valid),
            (b"data", &[])
        ])),
        Err(MediaError::InvalidOutput)
    ));
    let oversized = format_chunk(257);
    assert!(matches!(
        pcm_stream(&wave(&[(b"fmt ", &oversized), (b"data", &[])])),
        Err(MediaError::InvalidOutput)
    ));
}

#[test]
#[cfg(file_adapter)]
fn empty_wave_data_is_rejected_after_declared_bytes_are_drained() {
    let mut bytes = wave(&[(b"fmt ", &format_chunk(16)), (b"data", &[])]);
    bytes.extend_from_slice(b"trailing bytes");
    let mut stream = pcm_stream(&bytes).unwrap();
    assert_eq!(
        stream.read(&mut [0.0; 1], &|| false),
        Err(MediaError::InvalidOutput)
    );
}

#[test]
#[cfg(file_adapter)]
fn pcm_reader_validates_requests_and_defers_errors_after_returned_samples() {
    let data = [0.5_f32.to_le_bytes(), f32::NAN.to_le_bytes()].concat();
    let mut stream = pcm_stream(&wave(&[(b"fmt ", &format_chunk(16)), (b"data", &data)])).unwrap();
    assert_eq!(
        stream.read(&mut [], &|| false),
        Err(MediaError::InvalidRequest)
    );
    let mut output = [0.0; 2];
    assert_eq!(stream.read(&mut output, &|| false).unwrap(), 1);
    assert_eq!(output[0], 0.5);
    assert_eq!(
        stream.read(&mut output, &|| false),
        Err(MediaError::InvalidOutput)
    );
}

#[test]
fn child_wait_helpers_retry_interrupts_and_map_other_errors() {
    let mut waits = 0;
    wait_until_reaped(&mut || {
        waits += 1;
        if waits < 3 {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    });
    assert_eq!(waits, 3);

    wait_until_reaped(&mut || Err(io::Error::from(io::ErrorKind::PermissionDenied)));

    let mut polls = 0;
    assert_eq!(
        poll_child(&mut || {
            polls += 1;
            if polls == 1 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(None)
            }
        })
        .unwrap(),
        None
    );
    assert_eq!(polls, 2);
    assert_eq!(
        poll_child(&mut || Err(io::Error::from(io::ErrorKind::PermissionDenied))),
        Err(MediaError::Io)
    );
}

#[test]
fn interrupted_pipe_reads_are_the_only_errors_retried() {
    assert!(super::retry_interrupted(&io::Error::from(
        io::ErrorKind::Interrupted
    )));
    assert!(!super::retry_interrupted(&io::Error::from(
        io::ErrorKind::WouldBlock
    )));
}

#[test]
fn child_read_retries_interrupted_calls_and_returns_the_next_result() {
    let mut calls = 0;
    assert_eq!(
        super::read_retry_interrupted(|| {
            calls += 1;
            if calls == 1 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(7)
            }
        })
        .unwrap(),
        7
    );
    assert_eq!(calls, 2);

    let mut calls = 0;
    assert_eq!(
        super::read_retry_interrupted(|| {
            calls += 1;
            Err::<usize, _>(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(calls, 1);
}

#[test]
#[cfg(target_os = "linux")]
fn normal_scheduler_request_uses_other_at_zero_priority_and_propagates_errors() {
    let mut request = None;
    assert!(
        set_normal_scheduler(|policy, parameters| {
            request = Some((policy, parameters.sched_priority));
            0
        })
        .is_ok()
    );
    assert_eq!(request, Some((libc::SCHED_OTHER, 0)));

    let error = set_normal_scheduler(|_, parameters| unsafe {
        libc::sched_setscheduler(0, -1, parameters)
    })
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    assert!(normal_scheduler().is_ok());
    assert_eq!(unsafe { libc::sched_getscheduler(0) }, libc::SCHED_OTHER);
}

#[test]
fn stream_yields_output_before_exit_and_reports_failure_after_drain() {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    let mut command = Command::new("sh");
    command.args(["-c", "printf first; sleep 1; printf last"]);
    let mut stream = ChildStream::spawn(&config, &mut command, None, &|| false).unwrap();
    let mut bytes = [0; 16];
    assert_eq!(stream.read(&mut bytes, &|| false).unwrap(), 5);
    assert_eq!(&bytes[..5], b"first");
    assert!(stream.try_wait().unwrap().is_none());
    assert_eq!(stream.read(&mut bytes, &|| false).unwrap(), 4);
    assert_eq!(&bytes[..4], b"last");
    assert_eq!(stream.read(&mut bytes, &|| false).unwrap(), 0);

    let mut command = Command::new("sh");
    command.args(["-c", "printf audio; exit 9"]);
    let mut stream = ChildStream::spawn(&config, &mut command, None, &|| false).unwrap();
    assert_eq!(stream.read(&mut bytes, &|| false).unwrap(), 5);
    assert_eq!(&bytes[..5], b"audio");
    assert_eq!(
        stream.read(&mut bytes, &|| false),
        Err(MediaError::ProcessFailed)
    );
}

#[test]
#[cfg(unix)]
fn eof_waits_for_a_child_that_closed_stdout_before_exiting() {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(2),
        child_reaper: None,
    };
    let mut command = Command::new("sh");
    command.args(["-c", "exec 1>&-; sleep 0.02; exit 0"]);
    let mut stream = ChildStream::spawn(&config, &mut command, None, &|| false).unwrap();

    assert_eq!(stream.read(&mut [0; 1], &|| false), Ok(0));
    assert!(stream.try_wait().unwrap().is_some());
}

#[test]
fn stream_cancellation_and_deadline_kill_and_reap_the_owned_child() {
    use std::sync::atomic::AtomicBool;

    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    let mut command = Command::new("sh");
    command.args(["-c", "exec sleep 30"]);
    let mut stream = ChildStream::spawn(&config, &mut command, None, &|| false).unwrap();
    let pid = stream.id();
    let cancelled = AtomicBool::new(true);
    let mut byte = [0];
    assert_eq!(
        stream.read(&mut byte, &|| cancelled.load(Ordering::Relaxed)),
        Err(MediaError::Cancelled)
    );
    drop(stream);
    assert!(!Path::new("/proc").join(pid.to_string()).exists());

    let timed = Config {
        process_timeout: Duration::from_millis(10),
        ..config
    };
    let mut command = Command::new("sh");
    command.args(["-c", "exec sleep 30"]);
    let mut stream = ChildStream::spawn(&timed, &mut command, None, &|| false).unwrap();
    let pid = stream.id();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(stream.read(&mut byte, &|| false), Err(MediaError::TimedOut));
    drop(stream);
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
}

#[test]
fn child_stream_handles_pre_cancelled_spawn_stdin_failure_and_cached_exit() {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    let mut command = Command::new("cat");
    assert!(matches!(
        ChildStream::spawn(&config, &mut command, None, &|| true),
        Err(MediaError::Cancelled)
    ));

    let mut command = Command::new("sh");
    command.args(["-c", "exec 0<&-; sleep 1"]);
    assert!(matches!(
        ChildStream::spawn(&config, &mut command, Some(&vec![0; 1_000_000]), &|| false),
        Err(MediaError::Io)
    ));

    let mut command = Command::new("true");
    let mut stream = ChildStream::spawn(&config, &mut command, None, &|| false).unwrap();
    for _ in 0..100 {
        if stream.try_wait().unwrap().is_some() {
            assert!(stream.try_wait().unwrap().is_some());
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("successful child did not exit");
}

#[test]
#[cfg(unix)]
fn child_stream_reports_closed_stdout_descriptor_errors() {
    use std::os::fd::AsRawFd;

    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    let mut command = Command::new("sh");
    command.args(["-c", "exec sleep 1"]);
    let mut stream = ChildStream::spawn(&config, &mut command, None, &|| false).unwrap();
    let stdout = stream.stdout.as_ref().unwrap();
    let target = stdout.as_raw_fd();
    let mut descriptors = [0; 2];
    // SAFETY: descriptors points to writable storage for the new pipe endpoints.
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    // SAFETY: replace the owned descriptor with a valid write-only pipe endpoint. ChildStdout
    // still owns target, which remains valid and is closed exactly once on drop.
    assert_eq!(unsafe { libc::dup2(descriptors[1], target) }, target);
    // SAFETY: these are the original, separately owned pipe descriptors.
    assert_eq!(unsafe { libc::close(descriptors[0]) }, 0);
    assert_eq!(unsafe { libc::close(descriptors[1]) }, 0);
    assert_eq!(stream.read(&mut [0_u8; 1], &|| false), Err(MediaError::Io));
}

#[test]
#[cfg(unix)]
fn nonblocking_setup_reports_an_invalid_descriptor() {
    assert!(set_nonblocking_fd(-1).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn nonblocking_setup_reports_a_failure_after_reading_descriptor_flags() {
    // O_PATH permits F_GETFL but not F_SETFL, covering the second fcntl failure.
    let descriptor = unsafe { libc::open(c"/proc/self".as_ptr(), libc::O_PATH) };
    assert!(descriptor >= 0);
    assert!(set_nonblocking_fd(descriptor).is_err());
    assert_eq!(unsafe { libc::close(descriptor) }, 0);
}

#[test]
fn read_reports_a_child_without_a_stdout_pipe() {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        process_timeout: Duration::from_secs(1),
        child_reaper: None,
    };
    let child = Command::new("true").stdout(Stdio::null()).spawn().unwrap();
    let mut stream = ChildStream {
        child: OwnedChild(child),
        stdout: None,
        _reaper: ReaperGuard(config.child_reaper),
        started: Instant::now(),
        timeout: config.process_timeout,
        status: None,
    };
    assert_eq!(stream.read(&mut [0], &|| false), Err(MediaError::Io));
}
