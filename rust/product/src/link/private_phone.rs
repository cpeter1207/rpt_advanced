//! Command-only phone I/O on the existing serial peer owner, independent of RF.
use super::{dispatcher::PeerWake, outbound::Outbound, session::SerialSession};
use crate::{
    Error,
    phone::PrivatePhoneReply,
    services::{PeerInput, PeerIo},
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

const BLOCK: usize = 960;

/// Control endpoint retains the exact source's revocation fence, never a radio handle.
pub(crate) struct PrivatePhoneControl {
    replies: Producer<PrivatePhoneReply>,
    digits: Consumer<(char, u64)>,
    reply: PeerReply,
    wake: PeerWake,
    stop: Arc<AtomicBool>,
}
impl PrivatePhoneControl {
    /// Queue a prepared item without consuming it when this call is backpressured.
    pub(crate) fn send(&mut self, reply: PrivatePhoneReply) -> Result<(), PrivatePhoneReply> {
        if !reply.targets(&self.reply) {
            return Err(reply);
        }
        self.replies
            .push(reply)
            .map_err(|rtrb::PushError::Full(reply)| reply)?;
        self.wake.notify();
        Ok(())
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
    replies: Consumer<PrivatePhoneReply>,
    digits: Producer<(char, u64)>,
    current: Option<PrivatePhoneReply>,
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
        let (output, input) = LinkAudioQueue::new(BLOCK, 1)
            .expect("fixed nonzero private block and span capacities")
            .into_endpoints();
        let outbound = Outbound::new(io.rate(), input)?;
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
        };
        Ok((
            Self {
                io,
                reply,
                stop,
                wake,
                epoch: None,
                replies: incoming,
                digits,
                current: None,
                output,
                outbound,
                source_position: 0,
                next_audio_ms: 0,
                pcm: [0.0; BLOCK],
            },
            control,
        ))
    }
    fn step_inner(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.stop.load(Ordering::Acquire) || self.reply.ended() {
            return Err(Error::Hangup);
        }
        // A full event queue leaves the provider's next digit unread; no login/command tail is lost.
        for _ in 0..64 {
            if self.digits.slots() == 0 || !self.io.ready()? {
                break;
            }
            let mut digit = None;
            let mut duplicate = false;
            let seen = self.io.read(|input| {
                if let PeerInput::Digit(value) = input {
                    duplicate |= digit.replace(value).is_some();
                }
                // Command-only audio/text/key events cannot create a ring or RF demand.
            })?;
            if duplicate {
                return Err(Error::InvalidFrame);
            }
            if let Some(digit) = digit {
                self.digits
                    .push((digit, now_ms))
                    .expect("reserved digit slot");
            }
            if !seen {
                break;
            }
        }
        if self.current.is_none() {
            self.current = self.replies.pop().ok();
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
}
impl Drop for PrivatePhoneSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "private_phone_tests.rs"]
mod tests;
