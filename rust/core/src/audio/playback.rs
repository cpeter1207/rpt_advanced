//! Prepared-media playback with an irreversible Morse fallback.

use super::{AudioSource, MorseError, MorseRenderer};

/// An invalid playback fallback configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackError;

/// One scheduled audio item with prepared PCM and a ready Morse fallback.
pub struct Playback {
    prepared: Vec<f32>,
    using_prepared: bool,
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
        let morse = MorseRenderer::new(
            morse_text,
            morse_speed_wpm,
            morse_frequency_hz,
            morse_level_db,
        )
        .map_err(|MorseError| PlaybackError)?;
        let prepared = prepared
            .filter(|audio| !audio.is_empty())
            .unwrap_or_default();
        Ok(Self {
            using_prepared: !prepared.is_empty() && !receiving,
            prepared,
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
            self.using_prepared = false;
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
        } else {
            let count = self.morse.render(output);
            self.finished = count < output.len();
            count
        }
    }

    /// Restart this prepared media item without allocating or replacing its fallback.
    pub fn restart(&mut self, receiving: bool) {
        self.offset = 0;
        self.morse.restart();
        self.finished = false;
        self.using_prepared = !self.prepared.is_empty() && !receiving;
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
