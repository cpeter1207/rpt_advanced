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
