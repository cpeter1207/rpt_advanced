//! Command-only phone I/O on the existing serial peer owner, independent of RF.
use super::{
    dispatcher::PeerWake,
    outbound::Outbound,
    session::{
        SerialSession,
        source::{Decoded, SourceIngress},
    },
};
use crate::{
    Error,
    phone::PrivatePhoneReply,
    services::{
        PcmProcessor, PeerInput, PeerIo, PhoneDigitMode, SourceCompletion, SourceInput,
        SourceTimeClock,
    },
};
use rpt_advanced_core::{
    audio::{AudioSpan, LinkAudioProducer, LinkAudioQueue, PlaybackOutcome},
    link::PeerReply,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[path = "phone_media.rs"]
pub(crate) mod media;

const BLOCK: usize = 960;

enum PrivateCommand {
    Reply(PrivatePhoneReply),
    Media(Box<media::Ingress>),
}

/// Control endpoint retains the exact source's revocation fence, never a radio handle.
pub(crate) struct PrivatePhoneControl {
    replies: Producer<PrivateCommand>,
    digits: Consumer<(char, u64)>,
    reply: PeerReply,
    wake: PeerWake,
    stop: Arc<AtomicBool>,
    rate: u32,
    source_media: bool,
    media: Option<Arc<media::Shared>>,
}
impl PrivatePhoneControl {
    /// Queue a prepared item without consuming it when this call is backpressured.
    pub(crate) fn send(&mut self, reply: PrivatePhoneReply) -> Result<(), PrivatePhoneReply> {
        if !reply.targets(&self.reply) {
            return Err(reply);
        }
        if self.replies.slots() == 0 {
            return Err(reply);
        }
        // This endpoint has one producer; the consumer can only free slots.
        self.replies
            .push(PrivateCommand::Reply(reply))
            .unwrap_or_else(|_| unreachable!("reserved private command slot"));
        self.wake.notify();
        Ok(())
    }
    /// Prepare only a source-timed receive generation, not an RF attachment.
    pub(crate) fn prepare_media(
        &mut self,
        processor: Option<PcmProcessor>,
    ) -> Result<media::PhoneMediaInput, Error> {
        if !self.source_media || !self.reply.phone_authenticated() {
            return Err(Error::Admission);
        }
        if self.media.as_ref().is_some_and(|media| media.occupied()) {
            return Err(Error::Reservation);
        }
        if self.replies.slots() == 0 {
            return Err(Error::Allocation);
        }
        let (ingress, input, shared) = media::prepare(self.reply.clone(), self.rate, processor)?;
        self.replies
            .push(PrivateCommand::Media(ingress))
            .unwrap_or_else(|_| unreachable!("reserved private command slot"));
        self.media = Some(shared);
        self.wake.notify();
        Ok(input)
    }
    pub(crate) fn retire_media(&mut self) {
        if let Some(media) = self.media.take() {
            media.retire();
        }
        self.wake.notify();
    }
    pub(crate) fn digit(&mut self) -> Option<(char, u64)> {
        self.digits.pop().ok()
    }
    #[cfg(test)]
    pub(crate) fn pending_digits(&self) -> usize {
        self.digits.slots()
    }
}
impl Drop for PrivatePhoneControl {
    fn drop(&mut self) {
        self.retire_media();
        self.stop.store(true, Ordering::Release);
        self.reply.retire();
        self.wake.notify();
    }
}

/// Sole owner of a private call's transport and the shared existing egress converter.
pub(crate) struct PrivatePhoneSession {
    io: PeerIo,
    reply: PeerReply,
    stop: Arc<AtomicBool>,
    wake: PeerWake,
    epoch: Option<Instant>,
    source_clock: Option<SourceTimeClock>,
    source_completion: SourceCompletion,
    digit_mode: PhoneDigitMode,
    // A 20 ms offer needs at most three 8 ms partitions at either released rate.
    source: Option<SourceIngress<4>>,
    replies: Consumer<PrivateCommand>,
    digits: Producer<(char, u64)>,
    current: Option<PrivatePhoneReply>,
    media: Option<Box<media::Ingress>>,
    source_end: u64,
    output: LinkAudioProducer,
    outbound: Outbound,
    source_position: u64,
    next_audio_ms: u64,
    pcm: [f32; BLOCK],
}
impl PrivatePhoneSession {
    /// Prepare bounded private media/control without radio binding or ASL negotiation.
    pub(crate) fn prepare(
        io: PeerIo,
        reply: PeerReply,
    ) -> Result<(Self, PrivatePhoneControl), Error> {
        let source = reply.phone_source().ok_or(Error::Admission)?;
        if io.identity().call_id() != Some(source.call_id())
            || io.phone_account() != Some(source.account())
            || io.generations() != (source.account_generation(), source.call_generation())
            || reply.ended()
        {
            return Err(Error::Admission);
        }
        let digit_mode = io.phone_digit_mode().ok_or(Error::Admission)?;
        let rate = io.rate();
        let source = if io.source_media() {
            Some(SourceIngress::<4>::at_rate(true, rate, rate as usize / 50)?)
        } else {
            None
        };
        let (output, input) = LinkAudioQueue::new(BLOCK, 1)
            .expect("fixed nonzero private block and span capacities")
            .into_endpoints();
        let outbound = Outbound::new(rate, input)?;
        let (replies, incoming) = RingBuffer::new(16);
        let (digits, events) = RingBuffer::new(64);
        let wake = PeerWake::default();
        let stop = Arc::new(AtomicBool::new(false));
        let control = PrivatePhoneControl {
            replies,
            digits: events,
            reply: reply.clone(),
            wake: wake.clone(),
            stop: Arc::clone(&stop),
            rate,
            source_media: io.source_media(),
            media: None,
        };
        Ok((
            Self {
                io,
                reply,
                stop,
                wake,
                epoch: None,
                source_clock: None,
                source_completion: SourceCompletion::Pending,
                digit_mode,
                source,
                replies: incoming,
                digits,
                current: None,
                media: None,
                source_end: 0,
                output,
                outbound,
                source_position: 0,
                next_audio_ms: 0,
                pcm: [0.0; BLOCK],
            },
            control,
        ))
    }
    fn read_legacy(&mut self, now_ms: u64) -> Result<bool, Error> {
        if !self.io.ready()? {
            return Ok(false);
        }
        let mut digit = None;
        let mut duplicate = false;
        let seen = self.io.read(|input| {
            if let PeerInput::Digit(value) = input {
                duplicate |= digit.replace(value).is_some();
            }
            // Command-only audio/text/key events create no ring or RF demand.
        })?;
        if duplicate {
            return Err(Error::InvalidFrame);
        }
        if let Some(digit) = digit {
            self.digits
                .push((digit, now_ms))
                .expect("reserved digit slot");
        }
        Ok(seen)
    }
    fn read_source(&mut self, now_ms: u64) -> Result<bool, Error> {
        let reference = self
            .source_clock
            .map(SourceTimeClock::reference)
            .unwrap_or_default();
        let digits = &mut self.digits;
        let media = &mut self.media;
        let source_end = &mut self.source_end;
        let source = self.source.as_mut().ok_or(Error::Operation)?;
        let mode = self.digit_mode;
        let scratch = &mut self.pcm;
        let mut failure = None;
        let result = self
            .io
            .read_source_at(self.source_completion, reference, |input| {
                let credit = match input {
                    SourceInput::Digit(digit) => {
                        if mode == PhoneDigitMode::InBand {
                            return 1;
                        }
                        if digits.slots() == 0 {
                            return 0;
                        }
                        if let Some(media) = media.as_ref() {
                            media.retire();
                        }
                        if digit == '#' {
                            source.end_command();
                        }
                        return usize::from(digits.push((digit, now_ms)).is_ok());
                    }
                    SourceInput::Audio {
                        start,
                        samples,
                        concealed,
                        birth,
                    } => {
                        if samples.is_empty() {
                            return 0;
                        }
                        let count = source.limit(samples.len());
                        // Reserve the worst-case completions before advancing detection.
                        if mode == PhoneDigitMode::InBand
                            && digits.slots() < source.digit_capacity(count)
                        {
                            return 0;
                        }
                        *source_end = (*source_end).max(start + count as u64);
                        let current_media = media.as_ref();
                        let analyzed = source.prepare(
                            scratch,
                            Decoded {
                                start,
                                pcm: &samples[..count],
                                concealed,
                                birth,
                            },
                            |digit| {
                                if mode == PhoneDigitMode::InBand {
                                    digits
                                        .push((digit, now_ms))
                                        .expect("reserved private analysis digit slots");
                                }
                            },
                            || {
                                if let Some(media) = current_media {
                                    media.retire();
                                }
                            },
                        );
                        analyzed.and_then(|()| {
                            if source.muted() {
                                if let Some(media) = media.as_ref() {
                                    media.retire();
                                }
                            }
                            let accepted = match media.as_mut() {
                                Some(media) if concealed => media.gap(start, count)?,
                                Some(media) => media.audio(start, &scratch[..count], birth)?,
                                None => count,
                            };
                            source.accepted(accepted)?;
                            Ok(accepted)
                        })
                    }
                    SourceInput::Gap { start, count } => {
                        if mode == PhoneDigitMode::InBand && digits.slots() == 0 {
                            return 0;
                        }
                        source
                            .discontinuity(|digit| {
                                if mode == PhoneDigitMode::InBand {
                                    digits
                                        .push((digit, now_ms))
                                        .expect("reserved private loss digit slot");
                                }
                            })
                            .and_then(|()| {
                                *source_end = (*source_end).max(start + count as u64);
                                media
                                    .as_mut()
                                    .map_or(Ok(count), |media| media.gap(start, count))
                            })
                    }
                    SourceInput::Discontinuity => {
                        if let Some(media) = media.take() {
                            media.retire();
                        }
                        if mode == PhoneDigitMode::InBand && digits.slots() == 0 {
                            return 0;
                        }
                        match source.reset(|digit| {
                            if mode == PhoneDigitMode::InBand {
                                digits
                                    .push((digit, now_ms))
                                    .expect("reserved private reset digit slot");
                            }
                        }) {
                            Ok(()) => {
                                *source_end = 0;
                                return 1;
                            }
                            Err(error) => Err(error),
                        }
                    }
                    SourceInput::Text { .. } => return 0,
                };
                match credit {
                    Ok(count) => count,
                    Err(error) => {
                        failure = Some(error);
                        0
                    }
                }
            })?;
        if let Some(error) = failure {
            return Err(error);
        }
        if result.closed {
            return Err(Error::Hangup);
        }
        if result.finish_pending {
            if let Some(media) = self.media.take() {
                media.retire();
            }
            if mode == PhoneDigitMode::InBand && digits.slots() == 0 {
                return Ok(false);
            }
            source.discontinuity(|digit| {
                if mode == PhoneDigitMode::InBand {
                    digits
                        .push((digit, now_ms))
                        .expect("reserved private finish digit slot");
                }
            })?;
        }
        let acknowledge =
            result.finish_pending && self.source_completion == SourceCompletion::Pending;
        self.source_completion = if result.finish_pending {
            SourceCompletion::Cancelled
        } else {
            SourceCompletion::Pending
        };
        Ok(matches!(result.progress, 1 | 3) || acknowledge)
    }
    fn step_inner(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.stop.load(Ordering::Acquire) || self.reply.ended() {
            return Err(Error::Hangup);
        }
        if self.current.is_none() {
            for _ in 0..16 {
                match self.replies.pop().ok() {
                    Some(PrivateCommand::Reply(reply)) => {
                        self.current = Some(reply);
                        break;
                    }
                    Some(PrivateCommand::Media(mut media)) => {
                        if self.source.as_ref().is_some_and(SourceIngress::muted) {
                            media.retire();
                        }
                        media.adopt(
                            self.source_end,
                            self.source_clock
                                .map(SourceTimeClock::reference)
                                .unwrap_or_default(),
                        );
                        self.media = Some(media);
                    }
                    None => break,
                }
            }
        }
        // One serial provider reader. Source callbacks retain native digits or
        // unanalyzed PCM under backpressure; non-authoritative symbols can drain.
        for _ in 0..64 {
            if !self.io.source_media() && self.digits.slots() == 0 {
                break;
            }
            let progressed = if self.io.source_media() {
                self.read_source(now_ms)?
            } else {
                self.read_legacy(now_ms)?
            };
            if !progressed {
                break;
            }
        }
        if now_ms >= self.next_audio_ms && self.outbound.ready_for_input() {
            if let Some(reply) = &mut self.current {
                let count = reply.render(false, &mut self.pcm);
                if count != 0 {
                    let source = self.reply.phone_source().expect("prepared phone source");
                    // ready_for_input acquired an empty one-block queue; Playback
                    // returns at most this prepared block's capacity.
                    self.output.write(
                        AudioSpan {
                            generation: source.call_generation(),
                            burst: 0,
                            source_start: self.source_position,
                            stream: source.ingress(),
                            // This is newly rendered private prompt PCM, never provider receive PCM.
                            born_ns: Some(now_ms.saturating_mul(1_000_000)),
                        },
                        &self.pcm[..count],
                    );
                    self.source_position = self.source_position.wrapping_add(count as u64);
                    self.next_audio_ms = now_ms.saturating_add(20);
                }
                match reply.outcome() {
                    PlaybackOutcome::Active => {}
                    PlaybackOutcome::Succeeded => self.current = None,
                    PlaybackOutcome::Failed | PlaybackOutcome::Cancelled => {
                        return Err(Error::Operation);
                    }
                }
            }
        }
        #[cfg(test)]
        tests::before_send();
        if self.reply.ended() {
            return Err(Error::Hangup);
        }
        let deadline = self.outbound.step(&mut self.io, now_ms)?;
        let next = deadline
            .into_iter()
            .chain(Some(now_ms.saturating_add(10).saturating_mul(1_000_000)))
            .min()
            .unwrap();
        let elapsed = self
            .epoch
            .map_or(now_ms.saturating_mul(1_000_000), |epoch| {
                epoch.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
            });
        self.io.set_deadline(next.saturating_sub(elapsed))
    }
}
impl SerialSession for PrivatePhoneSession {
    fn io(&self) -> &PeerIo {
        &self.io
    }
    fn step(&mut self, now_ms: u64) -> Result<(), Error> {
        let result = self.step_inner(now_ms);
        if let Err(error) = result {
            if self.reply.ended() {
                self.stop();
            } else {
                self.fail("private call I/O", error);
            }
        }
        result
    }
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.reply.retire();
    }
    fn fail(&self, cause: &str, error: Error) {
        let first = !self.stop.swap(true, Ordering::AcqRel);
        self.reply.retire();
        if first {
            super::session::log_error(&format!(
                "rpt_advanced {} phone: unexpected termination during {cause}: {error:?}",
                self.io.local().escape_default(),
            ));
        }
    }
    fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }
    fn reply(&self) -> PeerReply {
        self.reply.clone()
    }
    fn wake(&self) -> PeerWake {
        self.wake.clone()
    }
    fn epoch(&self) -> Option<Instant> {
        self.epoch
    }
    fn set_epoch(&mut self, epoch: Instant) {
        self.epoch = Some(epoch);
    }
    fn set_source_clock(&mut self, clock: SourceTimeClock) {
        self.source_clock = Some(clock);
    }
}
impl Drop for PrivatePhoneSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "private_phone_tests.rs"]
mod tests;
