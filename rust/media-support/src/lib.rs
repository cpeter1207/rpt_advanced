#![deny(warnings, missing_docs)]
//! Private source shared by independently built streaming file and speech adapters.
use std::path::PathBuf;
use std::time::Duration;
pub mod abi;
mod process;
mod provider;
mod result;
#[cfg(file_adapter)]
pub use provider::rptadv_file_adapter_descriptor;
#[cfg(speech_adapter)]
pub use provider::rptadv_speech_adapter_descriptor;
pub use result::MediaError;

/// Local subprocess configuration for incremental PCM streams.
pub struct Config {
    /// FFmpeg executable, resolved through PATH when not absolute.
    #[cfg(file_adapter)]
    pub ffmpeg: PathBuf,
    /// Piper executable, resolved through PATH when not absolute.
    #[cfg(speech_adapter)]
    pub piper: PathBuf,
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
    fn open_file(
        &self,
        path: &std::path::Path,
        cancelled: &impl Fn() -> bool,
    ) -> Result<process::PcmStream, MediaError> {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let input = process::open_local(path)?;
        let mut command = std::process::Command::new(&self.config.ffmpeg);
        command.args([
            "-nostdin",
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
            "pipe:0",
            "-map",
            "0:a:0",
            "-vn",
            "-ac",
            "1",
            "-c:a",
            "pcm_f32le",
            "-f",
            "wav",
            "pipe:1",
        ]);
        command.stdin(std::process::Stdio::from(input));
        let child = process::ChildStream::spawn(&self.config, &mut command, None, cancelled)?;
        process::PcmStream::f32_wave(child, cancelled)
    }
    #[cfg(speech_adapter)]
    fn open_speech(
        &self,
        text: &str,
        model: &std::path::Path,
        speed_percent: u32,
        level_db: i32,
        cancelled: &impl Fn() -> bool,
    ) -> Result<process::PcmStream, MediaError> {
        if text.is_empty() || !(1..=1000).contains(&speed_percent) || !(-60..=0).contains(&level_db)
        {
            return Err(MediaError::InvalidRequest);
        }
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let model_config = model_config_path(model);
        let bytes = std::fs::read(model_config).map_err(process::io_error)?;
        let config: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| MediaError::ProcessFailed)?;
        let sample_rate_hz = config
            .get("audio")
            .and_then(|audio| audio.get("sample_rate"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|rate| u32::try_from(rate).ok())
            .filter(|rate| (8_000..=192_000).contains(rate))
            .ok_or(MediaError::InvalidOutput)?;
        let scale = 100_000_000 / speed_percent;
        let length = format!("{:03}.{:06}", scale / 1_000_000, scale % 1_000_000);
        let mut command = std::process::Command::new(&self.config.piper);
        command
            .arg("--model")
            .arg(model)
            .arg("--output_raw")
            .arg("--length_scale")
            .arg(length);
        let gain = 10_f32.powf(level_db as f32 / 20.0);
        let child = process::ChildStream::spawn(
            &self.config,
            &mut command,
            Some(text.as_bytes()),
            cancelled,
        )?;
        Ok(process::PcmStream::s16_raw(child, sample_rate_hz, gain))
    }
}

#[cfg(speech_adapter)]
fn model_config_path(model: &std::path::Path) -> PathBuf {
    let mut path = model.as_os_str().to_os_string();
    path.push(".json");
    PathBuf::from(path)
}
