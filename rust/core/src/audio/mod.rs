//! Bounded, allocation-free real-time audio primitives.

mod dtmf;
mod link_queue;
mod morse;
mod playback;
mod tone;

pub use dtmf::{DtmfDetector, DtmfError, DtmfGenerator};
pub use link_queue::{
    AudioSpan, LinkAudioConsumer, LinkAudioProducer, LinkAudioQueue, LinkQueueError, ReadSpan,
};
pub use morse::{MorseError, MorseRenderer};
pub use playback::{Playback, PlaybackOutcome};
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
    /// The producer or ring failed; any earlier samples do not constitute successful completion.
    Failed,
}

/// Nonblocking selection of a producer-rendered Morse replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorseSwitch {
    /// The replacement generation is selected; its PCM may still be pending.
    Selected,
    /// Primary output is gated while its endpoint waits for off-audio retirement.
    Pending,
    /// This item has no usable Morse fallback.
    Unavailable,
}

/// Reads already-converted native PCM without waiting or allocating.
pub trait PcmStreamReader: Send {
    /// Start one playback pass without waiting or allocating.
    fn start(&mut self) {}

    /// Rewind an admitted attempt without refreshing its values or destroying its producer.
    fn rewind(&mut self) {
        self.start();
    }

    /// Invalidate values prepared before any PCM was played, if this reader resolves them.
    /// Returns true when playback must start a fresh attempt after the deferral.
    /// A deferred reader is not read again until that fresh start.
    /// Static media keeps its prepared source and needs no new value snapshot.
    fn defer_before_output(&mut self) -> bool {
        false
    }

    /// Copy currently available samples into caller-owned storage.
    fn render(&mut self, output: &mut [f32]) -> PcmRead;

    /// Replace the active streamed source with its producer-rendered Morse fallback.
    ///
    /// Pending selection must be retried without reading any primary samples.
    fn select_morse_fallback(&mut self) -> MorseSwitch {
        MorseSwitch::Unavailable
    }

    /// Request producer cancellation without waiting; the worker owns cleanup.
    fn cancel(&mut self) {}
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
