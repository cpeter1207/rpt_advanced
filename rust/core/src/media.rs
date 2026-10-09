//! Offline media preparation port. Selection and fallback belong to the controller.

use crate::audio::PcmStreamReader;
use std::path::{Path, PathBuf};
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

/// Owned source chain queued for off-callback station rendering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaSource {
    /// Optional file attempted before speech.
    pub file: Option<PathBuf>,
    /// Optional speech fallback after file-open or pre-output failure.
    pub speech: Option<SpeechSource>,
    /// Gain applied only to decoded file/speech output before generated fallbacks.
    pub provider_gain_db: i8,
    /// Optional tone source after file/speech failure, before Morse fallback.
    pub tone: Option<ToneSource>,
    /// Optional Morse source after all higher-priority sources fail.
    pub morse: Option<MorseSource>,
}

/// Generated tone sequence rendered by the station-telemetry producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToneSource {
    /// Validated tone-sequence text.
    pub sequence: String,
    /// Tone level relative to full scale.
    pub level_db: i8,
}

/// Morse message rendered by the station-telemetry producer.
#[derive(Clone, Debug, PartialEq)]
pub struct MorseSource {
    /// Text encoded as Morse.
    pub text: String,
    /// Sending speed in words per minute.
    pub speed_wpm: u32,
    /// Sidetone frequency in Hz.
    pub frequency_hz: f32,
    /// Morse level relative to full scale.
    pub level_db: i8,
}

/// Owned Piper-compatible speech request kept off audio callbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechSource {
    /// Literal text.
    pub text: String,
    /// Local speech model path.
    pub model: PathBuf,
    /// Speed in percent.
    pub speed_percent: u32,
    /// Speech gain in dB.
    pub level_db: i32,
}

/// Generation-owned builder/control for one serialized station media worker.
pub trait StationMediaSession: Send {
    /// Register an immutable source chain and return its callback-safe reader.
    fn register(&mut self, source: MediaSource) -> Result<Box<dyn PcmStreamReader>, MediaError>;

    /// Register one-shot telemetry generated after the station worker starts.
    fn register_once(
        &mut self,
        source: MediaSource,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        self.register(source)
    }

    /// Stream already validated PCM through the same station ring used for decoded media.
    fn register_prepared(
        &mut self,
        _audio: PreparedAudio,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        Err(MediaError::IncompatibleAdapter)
    }

    /// Start the single worker after all source chains are registered.
    fn start(&mut self) -> Result<(), MediaError>;
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
    /// The bounded station-producer request queue has no available slot.
    QueueFull,
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
