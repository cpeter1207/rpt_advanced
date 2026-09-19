//! Bounded, allocation-free real-time audio primitives.

mod dtmf;
mod link_queue;
mod morse;
mod playback;
mod tone;

pub use dtmf::{DtmfDetector, DtmfDigit};
pub use link_queue::{LinkAudioConsumer, LinkAudioProducer, LinkAudioQueue, LinkQueueError};
pub use morse::{MorseError, MorseRenderer};
pub use playback::{Playback, PlaybackError};
pub use tone::{ToneError, ToneSequence};

/// A bounded source that renders normalized native PCM into caller-owned storage.
pub trait AudioSource {
    /// Render samples into `output`, returning the number actually produced.
    fn render(&mut self, output: &mut [f32]) -> usize;
}
