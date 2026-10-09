//! Nonblocking playback from producer-owned native PCM streams.

use super::{AudioSource, PcmRead, PcmStreamReader};

/// One scheduled item consumed only from the telemetry producer's PCM ring.
pub struct Playback {
    stream: Option<Box<dyn PcmStreamReader>>,
    stream_started: bool,
    stream_fallback: bool,
    waiting_for_stream: bool,
    finished: bool,
}

impl Playback {
    /// Construct playback from a nonblocking producer-backed PCM stream.
    pub fn new_stream(stream: Box<dyn PcmStreamReader>) -> Self {
        Self {
            stream: Some(stream),
            stream_started: false,
            stream_fallback: false,
            waiting_for_stream: false,
            finished: false,
        }
    }

    /// Read available PCM, returning only samples actually produced.
    pub fn render(&mut self, receiving: bool, output: &mut [f32]) -> usize {
        if self.finished {
            return 0;
        }
        if receiving && !self.stream_fallback {
            self.stream_fallback = self
                .stream
                .as_mut()
                .is_some_and(|stream| stream.select_morse_fallback());
            if !self.stream_fallback {
                if let Some(stream) = &mut self.stream {
                    stream.cancel();
                }
                self.finished = true;
                self.waiting_for_stream = false;
                return 0;
            }
        }
        if output.is_empty() {
            return 0;
        }
        if !self.stream_started {
            if let Some(stream) = &mut self.stream {
                stream.start();
            }
            self.stream_started = true;
        }
        match self
            .stream
            .as_mut()
            .map_or(PcmRead::Finished, |stream| stream.render(output))
        {
            PcmRead::Samples(count) => {
                self.waiting_for_stream = false;
                count.min(output.len())
            }
            PcmRead::FinalSamples(count) => {
                self.waiting_for_stream = false;
                self.finished = true;
                count.min(output.len())
            }
            PcmRead::Pending => {
                self.waiting_for_stream = true;
                0
            }
            PcmRead::Finished | PcmRead::Failed => {
                if let Some(stream) = &mut self.stream {
                    stream.cancel();
                }
                self.waiting_for_stream = false;
                self.finished = true;
                0
            }
        }
    }

    /// Restart a producer-backed item without allocating on the audio path.
    pub fn restart(&mut self, receiving: bool) {
        self.finished = false;
        self.waiting_for_stream = false;
        self.stream_fallback = false;
        self.stream_started = false;
        if receiving
            && !self
                .stream
                .as_mut()
                .is_some_and(|stream| stream.select_morse_fallback())
        {
            self.finished = true;
        } else if receiving {
            self.stream_fallback = true;
        }
    }

    /// Return the reader for retirement by its non-audio owner.
    pub fn take_stream(&mut self) -> Option<Box<dyn PcmStreamReader>> {
        self.finished = true;
        self.stream_started = false;
        self.stream.take()
    }

    /// Whether playback is waiting for the station producer to supply PCM.
    #[must_use]
    pub const fn waiting_for_stream(&self) -> bool {
        self.waiting_for_stream
    }

    /// Whether the selected source is an active producer-backed stream.
    #[must_use]
    pub const fn is_streaming(&self) -> bool {
        !self.finished && self.stream.is_some()
    }

    /// Whether the current pass has reached terminal completion.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
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
