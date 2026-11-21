//! Finite DTMF PCM produced outside the audio callback.

use crate::{Error, link::ring::RegenerationWriter};
use rpt_advanced_core::link::RegenerationOrigin;
use rpt_advanced_core::{audio::DtmfGenerator, config::ResolvedDtmfRegenerationSettings};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct RegenerationDigit {
    pub(crate) symbol: char,
    pub(crate) origin: Option<RegenerationOrigin>,
}
impl From<char> for RegenerationDigit {
    fn from(symbol: char) -> Self {
        Self {
            symbol,
            origin: None,
        }
    }
}
pub(super) struct RegenerationProducer {
    settings: ResolvedDtmfRegenerationSettings,
    digits: rtrb::Consumer<RegenerationDigit>,
    writer: RegenerationWriter,
    generator: Option<DtmfGenerator>,
    start: Option<Instant>,
    scratch: Vec<f32>,
    pending: std::ops::Range<usize>,
    delay: usize,
    padding: usize,
    position: u64,
    origin: Option<RegenerationOrigin>,
}
impl RegenerationProducer {
    pub(super) fn new(
        settings: ResolvedDtmfRegenerationSettings,
        digits: rtrb::Consumer<RegenerationDigit>,
        writer: RegenerationWriter,
        maximum: usize,
    ) -> Result<Self, Error> {
        if !(1..=4096).contains(&maximum) {
            return Err(Error::InvalidFrame);
        }
        let delay = usize::try_from(
            writer
                .output_delay_samples()
                .map_err(|_| Error::Operation)?,
        )
        .map_err(|_| Error::Operation)?;
        Ok(Self {
            settings,
            digits,
            writer,
            generator: None,
            start: None,
            scratch: vec![0.0; maximum],
            pending: 0..0,
            delay,
            padding: 0,
            position: 0,
            origin: None,
        })
    }
    /// One bounded chunk per turn; a full ring retains the unaccepted suffix.
    pub(super) fn pump(&mut self, now: Instant) -> Result<(), Error> {
        self.writer.reclaim_origins();
        if self.origin.as_ref().is_some_and(|o| !o.current()) {
            self.generator = None;
            self.pending = 0..0;
            // Flush the filter under the revoked marker before a new source can key.
            self.padding = self.delay;
            self.origin = None;
        }
        if self.pending.is_empty() && self.padding != 0 {
            let count = self.padding.min(self.scratch.len());
            self.scratch[..count].fill(0.0);
            self.pending = 0..count;
            self.padding -= count;
        }
        if self.pending.is_empty() {
            if self.generator.is_none() {
                if self.digits.is_empty() {
                    self.start = None;
                    return Ok(());
                }
                let start = self.start.get_or_insert(now + Duration::from_millis(500));
                if now < *start {
                    return Ok(());
                }
                let digit = self.digits.peek().map_err(|_| Error::Operation)?;
                if digit.origin.as_ref().is_some_and(|o| !o.current()) {
                    let _ = self.digits.pop();
                    return Ok(());
                }
                if !self.writer.mark_origin(self.position, &digit.origin) {
                    return Ok(());
                }
                let digit = self.digits.pop().map_err(|_| Error::Operation)?;
                self.origin = digit.origin;
                self.generator = Some(
                    DtmfGenerator::new(
                        digit.symbol,
                        self.settings.duration_ms,
                        self.settings.gap_ms,
                        self.settings.level_dbfs,
                    )
                    .map_err(|_| Error::Operation)?,
                );
            }
            let made = self
                .generator
                .as_mut()
                .expect("generator prepared above")
                .render(&mut self.scratch)
                .map_err(|_| Error::Operation)?;
            if made == 0 {
                self.generator = None;
                // Flush finite media, including its final gap, through the ring's filter.
                if self.digits.is_empty() {
                    self.padding = self.delay;
                }
                return Ok(());
            }
            self.pending = 0..made;
        }
        let accepted = self
            .writer
            .write_generated(&self.scratch[self.pending.clone()], self.position)
            .map_err(|_| Error::Write)?;
        self.pending.start += accepted;
        self.position = self
            .position
            .checked_add(accepted as u64)
            .ok_or(Error::Operation)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "regeneration_tests.rs"]
mod tests;
