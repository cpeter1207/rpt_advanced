use super::*;

#[test]
#[cfg(file_adapter)]
fn fifo_source_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let directory = Temporary::new(&std::env::temp_dir()).unwrap();
    let path = directory.path("input");
    let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path_bytes is a live NUL-terminated pathname.
    assert_eq!(unsafe { libc::mkfifo(path_bytes.as_ptr(), 0o600) }, 0);
    let started = Instant::now();
    assert!(matches!(open_local(&path), Err(MediaError::Unavailable)));
    assert!(started.elapsed() < Duration::from_secs(1));
    fs::remove_file(path).unwrap();
}

#[test]
#[cfg(file_adapter)]
fn regular_file_source_is_opened() {
    let directory = Temporary::new(&std::env::temp_dir()).unwrap();
    let path = directory.path("input");
    fs::write(&path, b"audio").unwrap();
    assert!(open_local(&path).is_ok());
    fs::remove_file(path).unwrap();
}

#[test]
fn temporary_creation_retries_collisions_and_reports_exhaustion_and_io_errors() {
    let owner = Temporary::new(&std::env::temp_dir()).unwrap();

    let collision_parent = owner.path("collision-parent");
    fs::create_dir(&collision_parent).unwrap();
    fs::create_dir(temporary_path(&collision_parent, 0)).unwrap();
    let next = AtomicU64::new(0);
    let temporary = Temporary::new_with_counter(&collision_parent, &next).unwrap();
    assert_eq!(temporary.path, temporary_path(&collision_parent, 1));
    drop(temporary);
    fs::remove_dir(temporary_path(&collision_parent, 0)).unwrap();
    fs::remove_dir(&collision_parent).unwrap();

    let full_parent = owner.path("full-parent");
    fs::create_dir(&full_parent).unwrap();
    for sequence in 0..64 {
        fs::create_dir(temporary_path(&full_parent, sequence)).unwrap();
    }
    let next = AtomicU64::new(0);
    assert!(matches!(
        Temporary::new_with_counter(&full_parent, &next),
        Err(MediaError::Io)
    ));
    for sequence in 0..64 {
        fs::remove_dir(temporary_path(&full_parent, sequence)).unwrap();
    }
    fs::remove_dir(&full_parent).unwrap();

    let missing_parent = owner.path("missing-parent");
    let next = AtomicU64::new(0);
    assert!(matches!(
        Temporary::new_with_counter(&missing_parent, &next),
        Err(MediaError::Unavailable)
    ));

    let first = owner.create("duplicate").unwrap();
    assert!(matches!(owner.create("duplicate"), Err(MediaError::Io)));
    drop(first);
    fs::remove_file(owner.path("duplicate")).unwrap();
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
fn run_honors_cancellation_before_spawning() {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        temporary_directory: std::env::temp_dir(),
        process_timeout: Duration::from_secs(1),
        child_reaper: None,
    };
    assert_eq!(
        run(&config, &mut Command::new("must-not-run"), &|| true),
        Err(MediaError::Cancelled)
    );
}

#[test]
fn run_reports_success_and_process_failure() {
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        temporary_directory: std::env::temp_dir(),
        process_timeout: Duration::from_secs(1),
        child_reaper: None,
    };
    assert_eq!(run(&config, &mut Command::new("true"), &|| false), Ok(()));
    assert_eq!(
        run(&config, &mut Command::new("false"), &|| false),
        Err(MediaError::ProcessFailed)
    );

    extern "C" fn no_op() {}
    let guarded = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        temporary_directory: std::env::temp_dir(),
        process_timeout: Duration::from_secs(1),
        child_reaper: Some(crate::ChildReaper {
            acquire: no_op,
            release: no_op,
        }),
    };
    assert_eq!(run(&guarded, &mut Command::new("true"), &|| false), Ok(()));

    let timed_out = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        temporary_directory: std::env::temp_dir(),
        process_timeout: Duration::ZERO,
        child_reaper: None,
    };
    assert_eq!(
        run(&timed_out, &mut Command::new("true"), &|| false),
        Err(MediaError::TimedOut)
    );

    let checks = AtomicU64::new(0);
    assert_eq!(
        run(&config, &mut Command::new("sleep"), &|| {
            checks.fetch_add(1, Ordering::Relaxed) != 0
        }),
        Err(MediaError::Cancelled)
    );
}

#[test]
#[cfg(file_adapter)]
fn decode_preserves_file_level_and_reports_missing_decoder() {
    use std::io::Write;

    let parent = Temporary::new(&std::env::temp_dir()).unwrap();
    let source = parent.path("text");
    let mut input = parent.create("text").unwrap();
    let mut wave = Vec::from(
        *b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0data\x04\0\0\0",
    );
    wave.extend_from_slice(&[0, 128, 255, 127]);
    input.write_all(&wave).unwrap();
    drop(input);
    let config = Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        temporary_directory: parent.path.clone(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    let audio = decode(&config, File::open(&source).unwrap(), &|| false).unwrap();
    assert_eq!(audio.sample_rate_hz(), 22_050);
    assert_eq!(audio.samples(), &[-1.0, 0.9999695]);
    let unavailable = Config {
        #[cfg(file_adapter)]
        ffmpeg: "/definitely/missing/ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "piper".into(),
        temporary_directory: parent.path.clone(),
        process_timeout: Duration::from_secs(5),
        child_reaper: None,
    };
    assert_eq!(
        decode(&unavailable, File::open(source).unwrap(), &|| false),
        Err(MediaError::Unavailable)
    );
}
