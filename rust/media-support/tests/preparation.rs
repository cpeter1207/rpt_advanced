use provider::Config;
use rpt_advanced_core::media::{Cancellation, MediaError};
#[cfg(file_adapter)]
use rpt_advanced_core::media::{FilePreparer, FileRequest};
#[cfg(speech_adapter)]
use rpt_advanced_core::media::{SpeechPreparer, SpeechRequest};
#[cfg(file_adapter)]
use rptadv_file_adapter as provider;
#[cfg(speech_adapter)]
use rptadv_speech_adapter as provider;
#[path = "support/consumer.rs"]
mod consumer;
use consumer::MediaAdapter;
use std::{fs, path::Path, time::Duration};

#[test]
#[cfg(file_adapter)]
fn decodes_opened_local_wave_at_source_rate_with_normalized_pcm() {
    let directory = std::env::temp_dir().join(format!("rptadv-media-test-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("source.wav");
    let mut wave = Vec::from(*b"RIFF\x5e\x11\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0data\x3a\x11\0\0");
    for _ in 0..2205 {
        wave.extend_from_slice(&1000_i16.to_le_bytes());
    }
    fs::write(&path, wave).unwrap();
    let adapter = MediaAdapter::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: Path::new("ffmpeg").into(),
        #[cfg(speech_adapter)]
        piper: Path::new("missing-piper").into(),
        temporary_directory: directory.clone(),
        process_timeout: Duration::from_secs(30),
        child_reaper: None,
    })
    .unwrap();
    let result = adapter.prepare_file(&FileRequest {
        path: &path,
        cancellation: &Cancellation::default(),
    });
    drop(adapter);
    fs::remove_file(path).unwrap();
    assert_eq!(
        fs::read_dir(&directory).unwrap().count(),
        0,
        "adapter leaked temporary resources"
    );
    fs::remove_dir(directory).unwrap();
    let audio = result.unwrap();
    assert_eq!(audio.sample_rate_hz(), 22050);
    assert_eq!(audio.samples().len(), 2205);
    assert!(audio.samples().iter().all(|sample| *sample == 0.030517578));
}

#[test]
#[cfg(speech_adapter)]
fn synthesizes_literal_text_with_speed_and_speech_only_gain() {
    let directory = std::env::temp_dir().join(format!("rptadv-speech-test-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let program = directory.join("fixture");
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
            .arg(&program)
            .status()
            .unwrap()
            .success()
    );
    let adapter = MediaAdapter::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: "must-not-run-ffmpeg-for-speech".into(),
        #[cfg(speech_adapter)]
        piper: program.clone(),
        temporary_directory: directory.clone(),
        process_timeout: Duration::from_secs(30),
        child_reaper: None,
    })
    .unwrap();
    for (speed, model) in [
        (1, "100.000000"),
        (50, "002.000000"),
        (100, "001.000000"),
        (1000, "000.100000"),
        (333, "000.300300"),
    ] {
        let audio = adapter
            .prepare_speech(&SpeechRequest {
                text: "Identifier; $(not a command)\n",
                model: Path::new(model),
                speed_percent: speed,
                level_db: -20,
                cancellation: &Cancellation::default(),
            })
            .unwrap();
        assert_eq!(audio.sample_rate_hz(), 22050);
        assert_eq!(audio.samples().len(), 2205);
        assert!((audio.samples()[1102] - 0.0030517578).abs() < 0.000000001);
    }
    fs::remove_file(program).unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}

#[test]
fn rejects_bad_settings_and_reports_individual_media_failures() {
    let adapter = MediaAdapter::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: "missing-ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "missing-piper".into(),
        temporary_directory: std::env::temp_dir(),
        process_timeout: Duration::from_secs(30),
        child_reaper: None,
    })
    .unwrap();
    let token = Cancellation::default();
    #[cfg(speech_adapter)]
    for (speed, level, text) in [
        (0, 0, "text"),
        (1001, 0, "text"),
        (100, -61, "text"),
        (100, 1, "text"),
        (100, 0, ""),
    ] {
        assert_eq!(
            adapter.prepare_speech(&SpeechRequest {
                text,
                model: Path::new("model"),
                speed_percent: speed,
                level_db: level,
                cancellation: &token
            }),
            Err(MediaError::InvalidRequest)
        );
    }
    #[cfg(speech_adapter)]
    assert_eq!(
        adapter.prepare_speech(&SpeechRequest {
            text: "text",
            model: Path::new(""),
            speed_percent: 100,
            level_db: 0,
            cancellation: &token
        }),
        Err(MediaError::InvalidRequest)
    );
    #[cfg(file_adapter)]
    assert_eq!(
        adapter.prepare_file(&FileRequest {
            path: Path::new("missing-file"),
            cancellation: &token
        }),
        Err(MediaError::Unavailable)
    );
    #[cfg(speech_adapter)]
    assert_eq!(
        adapter.prepare_speech(&SpeechRequest {
            text: "text",
            model: Path::new("model"),
            speed_percent: 100,
            level_db: 0,
            cancellation: &token
        }),
        Err(MediaError::Unavailable)
    );
    token.cancel();
    #[cfg(file_adapter)]
    assert_eq!(
        adapter.prepare_file(&FileRequest {
            path: Path::new("missing-file"),
            cancellation: &token
        }),
        Err(MediaError::Cancelled)
    );
    #[cfg(speech_adapter)]
    assert_eq!(
        adapter.prepare_speech(&SpeechRequest {
            text: "text",
            model: Path::new("model"),
            speed_percent: 100,
            level_db: 0,
            cancellation: &token
        }),
        Err(MediaError::Cancelled)
    );
}

#[test]
#[cfg(speech_adapter)]
fn failed_timed_out_and_cancelled_children_are_reaped_and_temporary_files_removed() {
    let directory =
        std::env::temp_dir().join(format!("rptadv-lifecycle-test-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let program = directory.join("fixture");
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
            .arg(&program)
            .status()
            .unwrap()
            .success()
    );
    let marker = directory.join("child-pid");
    for (model, expected) in [
        ("fail", MediaError::ProcessFailed),
        ("invalid", MediaError::ProcessFailed),
        ("empty", MediaError::ProcessFailed),
    ] {
        let adapter = MediaAdapter::new(Config {
            #[cfg(file_adapter)]
            ffmpeg: "ffmpeg".into(),
            #[cfg(speech_adapter)]
            piper: program.clone(),
            temporary_directory: directory.clone(),
            process_timeout: Duration::from_secs(5),
            child_reaper: None,
        })
        .unwrap();
        assert_eq!(
            adapter.prepare_speech(&SpeechRequest {
                text: "anything",
                model: Path::new(model),
                speed_percent: 50,
                level_db: 0,
                cancellation: &Cancellation::default()
            }),
            Err(expected)
        );
        if marker.exists() {
            assert_reaped(&marker);
            fs::remove_file(&marker).unwrap();
        }
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            1,
            "temporary output leaked after {model}"
        );
    }
    let waiting_model = format!("wait={}", marker.display());
    let adapter = MediaAdapter::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: program.clone(),
        temporary_directory: directory.clone(),
        process_timeout: Duration::from_millis(200),
        child_reaper: None,
    })
    .unwrap();
    assert_eq!(
        adapter.prepare_speech(&SpeechRequest {
            text: "anything",
            model: Path::new(&waiting_model),
            speed_percent: 50,
            level_db: 0,
            cancellation: &Cancellation::default()
        }),
        Err(MediaError::TimedOut)
    );
    assert_reaped(&marker);
    fs::remove_file(&marker).unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    let token = Cancellation::default();
    let cancellation = token.clone();
    let adapter = MediaAdapter::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: program.clone(),
        temporary_directory: directory.clone(),
        process_timeout: Duration::from_secs(30),
        child_reaper: None,
    })
    .unwrap();
    let worker = std::thread::spawn(move || {
        adapter.prepare_speech(&SpeechRequest {
            text: "anything",
            model: Path::new(&waiting_model),
            speed_percent: 50,
            level_db: 0,
            cancellation: &cancellation,
        })
    });
    let started = std::time::Instant::now();
    while !marker.exists() {
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    token.cancel();
    assert_eq!(worker.join().unwrap(), Err(MediaError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_reaped(&marker);
    fs::remove_file(marker).unwrap();
    fs::remove_file(program).unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}

#[cfg(speech_adapter)]
fn assert_reaped(marker: &Path) {
    let pid = fs::read_to_string(marker).unwrap();
    assert!(
        !Path::new("/proc").join(pid).exists(),
        "owned child was not reaped"
    );
}

#[test]
#[cfg(file_adapter)]
fn host_reaper_guards_spawn_failure_and_success_while_source_is_opened_once() {
    use std::sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    };
    static PATHS: OnceLock<(std::path::PathBuf, std::path::PathBuf)> = OnceLock::new();
    static ACQUIRED: AtomicUsize = AtomicUsize::new(0);
    static RELEASED: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn acquire() {
        if ACQUIRED.fetch_add(1, Ordering::SeqCst) == 0 {
            let (source, original) = PATHS.get().unwrap();
            fs::rename(source, original).unwrap();
            fs::write(source, b"replaced source must not be reopened").unwrap();
        }
    }
    extern "C" fn release() {
        RELEASED.fetch_add(1, Ordering::SeqCst);
    }
    let directory = std::env::temp_dir().join(format!("rptadv-reaper-test-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("source.wav");
    let original = directory.join("original.wav");
    let mut wave = Vec::from(*b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0data\x04\0\0\0");
    wave.extend_from_slice(&[0, 128, 255, 127]);
    fs::write(&source, wave).unwrap();
    PATHS.set((source.clone(), original.clone())).unwrap();
    let adapter = MediaAdapter::new(Config {
        #[cfg(file_adapter)]
        ffmpeg: "ffmpeg".into(),
        #[cfg(speech_adapter)]
        piper: "missing-piper".into(),
        temporary_directory: directory.clone(),
        process_timeout: Duration::from_secs(30),
        child_reaper: Some(provider::ChildReaper { acquire, release }),
    })
    .unwrap();
    let token = Cancellation::default();
    let audio = adapter
        .prepare_file(&FileRequest {
            path: &source,
            cancellation: &token,
        })
        .unwrap();
    assert_eq!(audio.samples(), &[-1.0, 0.9999695]);
    assert_eq!(ACQUIRED.load(Ordering::SeqCst), 1);
    assert_eq!(RELEASED.load(Ordering::SeqCst), 1);
    fs::remove_file(source).unwrap();
    fs::remove_file(original).unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}
