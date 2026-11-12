//! Serialized private call ownership; no radio binding, topology, or ASL session.
pub use crate::link::private_phone::media::PhoneMediaInput;
use crate::link::{
    private_phone::{PrivatePhoneControl, PrivatePhoneSession},
    session::PeerIoWorker,
};
use crate::{
    Error,
    services::{PcmProcessor, PeerIo},
};
use rpt_advanced_core::{
    audio::{PcmStreamReader, Playback, PlaybackOutcome},
    config::PhoneMailbox,
    link::{PeerReply, PeerSignals, PhoneCallId, PhoneSource},
    phone_dtmf::{PhoneDtmfAction, PhoneDtmfGrammar},
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
    pub(crate) fn targets(&self, reply: &PeerReply) -> bool {
        self.reply == *reply && self.current()
    }
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
    io: Option<PeerIo>,
    local: String,
    owner: Option<PrivatePhoneControl>,
    login: LoginCollector,
    grammar: Option<PhoneDtmfGrammar>,
    collection_dirty: bool,
    source: PhoneSource,
    signals: PeerSignals,
    mailbox: Option<PhoneMailbox>,
    attempts: u8,
    login_deadline_ms: u64,
    replies: VecDeque<PrivatePhoneReply>,
}

#[derive(Default)]
// Mailbox and PIN each end with '#'; no login symbol reaches the command grammar.
struct LoginCollector {
    mailbox: String,
    pin: String,
    pin_phase: bool,
    invalid: bool,
    last_ms: Option<u64>,
}
/// Local-only result; credential buffers never implement Debug or enter node parsers.
pub(crate) enum LoginInput {
    Credentials { mailbox: String, pin: String },
    Retry,
}
impl LoginCollector {
    fn clear(&mut self) {
        *self = Self::default();
    }
    fn tick(&mut self, now_ms: u64) -> Option<LoginInput> {
        if self
            .last_ms
            .is_some_and(|last| now_ms.saturating_sub(last) >= 3000)
        {
            self.clear();
            return Some(LoginInput::Retry);
        }
        None
    }
    fn digit(&mut self, digit: char, now_ms: u64) -> Option<LoginInput> {
        self.last_ms = Some(now_ms);
        if digit == '#' {
            if !self.pin_phase && !self.invalid && !self.mailbox.is_empty() {
                self.pin_phase = true;
                return None;
            }
            let credentials = LoginInput::Credentials {
                mailbox: if self.invalid {
                    String::new()
                } else {
                    std::mem::take(&mut self.mailbox)
                },
                pin: if self.invalid {
                    String::new()
                } else {
                    std::mem::take(&mut self.pin)
                },
            };
            self.clear();
            return Some(credentials);
        }
        let target = if self.pin_phase {
            &mut self.pin
        } else {
            &mut self.mailbox
        };
        if digit.is_ascii_digit() && target.len() < 32 && !self.invalid {
            target.push(digit);
        } else {
            self.invalid = true;
            self.mailbox.clear();
            self.pin.clear();
        }
        None
    }
}

/// Credential-free result of one exact call's login attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneLogin {
    /// Mailbox/PIN verified; ordinary command access remains separate from unlock.
    Authenticated,
    /// Generic denial; neither mailbox existence nor entered digits are disclosed.
    Retry,
    /// Third failure or 60-second admission deadline revoked this call.
    Closed,
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
    pub fn admit(&mut self, io: PeerIo, now_ms: u64) -> Result<PhoneSource, Error> {
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
            local: io.local().to_owned(),
            io: Some(io),
            owner: None,
            login: LoginCollector::default(),
            grammar: None,
            collection_dirty: false,
            source: source.clone(),
            signals: PeerSignals::for_phone(source.clone()),
            mailbox: None,
            attempts: 0,
            login_deadline_ms: now_ms.saturating_add(60_000),
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
    /// Original configured local binding of this exact live call, not a destination grant.
    pub fn local(&self, source: &PhoneSource, reply: &PeerReply) -> Option<&str> {
        self.calls
            .iter()
            .find(|call| {
                &call.source == source && call.signals.reply_target() == *reply && !reply.ended()
            })
            .map(|call| call.local.as_str())
    }

    /// Transfer the unique provider handle to the existing serial I/O owner.
    pub(crate) fn start(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        grammar: PhoneDtmfGrammar,
        worker: &mut PeerIoWorker,
    ) -> Result<(), Error> {
        let call = self.current(source, reply)?;
        let io = call.io.take().ok_or(Error::Admission)?;
        let (session, control) = PrivatePhoneSession::prepare(io, reply.clone())?;
        worker.attach_private(session).map_err(|_| Error::Thread)?;
        call.owner = Some(control);
        call.grammar = Some(grammar);
        Ok(())
    }
    /// Prepare the existing source ring/shared graph for an authenticated call.
    /// This is not node attachment or permission approval; callers must separately
    /// prove target policy, typed mixer ownership and RF/timing readiness.
    /// The returned generation stays inactive until its serial provider owner adopts it.
    pub fn prepare_media(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        processor: Option<PcmProcessor>,
    ) -> Result<PhoneMediaInput, Error> {
        let call = self.current(source, reply)?;
        if call.mailbox.is_none() {
            return Err(Error::Admission);
        }
        call.owner
            .as_mut()
            .ok_or(Error::Admission)?
            .prepare_media(processor)
    }
    /// Revoke only this call's prepared programme generation, preserving private control.
    pub fn retire_media(&mut self, source: &PhoneSource, reply: &PeerReply) -> Result<(), Error> {
        let call = self.current(source, reply)?;
        call.owner.as_mut().ok_or(Error::Admission)?.retire_media();
        Ok(())
    }
    /// One copied digit still belongs to the original call and ingress token.
    pub(crate) fn event(&mut self) -> Option<(PhoneSource, PeerReply, char, u64)> {
        self.calls.iter_mut().find_map(|call| {
            let (digit, now_ms) = call.owner.as_mut()?.digit()?;
            Some((
                call.source.clone(),
                call.signals.reply_target(),
                digit,
                now_ms,
            ))
        })
    }
    pub(crate) fn login_digit(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        digit: char,
        now_ms: u64,
    ) -> Result<Option<LoginInput>, Error> {
        let call = self.current(source, reply)?;
        let expired = call.login.tick(now_ms);
        Ok(call.login.digit(digit, now_ms).or(expired))
    }
    /// Login timeout clears its partial credentials privately, never replaying them.
    pub(crate) fn login_timeouts(&mut self, now_ms: u64) -> Vec<(PhoneSource, PeerReply)> {
        self.calls
            .iter_mut()
            .filter_map(|call| {
                if call.mailbox.is_none() && call.login.tick(now_ms).is_some() {
                    Some((call.source.clone(), call.signals.reply_target()))
                } else {
                    None
                }
            })
            .collect()
    }
    pub(crate) fn actions(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        digit: char,
        now_ms: u64,
    ) -> Result<Vec<PhoneDtmfAction>, Error> {
        let call = self.current(source, reply)?;
        call.collection_dirty = true;
        Ok(call
            .grammar
            .as_mut()
            .ok_or(Error::Admission)?
            .digit(digit, now_ms))
    }
    pub(crate) fn cancel_grammar(&mut self, source: &PhoneSource, reply: &PeerReply) {
        if let Ok(call) = self.current(source, reply) {
            if let Some(grammar) = &mut call.grammar {
                grammar.cancel();
                call.collection_dirty = true;
            }
        }
    }
    /// Existing node expiry supplies exact dirty tokens, including silent expiry.
    pub(crate) fn collection_expired(&mut self, reply: &PeerReply) {
        let Some(source) = reply.phone_source() else {
            return;
        };
        if let Ok(call) = self.current(source, reply) {
            call.collection_dirty = true;
        }
    }
    /// Ordinary serialized control supplies the authoritative lifecycle fence.
    /// Inspect only changed calls; a refused queue write keeps the same bounded
    /// intent. Source-side revision validation still wins a newer onset or reset.
    pub(crate) fn reconcile_collections(
        &mut self,
        mut pending: impl FnMut(&str, &PhoneSource, &PeerReply) -> Option<bool>,
    ) {
        for call in &mut self.calls {
            if !call.collection_dirty {
                continue;
            }
            call.collection_dirty = false;
            let reply = call.signals.reply_target();
            if !reply.phone_authenticated() || call.mailbox.is_none() {
                continue;
            }
            let Some(grammar) = call.grammar.as_ref() else {
                continue;
            };
            if grammar.collecting()
                || grammar
                    .selected_target()
                    .is_some_and(|target| target != call.local)
                || pending(&call.local, &call.source, &reply) != Some(false)
            {
                continue;
            }
            if let Some(owner) = call.owner.as_mut() {
                call.collection_dirty = !owner.collection_ended(&reply);
            }
        }
    }
    pub(crate) fn grammar_timeouts(
        &mut self,
        now_ms: u64,
    ) -> Vec<(PhoneSource, PeerReply, PhoneDtmfAction)> {
        let mut actions = Vec::new();
        for call in &mut self.calls {
            if call.mailbox.is_some() {
                if let Some(grammar) = &mut call.grammar {
                    let collecting = grammar.collecting();
                    let expired = grammar.tick(now_ms);
                    call.collection_dirty |= collecting && !grammar.collecting();
                    actions.extend(
                        expired.into_iter().map(|action| {
                            (call.source.clone(), call.signals.reply_target(), action)
                        }),
                    );
                }
            }
        }
        actions
    }
    /// Hand off prepared streams only while both sides retain the exact live source.
    pub(crate) fn transfer_replies(&mut self) {
        for call in &mut self.calls {
            if let Some(owner) = &mut call.owner {
                while let Some(reply) = call.replies.pop_front() {
                    if let Err(reply) = owner.send(reply) {
                        call.replies.push_front(reply);
                        break;
                    }
                }
            }
        }
    }
    fn current(&mut self, source: &PhoneSource, reply: &PeerReply) -> Result<&mut Call, Error> {
        self.calls
            .iter_mut()
            .find(|item| {
                &item.source == source && item.signals.reply_target() == *reply && !reply.ended()
            })
            .ok_or(Error::Admission)
    }
    /// Verify a configured mailbox/PIN on serialized control. Three failures or
    /// 60 seconds from admission revoke the call; new input never renews that
    /// deadline. Provider credentials and successful login grant no node unlock.
    pub fn authenticate(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        mailboxes: &[PhoneMailbox],
        mailbox: &str,
        pin: &str,
        now_ms: u64,
    ) -> Result<PhoneLogin, Error> {
        let call = self.current(source, reply)?;
        if call.mailbox.is_some() {
            return Err(Error::Admission);
        }
        if now_ms >= call.login_deadline_ms {
            call.signals.end();
            return Ok(PhoneLogin::Closed);
        }
        call.attempts += 1;
        let record = mailboxes
            .iter()
            .find(|record| record.id.as_str() == mailbox)
            .filter(|record| record.verify_pin(pin));
        let Some(record) = record else {
            if call.attempts >= 3 {
                call.signals.end();
                return Ok(PhoneLogin::Closed);
            }
            return Ok(PhoneLogin::Retry);
        };
        #[cfg(test)]
        tests::before_authenticate();
        if !call.signals.authenticate_phone() {
            return Err(Error::Admission);
        }
        call.login.clear();
        call.mailbox = Some(record.clone());
        call.collection_dirty = true;
        Ok(PhoneLogin::Authenticated)
    }
    /// The configured profile is visible only for the same authenticated call.
    pub fn mailbox(&self, source: &PhoneSource, reply: &PeerReply) -> Option<&PhoneMailbox> {
        self.calls
            .iter()
            .find(|call| {
                &call.source == source
                    && call.signals.reply_target() == *reply
                    && reply.phone_authenticated()
            })
            .and_then(|call| call.mailbox.as_ref())
    }
    /// Retire abandoned login resources without requiring another caller digit.
    pub fn expire_authentication(&mut self, now_ms: u64) -> Vec<PeerReply> {
        let mut retired = Vec::new();
        self.calls.retain(|call| {
            if call.signals.ended() || (call.mailbox.is_none() && now_ms >= call.login_deadline_ms)
            {
                call.signals.end();
                retired.push(call.signals.reply_target());
                false
            } else {
                true
            }
        });
        retired
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
pub(crate) mod tests;
