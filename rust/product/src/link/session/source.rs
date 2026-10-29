//! Source-rate detection and retained mute metadata; the provider owns raw retries.
use crate::{
    Error,
    link::ring::{InboundProducer, MuteSpan},
};
use rpt_advanced_core::audio::DtmfDetector;

const EMPTY: MuteSpan = MuteSpan { start: 0, end: 0 };

#[derive(Clone, Copy)]
struct Cached {
    start: u64,
    count: usize,
    next: usize,
    concealed: bool,
}

pub(super) struct Decoded<'a> {
    pub start: u64,
    pub pcm: &'a [f32],
    pub concealed: bool,
}

/// One serial receive owner. Cached metadata describes transformed samples in
/// PeerSession's existing scratch block; raw rejected PCM remains provider-owned.
pub(super) struct SourceIngress {
    detector: DtmfDetector,
    muting: bool,
    command_muted: bool,
    generation: u64,
    burst_start: Option<u64>,
    cursor: u64,
    cached: Option<Cached>,
    spans: [MuteSpan; 64],
    span_count: usize,
    pending_mute: Option<MuteSpan>,
    finishing: bool,
}

fn span(spans: &mut [MuteSpan; 64], count: &mut usize, start: u64, end: u64) -> Result<(), Error> {
    if start >= end {
        return Ok(());
    }
    if *count != 0 && start <= spans[*count - 1].end {
        let previous = &mut spans[*count - 1];
        previous.start = previous.start.min(start);
        previous.end = previous.end.max(end);
    } else {
        let output = spans.get_mut(*count).ok_or(Error::InvalidFrame)?;
        *output = MuteSpan { start, end };
        *count += 1;
    }
    Ok(())
}

impl SourceIngress {
    pub(super) fn new(muting: bool) -> Result<Self, Error> {
        Ok(Self {
            detector: DtmfDetector::at_rate(muting, 8000).map_err(|_| Error::UnsupportedFormat)?,
            muting,
            command_muted: false,
            generation: 1,
            burst_start: None,
            cursor: 0,
            cached: None,
            spans: [EMPTY; 64],
            span_count: 0,
            pending_mute: None,
            finishing: false,
        })
    }

    pub(super) fn reset(&mut self) -> Result<(), Error> {
        self.detector
            .process_spans(false, &mut [], |_| {}, |_| {})
            .map_err(|_| Error::InvalidFrame)?;
        self.generation = self.generation.checked_add(1).ok_or(Error::InvalidFrame)?;
        self.command_muted = false;
        self.burst_start = None;
        self.cached = None;
        self.span_count = 0;
        self.pending_mute = None;
        self.finishing = false;
        Ok(())
    }
    pub(super) fn set_muting(&mut self, muting: bool) {
        self.muting = muting;
        self.detector.set_muting(muting);
    }

    fn flush_mute(&mut self, inbound: &mut InboundProducer) -> Result<bool, Error> {
        if let Some(pending) = self.pending_mute {
            if inbound
                .mute(self.generation, &[pending])
                .map_err(|_| Error::Write)?
                == 0
            {
                return Ok(false);
            }
            self.pending_mute = None;
        }
        Ok(true)
    }

    // Loss is not contiguous fresh tone. Complete only an already qualified
    // real digit; never let analysis windows straddle unknown or codec-PLC PCM.
    fn discontinuity(&mut self, mut emit: impl FnMut(char)) -> Result<(), Error> {
        let command_muted = &mut self.command_muted;
        let muting = self.muting;
        self.detector
            .process_spans(
                false,
                &mut [],
                |digit| {
                    *command_muted = muting && digit != '#';
                    emit(digit);
                },
                |_| {},
            )
            .map_err(|_| Error::InvalidFrame)
    }

    pub(super) fn audio(
        &mut self,
        inbound: &mut InboundProducer,
        scratch: &mut [f32],
        block: Decoded<'_>,
        mut emit: impl FnMut(char),
    ) -> Result<usize, Error> {
        if !self.flush_mute(inbound)? || block.pcm.is_empty() {
            return Ok(0);
        }
        if self.cached.is_none() {
            if block.pcm.len() > scratch.len() {
                return Err(Error::InvalidFrame);
            }
            let burst_start = *self.burst_start.get_or_insert(block.start);
            let concealed_muted =
                block.concealed && (self.command_muted || self.detector.suppressing());
            if block.concealed {
                self.discontinuity(&mut emit)?;
            }
            scratch[..block.pcm.len()].copy_from_slice(block.pcm);
            self.span_count = 0;
            // Eight-ms partitions bound the end-of-command mute granularity.
            // Each offered source block is detected once, never again on retry.
            for (part, pcm) in scratch[..block.pcm.len()].chunks_mut(64).enumerate() {
                let start = block.start + (part * 64) as u64;
                let end = start + pcm.len() as u64;
                let was_muted = self.command_muted;
                let mut digit = None;
                let mut first = None;
                let mut failure = None;
                if !block.concealed {
                    let spans = &mut self.spans;
                    let count = &mut self.span_count;
                    self.detector
                        .process_spans(
                            true,
                            pcm,
                            |decoded| {
                                digit = Some(decoded);
                                emit(decoded);
                            },
                            |range| {
                                let from =
                                    start.saturating_add_signed(range.start).max(burst_start);
                                let to = start.saturating_add_signed(range.end).min(end);
                                if from < to {
                                    first = Some(first.map_or(from, |old: u64| old.min(from)));
                                    inbound.signals().set_dtmf_muted(true);
                                    if let Err(error) = span(spans, count, from, to) {
                                        failure = Some(error);
                                    }
                                }
                            },
                        )
                        .map_err(|_| Error::InvalidFrame)?;
                }
                if let Some(error) = failure {
                    return Err(error);
                }
                let muted = self.muting
                    && (was_muted || first.is_some() || digit.is_some() || concealed_muted);
                if muted {
                    span(
                        &mut self.spans,
                        &mut self.span_count,
                        first.unwrap_or(start),
                        end,
                    )?;
                }
                if !block.concealed {
                    self.command_muted = muted && digit != Some('#');
                }
            }
            self.cached = Some(Cached {
                start: block.start,
                count: block.pcm.len(),
                next: 0,
                concealed: block.concealed,
            });
        }
        let cached = self.cached.as_mut().ok_or(Error::InvalidFrame)?;
        if block.start != cached.start + cached.next as u64
            || block.pcm.len() > cached.count - cached.next
            || block.concealed != cached.concealed
        {
            return Err(Error::InvalidFrame);
        }
        let end = block.start + block.pcm.len() as u64;
        let mut offered = [EMPTY; 64];
        let mut count = 0;
        for interval in &self.spans[..self.span_count] {
            span(
                &mut offered,
                &mut count,
                interval.start,
                interval.end.min(end),
            )?;
        }
        let accepted = inbound
            .write_tagged(
                &scratch[cached.next..cached.next + block.pcm.len()],
                self.generation,
                block.start,
                &offered[..count],
            )
            .map_err(|_| Error::Write)?;
        cached.next += accepted;
        self.cursor = block.start + accepted as u64;
        if cached.next == cached.count {
            self.cached = None;
        }
        if accepted != 0 {
            inbound
                .signals()
                .set_dtmf_muted(self.command_muted || self.detector.suppressing());
        }
        Ok(accepted)
    }

    pub(super) fn gap(
        &mut self,
        inbound: &mut InboundProducer,
        start: u64,
        count: usize,
        emit: impl FnMut(char),
    ) -> Result<usize, Error> {
        if self.cached.is_some() {
            return Err(Error::InvalidFrame);
        }
        if !self.flush_mute(inbound)? {
            return Ok(0);
        }
        self.discontinuity(emit)?;
        inbound.signals().set_dtmf_muted(self.command_muted);
        let accepted = inbound
            .gap(count.min(4096) as u64)
            .map_err(|_| Error::Write)?;
        self.cursor = start + accepted;
        if self.command_muted && accepted != 0 {
            self.pending_mute = Some(MuteSpan {
                start,
                end: self.cursor,
            });
            self.flush_mute(inbound)?;
        }
        Ok(accepted as usize)
    }

    pub(super) fn finish(
        &mut self,
        inbound: &mut InboundProducer,
        mut emit: impl FnMut(char),
    ) -> Result<bool, Error> {
        if self.cached.is_some() {
            return Err(Error::InvalidFrame);
        }
        if !self.flush_mute(inbound)? {
            return Ok(false);
        }
        if !self.finishing {
            self.discontinuity(&mut emit)?;
            self.finishing = true;
        }
        self.command_muted = false;
        inbound.signals().set_dtmf_muted(false);
        Ok(true)
    }

    pub(super) fn timeout(&mut self, inbound: &InboundProducer) {
        self.command_muted = false;
        if self.cached.is_none() && self.pending_mute.is_none() {
            inbound
                .signals()
                .set_dtmf_muted(self.detector.suppressing());
        }
    }
}
