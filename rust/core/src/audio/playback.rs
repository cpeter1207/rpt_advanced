//! Scheduled playback with direct native-rate tones and a Morse fallback.

use super::{AudioSource, MorseRenderer, PcmRead, PcmStreamReader, ToneSequence};

/// An invalid playback fallback configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackError;

/// One scheduled item with prepared PCM, an optional generated tone, and Morse fallback.
pub struct Playback {
    prepared: Vec<f32>,
    stream: Option<Box<dyn PcmStreamReader>>,
    tone: Option<ToneSequence>,
    using_prepared: bool,
    using_stream: bool,
    stream_started: bool,
    waiting_for_stream: bool,
    using_tone: bool,
    offset: usize,
    morse: MorseRenderer,
    finished: bool,
}

impl Playback {
    /// Validate and construct fallback Morse, selecting PCM only when not receiving.
    pub fn new(
        prepared: Option<Vec<f32>>,
        morse_text: &str,
        morse_speed_wpm: u32,
        morse_frequency_hz: f32,
        morse_level_db: i8,
        receiving: bool,
    ) -> Result<Self, PlaybackError> {
        Self::new_with_tone(
            prepared,
            None,
            morse_text,
            morse_speed_wpm,
            morse_frequency_hz,
            morse_level_db,
            receiving,
        )
    }

    /// Validate and construct playback with a directly rendered native-rate tone source.
    pub(crate) fn new_with_tone(
        prepared: Option<Vec<f32>>,
        tone: Option<ToneSequence>,
        morse_text: &str,
        morse_speed_wpm: u32,
        morse_frequency_hz: f32,
        morse_level_db: i8,
        receiving: bool,
    ) -> Result<Self, PlaybackError> {
        let morse = MorseRenderer::new(
            morse_text,
            morse_speed_wpm,
            morse_frequency_hz,
            morse_level_db,
        )
        .map_err(|_| PlaybackError)?;
        Self::new_with_sources(prepared, None, tone, morse, receiving)
    }

    /// Construct playback from a nonblocking stream and a ready Morse fallback.
    pub fn new_stream(
        stream: Box<dyn PcmStreamReader>,
        morse_text: &str,
        morse_speed_wpm: u32,
        morse_frequency_hz: f32,
        morse_level_db: i8,
        receiving: bool,
    ) -> Result<Self, PlaybackError> {
        Self::new_stream_with_tone(
            stream,
            None,
            morse_text,
            morse_speed_wpm,
            morse_frequency_hz,
            morse_level_db,
            receiving,
        )
    }

    /// Construct streamed playback with a generated tone before its Morse fallback.
    pub(crate) fn new_stream_with_tone(
        stream: Box<dyn PcmStreamReader>,
        tone: Option<ToneSequence>,
        morse_text: &str,
        morse_speed_wpm: u32,
        morse_frequency_hz: f32,
        morse_level_db: i8,
        receiving: bool,
    ) -> Result<Self, PlaybackError> {
        let morse = MorseRenderer::new(
            morse_text,
            morse_speed_wpm,
            morse_frequency_hz,
            morse_level_db,
        )
        .map_err(|_| PlaybackError)?;
        Self::new_with_sources(None, Some(stream), tone, morse, receiving)
    }

    fn new_with_sources(
        prepared: Option<Vec<f32>>,
        stream: Option<Box<dyn PcmStreamReader>>,
        tone: Option<ToneSequence>,
        morse: MorseRenderer,
        receiving: bool,
    ) -> Result<Self, PlaybackError> {
        let prepared = prepared
            .filter(|audio| !audio.is_empty())
            .unwrap_or_default();
        let using_prepared = !prepared.is_empty() && !receiving;
        let using_stream = !using_prepared && stream.is_some() && !receiving;
        let using_tone = !using_prepared && !using_stream && tone.is_some() && !receiving;
        Ok(Self {
            using_prepared,
            using_stream,
            stream_started: false,
            waiting_for_stream: false,
            using_tone,
            prepared,
            stream,
            tone,
            offset: 0,
            morse,
            finished: false,
        })
    }

    /// Render scheduled PCM or its fallback, returning only samples actually produced.
    pub fn render(&mut self, receiving: bool, output: &mut [f32]) -> usize {
        if self.finished {
            return 0;
        }
        if receiving {
            if self.using_stream {
                if let Some(stream) = &mut self.stream {
                    stream.cancel();
                }
            }
            self.using_prepared = false;
            self.using_stream = false;
            self.stream_started = false;
            self.using_tone = false;
            self.waiting_for_stream = false;
        }
        if output.is_empty() {
            return 0;
        }
        if self.using_prepared {
            let prepared = &self.prepared;
            let count = output.len().min(prepared.len() - self.offset);
            output[..count].copy_from_slice(&prepared[self.offset..self.offset + count]);
            self.offset += count;
            self.finished = self.offset == prepared.len();
            count
        } else if self.using_stream {
            if !self.stream_started {
                if let Some(stream) = &mut self.stream {
                    stream.start();
                }
                self.stream_started = true;
            }
            match self.stream.as_mut().map(|stream| stream.render(output)) {
                Some(PcmRead::Samples(count)) => {
                    self.waiting_for_stream = false;
                    count.min(output.len())
                }
                Some(PcmRead::Pending) => {
                    self.waiting_for_stream = true;
                    0
                }
                Some(PcmRead::Failed) => {
                    self.using_stream = false;
                    self.stream_started = false;
                    self.waiting_for_stream = false;
                    self.using_tone = self.tone.is_some();
                    if self.using_tone {
                        self.render_tone(output)
                    } else {
                        let count = self.morse.render(output);
                        self.finished = count < output.len();
                        count
                    }
                }
                Some(PcmRead::Finished) | None => {
                    self.using_stream = false;
                    self.waiting_for_stream = false;
                    self.finished = true;
                    0
                }
            }
        } else if self.using_tone {
            self.render_tone(output)
        } else {
            self.render_fallback(output)
        }
    }

    fn render_tone(&mut self, output: &mut [f32]) -> usize {
        let tone = self
            .tone
            .as_mut()
            .expect("tone playback requires a configured tone");
        let count = tone.render(output);
        self.finished = tone.is_finished();
        count
    }

    fn render_fallback(&mut self, output: &mut [f32]) -> usize {
        let count = self.morse.render(output);
        self.finished = count < output.len();
        count
    }

    /// Restart this media item without allocating or replacing its fallback.
    pub fn restart(&mut self, receiving: bool) {
        self.offset = 0;
        if let Some(tone) = &mut self.tone {
            tone.restart();
        }
        self.morse.restart();
        self.finished = false;
        self.waiting_for_stream = false;
        // The worker reader is reusable; each pass requests a fresh producer stream.
        self.using_stream = self.stream.is_some() && !receiving;
        self.stream_started = false;
        self.using_prepared = !self.prepared.is_empty() && !receiving;
        self.using_tone = !self.using_prepared && self.tone.is_some() && !receiving;
    }

    /// Return a stream handle for retirement by its non-audio owner.
    pub fn take_stream(&mut self) -> Option<Box<dyn PcmStreamReader>> {
        self.using_stream = false;
        self.stream_started = false;
        self.stream.take()
    }

    /// Whether selected streamed audio is waiting for its producer and must not key PTT.
    #[must_use]
    pub const fn waiting_for_stream(&self) -> bool {
        self.waiting_for_stream
    }

    /// Whether the selected source is streamed PCM rather than generated or prepared audio.
    #[must_use]
    pub const fn is_streaming(&self) -> bool {
        self.using_stream
    }

    /// Return whether the current run has reached terminal completion.
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
