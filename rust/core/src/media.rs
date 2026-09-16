//! Offline media preparation port. Selection and fallback belong to the controller.

use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Shared cancellation for one preparation; a cancelled token is never reset.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Request cancellation from another control-plane owner.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    /// Whether preparation should stop.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Local source to decode once, preserving its source sample rate.
pub struct FileRequest<'a> {
    /// Configured local source path.
    pub path: &'a Path,
    /// Cancellation scoped to this preparation.
    pub cancellation: &'a Cancellation,
}

/// Literal text and a local voice model for offline synthesis.
pub struct SpeechRequest<'a> {
    /// Text delivered literally through the synthesizer's stdin.
    pub text: &'a str,
    /// Existing local model; preparation never downloads one.
    pub model: &'a Path,
    /// Speaking speed from 1 through 1000 percent.
    pub speed_percent: u32,
    /// Speech-only gain from -60 through 0 dB.
    pub level_db: i32,
    /// Cancellation scoped to this preparation.
    pub cancellation: &'a Cancellation,
}

/// Immutable mono normalized F32 PCM at its decoded source rate.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedAudio {
    rate: u32,
    samples: Arc<[f32]>,
}

impl PreparedAudio {
    /// Reject empty, non-finite, or rate-less output before making it playable.
    pub fn new(rate: u32, samples: Vec<f32>) -> Result<Self, MediaError> {
        if rate == 0 || samples.is_empty() || samples.iter().any(|sample| !sample.is_finite()) {
            return Err(MediaError::InvalidOutput);
        }
        Ok(Self {
            rate,
            samples: samples.into(),
        })
    }
    /// Source rate; the telemetry ring alone converts it to the native rate.
    pub fn sample_rate_hz(&self) -> u32 {
        self.rate
    }
    /// PCM retained until every playback owner releases its clone.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// Preparation failure. The controller decides whether another source is appropriate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaError {
    /// Input or synthesis settings are invalid.
    InvalidRequest,
    /// An individual local file, model, or executable is unavailable.
    Unavailable,
    /// File or process I/O failed.
    Io,
    /// An external process exited unsuccessfully.
    ProcessFailed,
    /// A subprocess exceeded its own monotonic deadline.
    TimedOut,
    /// The request was explicitly cancelled.
    Cancelled,
    /// Decoded data was empty, malformed, or non-finite.
    InvalidOutput,
    /// Required adapter descriptor failed composition validation.
    IncompatibleAdapter,
}

/// Independently replaceable control-plane local-file decoding capability.
pub trait FilePreparer {
    /// Decode a local file with no fallback or native-rate conversion.
    fn prepare_file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError>;
}

/// Independently replaceable control-plane literal speech synthesis capability.
pub trait SpeechPreparer {
    /// Synthesize literal text, applying gain only to this speech.
    fn prepare_speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError>;
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
