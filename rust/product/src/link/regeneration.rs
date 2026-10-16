//! Finite DTMF PCM produced outside the audio callback.

use crate::{Error, link::ring::InboundProducer};
use rpt_advanced_core::{audio::DtmfGenerator, config::ResolvedDtmfRegenerationSettings};
use std::time::{Duration, Instant};

pub(super) struct RegenerationProducer {
    settings: ResolvedDtmfRegenerationSettings,
    digits: rtrb::Consumer<char>,
    writer: InboundProducer,
    generator: Option<DtmfGenerator>,
    start: Option<Instant>,
    scratch: Vec<f32>,
    pending: std::ops::Range<usize>,
    delay: usize,
    padding: usize,
}
impl RegenerationProducer {
    pub(super) fn new(
        settings: ResolvedDtmfRegenerationSettings,
        digits: rtrb::Consumer<char>,
        writer: InboundProducer,
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
        })
    }
    /// One bounded chunk per turn; a full ring retains the unaccepted suffix.
    pub(super) fn pump(&mut self, now: Instant) -> Result<(), Error> {
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
                let digit = self.digits.pop().map_err(|_| Error::Operation)?;
                self.generator = Some(
                    DtmfGenerator::new(
                        digit,
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
            .write(&self.scratch[self.pending.clone()])
            .map_err(|_| Error::Write)?;
        self.pending.start += accepted;
        Ok(())
    }
}

#[cfg(test)]
#[path = "regeneration_tests.rs"]
mod tests;
