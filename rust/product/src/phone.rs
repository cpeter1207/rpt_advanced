//! Serialized private call ownership; no radio binding, topology, or ASL session.
use crate::{Error, services::PeerIo};
use rpt_advanced_core::{
    audio::{PcmStreamReader, Playback, PlaybackOutcome},
    link::{MailboxId, PeerReply, PeerSignals, PhoneCallId, PhoneSource},
};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_INGRESS: AtomicU64 = AtomicU64::new(1);

fn next_ingress(sequence: &AtomicU64) -> Result<u64, Error> {
    sequence
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| Error::Admission)
}

/// One prepared private stream, consumed by the existing serial media/transport owner.
/// Rendering reuses the common Playback implementation and never requests RF output.
pub struct PrivatePhoneReply {
    source: PhoneSource,
    reply: PeerReply,
    playback: Playback,
}
impl PrivatePhoneReply {
    /// Exact account, call, generation, and actual ingress retained at preparation.
    pub fn source(&self) -> &PhoneSource {
        &self.source
    }
    /// Retirement wins even after this item has left its bounded control queue.
    pub fn current(&self) -> bool {
        !self.reply.ended()
    }
    /// Render canonical 48 kHz PCM only while the same call remains live.
    /// This belongs to serialized media, never a hardware audio callback.
    pub fn render(&mut self, receiving: bool, output: &mut [f32]) -> usize {
        if !self.current() {
            return 0;
        }
        let written = self.playback.render(receiving, output);
        if !self.current() {
            output[..written].fill(0.0);
            return 0;
        }
        written
    }
    /// A retired call is cancellation, never successful prompt delivery.
    pub fn outcome(&self) -> PlaybackOutcome {
        if self.current() {
            self.playback.outcome()
        } else {
            PlaybackOutcome::Cancelled
        }
    }
}

struct Call {
    _io: PeerIo,
    source: PhoneSource,
    signals: PeerSignals,
    mailbox: Option<MailboxId>,
    replies: VecDeque<PrivatePhoneReply>,
}
impl Drop for Call {
    fn drop(&mut self) {
        self.signals.end();
    }
}

/// Bounded command-only calls owned by station control, separate from radio peer slots.
pub struct PhoneCalls {
    calls: Vec<Call>,
    maximum_calls: usize,
    maximum_replies: usize,
}
impl PhoneCalls {
    /// Prepare explicit transport/prompt bounds; these are not RF attachment limits.
    pub fn new(maximum_calls: usize, maximum_replies: usize) -> Result<Self, Error> {
        if maximum_calls == 0 || maximum_replies == 0 {
            return Err(Error::Admission);
        }
        let mut calls = Vec::new();
        calls
            .try_reserve_exact(maximum_calls)
            .map_err(|_| Error::Allocation)?;
        Ok(Self {
            calls,
            maximum_calls,
            maximum_replies,
        })
    }
    /// Adopt a validated private handle without starting any ASL/radio media session.
    /// A duplicate is rejected rather than replacing another live call implicitly.
    pub fn admit(&mut self, io: PeerIo) -> Result<PhoneSource, Error> {
        let call = io.identity().call_id().ok_or(Error::Admission)?;
        let account = io.phone_account().ok_or(Error::Admission)?.clone();
        if self.calls.len() == self.maximum_calls
            || self
                .calls
                .iter()
                .any(|existing| existing.source.call_id() == call)
        {
            return Err(Error::Admission);
        }
        let (account_generation, call_generation) = io.generations();
        let source = PhoneSource::new(
            account,
            account_generation,
            call,
            call_generation,
            next_ingress(&NEXT_INGRESS)?,
        )
        .map_err(|_| Error::Admission)?;
        let mut replies = VecDeque::new();
        replies
            .try_reserve_exact(self.maximum_replies)
            .map_err(|_| Error::Allocation)?;
        self.calls.push(Call {
            _io: io,
            source: source.clone(),
            signals: PeerSignals::for_phone(source.clone()),
            mailbox: None,
            replies,
        });
        Ok(source)
    }
    /// Lookup a provider call without manufacturing a public numeric node alias.
    pub fn source(&self, call: PhoneCallId) -> Option<&PhoneSource> {
        self.calls
            .iter()
            .find(|item| item.source.call_id() == call)
            .map(|item| &item.source)
    }
    /// Retain the actual call reply token before dispatching asynchronous work.
    pub fn reply(&self, source: &PhoneSource) -> Option<PeerReply> {
        self.calls
            .iter()
            .find(|item| &item.source == source)
            .map(|item| item.signals.reply_target())
    }
    fn current(&mut self, source: &PhoneSource, reply: &PeerReply) -> Result<&mut Call, Error> {
        self.calls
            .iter_mut()
            .find(|item| {
                &item.source == source && item.signals.reply_target() == *reply && !reply.ended()
            })
            .ok_or(Error::Admission)
    }
    /// Publish a mailbox only after the caller has verified its PIN. This method
    /// does not verify credentials or grant any node-administration authority.
    pub fn authenticate(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        mailbox: MailboxId,
    ) -> Result<(), Error> {
        let call = self.current(source, reply)?;
        if call.mailbox.is_some() {
            return Err(Error::Admission);
        }
        call.mailbox = Some(mailbox);
        if !call.signals.authenticate_phone() {
            return Err(Error::Admission);
        }
        Ok(())
    }
    /// Queue one already-prepared stream only to its exact call, including login prompts.
    pub fn queue_reply(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        stream: Box<dyn PcmStreamReader>,
    ) -> Result<(), Error> {
        let maximum = self.maximum_replies;
        let call = self.current(source, reply)?;
        if call.replies.len() == maximum {
            return Err(Error::Allocation);
        }
        call.replies.push_back(PrivatePhoneReply {
            source: source.clone(),
            reply: reply.clone(),
            playback: Playback::new_stream(stream),
        });
        Ok(())
    }
    /// Transfer a prepared private stream to its serial egress owner, never an RF mixer.
    pub fn take_reply(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
    ) -> Result<Option<PrivatePhoneReply>, Error> {
        Ok(self.current(source, reply)?.replies.pop_front())
    }
    /// Revoke one exact generation before destroying its transport and queued streams.
    pub fn retire(&mut self, source: &PhoneSource) -> Option<PeerReply> {
        let index = self.calls.iter().position(|item| &item.source == source)?;
        let call = self.calls.remove(index);
        call.signals.end();
        Some(call.signals.reply_target())
    }
    /// Retire all calls at committed configuration replacement or module stop.
    pub fn retire_all(&mut self) -> Vec<PeerReply> {
        self.calls
            .drain(..)
            .map(|call| {
                call.signals.end();
                call.signals.reply_target()
            })
            .collect()
    }
    /// No private call consumes a public/radio-peer slot.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

#[cfg(test)]
#[path = "phone_tests.rs"]
mod tests;
