#![deny(warnings, missing_docs)]
//! Private source shared by independently built file and speech preparation adapters.
use result::PreparedAudio;
use std::path::PathBuf;
use std::time::Duration;
pub mod abi;
mod process;
mod provider;
mod result;
mod wave;
#[cfg(file_adapter)]
pub use provider::rptadv_file_adapter_descriptor;
#[cfg(speech_adapter)]
pub use provider::rptadv_speech_adapter_descriptor;
pub use result::MediaError;

/// Local subprocess and temporary-file configuration.
pub struct Config {
    /// FFmpeg executable, resolved through PATH when not absolute.
    #[cfg(file_adapter)]
    pub ffmpeg: PathBuf,
    /// Piper executable, resolved through PATH when not absolute.
    #[cfg(speech_adapter)]
    pub piper: PathBuf,
    /// Existing temporary directory owned by the service user.
    pub temporary_directory: PathBuf,
    /// Monotonic time budget for each subprocess, normally 30 seconds.
    pub process_timeout: Duration,
    /// Host SIGCHLD coordination, required when the host has its own child reaper.
    pub child_reaper: Option<ChildReaper>,
}

/// Host-owned child-reaper exclusion around spawn through final reap.
#[derive(Clone, Copy)]
pub struct ChildReaper {
    /// Suspend competing child reaping before spawn; must be concurrency-safe.
    pub acquire: extern "C" fn(),
    /// Restore host reaping after cleanup, including spawn failure.
    pub release: extern "C" fn(),
}

struct Preparation {
    config: Config,
}

impl Preparation {
    fn new(config: Config) -> Self {
        Self { config }
    }
    #[cfg(file_adapter)]
    fn prepare_file(
        &self,
        path: &std::path::Path,
        cancelled: &impl Fn() -> bool,
    ) -> Result<PreparedAudio, MediaError> {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let input = process::open_local(path)?;
        process::decode(&self.config, input, cancelled)
    }
    #[cfg(speech_adapter)]
    fn prepare_speech(
        &self,
        text: &str,
        model: &std::path::Path,
        speed_percent: u32,
        level_db: i32,
        cancelled: &impl Fn() -> bool,
    ) -> Result<PreparedAudio, MediaError> {
        use std::io::{Seek, Write};
        if text.is_empty() || !(1..=1000).contains(&speed_percent) || !(-60..=0).contains(&level_db)
        {
            return Err(MediaError::InvalidRequest);
        }
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let temporary = process::Temporary::new(&self.config.temporary_directory)?;
        let mut input = temporary.create("text")?;
        input
            .write_all(text.as_bytes())
            .map_err(process::io_error)?;
        input.rewind().map_err(process::io_error)?;
        drop(temporary.create("speech.wav")?);
        let scale = 100_000_000 / speed_percent;
        let length = format!("{:03}.{:06}", scale / 1_000_000, scale % 1_000_000);
        let mut command = std::process::Command::new(&self.config.piper);
        command
            .arg("--model")
            .arg(model)
            .arg("--output_file")
            .arg(temporary.path("speech.wav"))
            .arg("--length_scale")
            .arg(length)
            .stdin(std::process::Stdio::from(input));
        process::run(&self.config, &mut command, cancelled)?;
        let bytes = std::fs::read(temporary.path("speech.wav")).map_err(process::io_error)?;
        // Preserve the previous decoder-stage status for malformed/empty Piper output.
        let wave = wave::parse(&bytes).map_err(|_| MediaError::ProcessFailed)?;
        let gain = 10_f32.powf(level_db as f32 / 20.0);
        PreparedAudio::new(
            wave.sample_rate_hz(),
            wave.samples().iter().map(|sample| sample * gain).collect(),
        )
    }
}
