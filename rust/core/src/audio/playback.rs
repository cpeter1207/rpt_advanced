//! Nonblocking playback from producer-owned native PCM streams.

use super::{AudioSource, MorseSwitch, PcmRead, PcmStreamReader};

/// Result of one playback attempt; only success satisfies recurring-message deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackOutcome {
    /// Playback is producing or waiting for its selected stream.
    Active,
    /// The complete selected stream drained successfully.
    Succeeded,
    /// The stream failed, possibly after partial output.
    Failed,
    /// Playback was interrupted without a usable replacement, or retired.
    Cancelled,
}

/// One scheduled item consumed only from the telemetry producer's PCM ring.
pub struct Playback {
    stream: Option<Box<dyn PcmStreamReader>>,
    stream_started: bool,
    morse: Option<MorseSwitch>,
    waiting_for_stream: bool,
    output_started: bool,
    outcome: PlaybackOutcome,
}

impl Playback {
    /// Construct playback from a nonblocking producer-backed PCM stream.
    pub fn new_stream(stream: Box<dyn PcmStreamReader>) -> Self {
        Self {
            stream: Some(stream),
            stream_started: false,
            morse: None,
            waiting_for_stream: false,
            output_started: false,
            outcome: PlaybackOutcome::Active,
        }
    }

    /// Read available PCM, returning only samples actually produced.
    pub fn render(&mut self, receiving: bool, output: &mut [f32]) -> usize {
        if self.is_finished() {
            return 0;
        }
        // Start before selecting Morse: start creates a fresh producer generation.
        if !self.stream_started && (receiving || !output.is_empty()) {
            if let Some(stream) = &mut self.stream {
                stream.start();
            }
            self.stream_started = true;
        }
        if receiving && self.morse.is_none() {
            self.morse = Some(MorseSwitch::Pending);
        }
        if self.morse == Some(MorseSwitch::Pending) {
            let selected = self
                .stream
                .as_mut()
                .map_or(MorseSwitch::Unavailable, |stream| {
                    stream.select_morse_fallback()
                });
            self.morse = Some(selected);
            match selected {
                MorseSwitch::Selected => {}
                MorseSwitch::Pending => {
                    self.waiting_for_stream = true;
                    return 0;
                }
                MorseSwitch::Unavailable => {
                    self.finish(PlaybackOutcome::Cancelled);
                    return 0;
                }
            }
        }
        if output.is_empty() {
            return 0;
        }
        match self
            .stream
            .as_mut()
            .map_or(PcmRead::Finished, |stream| stream.render(output))
        {
            PcmRead::Samples(count) => {
                self.output_started |= count != 0;
                self.waiting_for_stream = false;
                count.min(output.len())
            }
            PcmRead::FinalSamples(count) => {
                self.output_started |= count != 0;
                // The samples are already copied; release the producer without
                // requiring another read of this now-terminal playback item.
                self.finish(PlaybackOutcome::Succeeded);
                count.min(output.len())
            }
            PcmRead::Pending => {
                self.waiting_for_stream = true;
                0
            }
            PcmRead::Finished => {
                self.finish(PlaybackOutcome::Succeeded);
                0
            }
            PcmRead::Failed => {
                self.finish(PlaybackOutcome::Failed);
                0
            }
        }
    }

    /// Restart a producer-backed item without allocating on the audio path.
    pub fn restart(&mut self, receiving: bool) {
        self.outcome = PlaybackOutcome::Active;
        self.waiting_for_stream = false;
        self.output_started = false;
        self.morse = None;
        self.stream_started = false;
        if receiving {
            self.render(true, &mut []);
        }
    }

    fn finish(&mut self, outcome: PlaybackOutcome) {
        if let Some(stream) = &mut self.stream {
            stream.cancel();
        }
        self.waiting_for_stream = false;
        self.outcome = outcome;
    }

    /// Retire an unstarted opportunity without claiming successful completion.
    pub(crate) fn cancel(&mut self) {
        self.finish(PlaybackOutcome::Cancelled);
    }

    /// Restart a preempted, already-admitted item with the same values and Morse latch.
    pub(crate) fn rewind(&mut self) {
        self.stream
            .as_mut()
            .expect("admitted playback retains its stream until retirement")
            .rewind();
        self.stream_started = true;
        self.waiting_for_stream = true;
        self.outcome = PlaybackOutcome::Active;
        if self.morse.is_some() {
            self.morse = Some(MorseSwitch::Pending);
        }
    }

    /// Hold an ineligible item without keeping a stale, never-played value snapshot.
    /// Begun playback retains its attempt and receive-driven Morse latch.
    pub fn defer(&mut self, receiving: bool) {
        if !self.output_started
            && self
                .stream
                .as_mut()
                .is_some_and(|stream| stream.defer_before_output())
        {
            self.stream_started = false;
            self.waiting_for_stream = true;
            self.morse = (receiving || self.morse.is_some()).then_some(MorseSwitch::Pending);
        } else {
            self.render(receiving, &mut []);
        }
    }

    /// Return the reader for retirement by its non-audio owner.
    pub fn take_stream(&mut self) -> Option<Box<dyn PcmStreamReader>> {
        self.outcome = PlaybackOutcome::Cancelled;
        self.stream_started = false;
        self.stream.take()
    }

    /// Whether playback is waiting for the station producer to supply PCM.
    #[must_use]
    pub const fn waiting_for_stream(&self) -> bool {
        self.waiting_for_stream
    }

    /// Whether this attempt has emitted PCM; pending successors retain the same attempt.
    #[must_use]
    pub const fn has_output(&self) -> bool {
        self.output_started
    }

    /// Whether the selected source is an active producer-backed stream.
    #[must_use]
    pub const fn is_streaming(&self) -> bool {
        !self.is_finished() && self.stream.is_some()
    }

    /// Whether the current pass has reached terminal completion.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        !matches!(self.outcome, PlaybackOutcome::Active)
    }

    /// Distinguish successful completion from failure or cancellation.
    #[must_use]
    pub const fn outcome(&self) -> PlaybackOutcome {
        self.outcome
    }
}

impl AudioSource for Playback {
    fn render(&mut self, output: &mut [f32]) -> usize {
        self.render(false, output)
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
