//! Bounded, allocation-free real-time audio primitives.

mod dtmf;
mod link_queue;
mod morse;
mod playback;
mod tone;

pub use dtmf::{DtmfDetector, DtmfDigit};
pub use link_queue::{LinkAudioConsumer, LinkAudioProducer, LinkAudioQueue, LinkQueueError};
pub use morse::{MorseError, MorseRenderer};
pub use playback::Playback;
pub use tone::{ToneError, ToneSequence};

/// A bounded source that renders normalized native PCM into caller-owned storage.
pub trait AudioSource {
    /// Render samples into `output`, returning the number actually produced.
    fn render(&mut self, output: &mut [f32]) -> usize;
}

/// One nonblocking read from a producer-backed native PCM stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmRead {
    /// `count` samples were written to the output prefix.
    Samples(usize),
    /// `count` samples were written and the producer has no further output.
    FinalSamples(usize),
    /// No samples are ready yet; the producer is still active.
    Pending,
    /// The producer completed and all ring samples have been drained.
    Finished,
    /// The producer failed before yielding playable PCM; no transmit-worker fallback exists.
    Failed,
}

/// Reads already-converted native PCM without waiting or allocating.
pub trait PcmStreamReader: Send {
    /// Start one playback pass without waiting or allocating.
    fn start(&mut self) {}

    /// Copy currently available samples into caller-owned storage.
    fn render(&mut self, output: &mut [f32]) -> PcmRead;

    /// Replace the active streamed source with its producer-rendered Morse fallback.
    ///
    /// Returns false when this reader has no producer-owned fallback.
    fn select_morse_fallback(&mut self) -> bool {
        false
    }

    /// Request producer cancellation without waiting; the worker owns cleanup.
    fn cancel(&mut self) {}
}
