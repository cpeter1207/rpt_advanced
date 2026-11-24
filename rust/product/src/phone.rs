//! Serialized private call ownership; no radio binding, topology, or ASL session.
pub(crate) mod accounts;
mod login;
pub(crate) use login::LoginField;

use crate::{Error, services::PeerIo};
use rpt_advanced_core::{
    audio::{PcmStreamReader, Playback, PlaybackOutcome},
    config::PhoneMailbox,
    link::{MailboxId, MediaIsolationClaim, PeerReply, PeerSignals, PhoneCallId, PhoneSource},
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

/// Public mailbox identity retained by one authenticated private call.
pub struct CallerMailbox {
    /// Exact declaring resource of a modular scoped login.
    pub(crate) binding: Option<rpt_advanced_core::config::resources::candidate::ResourceId>,
    /// Exact configured number, including leading zeros.
    pub id: MailboxId,
    /// Configured first name.
    pub first_name: String,
    /// Configured callsign.
    pub callsign: String,
}

struct Call {
    local: String,
    io: Option<PeerIo>,
    transport: Option<crate::link::private_phone::Control>,
    login: login::Collector,
    attached:bool,
    _isolation: Option<MediaIsolationClaim>,
    target:Option<String>,
    source: PhoneSource,
    signals: std::sync::Arc<PeerSignals>,
    mailbox: Option<CallerMailbox>,
    attempts: u8,
    login_deadline_ms: u64,
    replies: VecDeque<PrivatePhoneReply>,
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
        self.admit_call(io, now_ms, false, None)
    }
    /// Adopt an answered leg with media only and retain its exact isolation claim.
    pub(crate) fn admit_outgoing(&mut self, io: PeerIo, now_ms: u64, isolation: Option<MediaIsolationClaim>)
        -> Result<PhoneSource, Error> {
        self.admit_call(io, now_ms, true, isolation)
    }
    fn admit_call(&mut self, io: PeerIo, now_ms: u64, outgoing: bool, isolation: Option<MediaIsolationClaim>)
        -> Result<PhoneSource, Error> {
        let call = io.identity().call_id().ok_or(Error::Admission)?;
        let account = io.phone_account().ok_or(Error::Admission)?.clone();
        let (account_generation, call_generation) = io.generations();
        if self.calls.len() == self.maximum_calls
            || self
                .calls
                .iter()
                .any(|existing| existing.source.call_id() == call
                    || (existing.source.account() == &account
                        && existing.source.account_generation() == account_generation
                        && existing.source.call_generation() == call_generation))
        {
            return Err(Error::Admission);
        }
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
            transport: None,
            login: login::Collector::default(),
            attached:outgoing,
            _isolation: isolation,
            target:None,
            source: source.clone(),
            signals: std::sync::Arc::new(if outgoing { PeerSignals::for_outgoing_phone(source.clone()) }
                else { PeerSignals::for_phone(source.clone()) }),
            mailbox: None,
            attempts: 0,
            login_deadline_ms: now_ms.saturating_add(60_000),
            replies,
        });
        Ok(source)
    }
    pub(crate) fn start_transport(&mut self, source: &PhoneSource, reply: &PeerReply,
        worker: &mut crate::link::session::PeerIoWorker) -> Result<(), Error> {
        let capacity = self.maximum_replies;
        let call = self.current(source, reply)?;
        let io = call.io.take().ok_or(Error::Admission)?;
        call.transport = Some(worker.attach_private(io, reply.clone(), capacity)?);
        Ok(())
    }

    pub(crate) fn attach_radio(&mut self,source:&PhoneSource,reply:&PeerReply)->Result<bool,Error>{
        let call=self.current(source,reply)?;
        if call.transport.is_none() || call.mailbox.is_none() || !reply.phone_authenticated(){return Err(Error::Admission);}
        if call.attached{return Ok(false);}
        call.attached=true;
        Ok(true)
    }
    pub(crate) fn detach_radio(&mut self,source:&PhoneSource,reply:&PeerReply){
        if let Ok(call)=self.current(source,reply){call.attached=false;}
    }
    pub(crate) fn select_target(&mut self,source:&PhoneSource,reply:&PeerReply,target:Option<String>)->Result<(),Error>{
        let call=self.current(source,reply)?;
        if !reply.phone_authenticated(){return Err(Error::Admission);}
        call.target=target;
        Ok(())
    }
    pub(crate) fn targets(&mut self,source:&PhoneSource,reply:&PeerReply,target:&str)->bool{
        self.current(source,reply).is_ok_and(|call|call.target.as_deref()==Some(target))
    }
    pub(crate) fn audio_routes(&self,local:&str)->impl Iterator<Item=(&PhoneSource,&std::sync::Arc<PeerSignals>,&crate::link::private_phone::Control)> {
        self.calls.iter().filter_map(move|call|{
            let reply=call.signals.reply_target();
            if call.local!=local || !reply.phone_media_allowed() || !call.attached{return None;}
            Some((&call.source,&call.signals,call.transport.as_ref()?))
        })
    }

    pub(crate) fn settle_inputs(&self, mut idle: impl FnMut(&str, &PeerReply) -> bool) {
        for call in &self.calls {
            let reply = call.signals.reply_target();
            if reply.phone_authenticated() && idle(&call.local, &reply) {
                if let Some(transport) = &call.transport { transport.collection_ended(); }
            }
        }
    }

    pub(crate) fn poll_inputs(&mut self) -> Vec<(String, PhoneSource, PeerReply, char, u64)> {
        let mut inputs = Vec::new();
        for call in &mut self.calls {
            let Some(transport) = &mut call.transport else { continue };
            let reply = call.signals.reply_target();
            if reply.ended() || reply.outgoing_phone() { continue; }
            // Capture each bounded queue length once so newer symbols cannot
            // starve another caller or move its inactivity deadline.
            for _ in 0..transport.pending_digits() {
                if let Some((symbol, received)) = transport.digit() {
                    inputs.push((call.local.clone(), call.source.clone(), reply.clone(), symbol, received));
                }
            }
        }
        inputs
    }

    pub(crate) fn login_digit(&mut self, source: &PhoneSource, reply: &PeerReply,
        symbol: char, at: u64, timeout: u64) -> Result<Option<LoginField>, Error> {
        let call = self.current(source, reply)?;
        if call.mailbox.is_some() || at >= call.login_deadline_ms { return Err(Error::Admission); }
        if call.login.expired(at, timeout) {
            let _ = call.login.digit(symbol, at);
            return Ok(Some(LoginField::Expired));
        }
        Ok(call.login.digit(symbol, at))
    }

    /// Advance partial mailbox/PIN fields on the owning port clock, including the
    /// pause between the mailbox terminator and the first PIN digit.
    pub(crate) fn expired_login_fields(&mut self, now:u64, mut timeout:impl FnMut(&str)->Option<u64>)
        ->Vec<(PhoneSource,PeerReply)>{
        let mut expired=Vec::new();
        for call in &mut self.calls{
            let reply=call.signals.reply_target();
            if call.mailbox.is_none() && !reply.outgoing_phone() && !reply.ended() && now<call.login_deadline_ms
                && timeout(&call.local).is_some_and(|ms|call.login.expired(now,ms)) {
                expired.push((call.source.clone(),reply));
            }
        }
        expired
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
        self.authenticate_with(source, reply, now_ms, || {
            let record = mailboxes
                .iter()
                .find(|record| record.id.as_str() == mailbox)?;
            record.verify_pin(pin).then(|| CallerMailbox {
                binding: None,
                id: record.id.clone(),
                first_name: record.first_name.clone(),
                callsign: record.callsign.clone(),
            })
        })
    }

    /// Verify against the effective whole mailbox and its declaring private slot.
    /// The borrowed verifier is the current generation's authenticated credential owner.
    #[cfg(target_os = "linux")]
    pub(crate) fn authenticate_scoped<'s>(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        mailboxes: &rpt_advanced_core::config::resources::profiles::mailbox::ScopedMailboxes,
        selected_pin: impl FnOnce(
            rpt_advanced_core::config::resources::candidate::ResourceId,
        )
            -> Option<&'s rptadv_peer_support::sealed_records::SecretBytes>,
        (number, pin): (&str, &str),
        now_ms: u64,
    ) -> Result<PhoneLogin, Error> {
        use rpt_advanced_core::config::verify_mailbox_pin;
        self.authenticate_with(source, reply, now_ms, || {
            let record = mailboxes.effective.get(number)?;
            let secret = selected_pin(record.binding.id)?;
            let phc = std::str::from_utf8(secret.expose_bytes()).ok()?;
            verify_mailbox_pin(phc, pin).then(|| CallerMailbox {
                binding: Some(record.binding.id),
                id: record.number.clone(),
                first_name: record.first_name.clone(),
                callsign: record.callsign.clone(),
            })
        })
    }

    fn authenticate_with(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        now_ms: u64,
        verify: impl FnOnce() -> Option<CallerMailbox>,
    ) -> Result<PhoneLogin, Error> {
        let call = self.current(source, reply)?;
        if reply.outgoing_phone() || call.mailbox.is_some() {
            return Err(Error::Admission);
        }
        if now_ms >= call.login_deadline_ms {
            call.signals.end();
            return Ok(PhoneLogin::Closed);
        }
        call.attempts += 1;
        let Some(record) = verify() else {
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
        call.mailbox = Some(record);
        Ok(PhoneLogin::Authenticated)
    }
    /// The configured profile is visible only for the same authenticated call.
    pub fn mailbox(&self, source: &PhoneSource, reply: &PeerReply) -> Option<&CallerMailbox> {
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
            if call.signals.ended() || (!call.signals.reply_target().outgoing_phone()
                && call.mailbox.is_none() && now_ms >= call.login_deadline_ms)
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
        if reply.outgoing_phone() { return Err(Error::Admission); }
        let call = self.current(source, reply)?;
        if call.replies.len() == maximum {
            return Err(Error::Allocation);
        }
        let prepared = PrivatePhoneReply {
            source: source.clone(),
            reply: reply.clone(),
            playback: Playback::new_stream(stream),
        };
        if let Some(transport) = &mut call.transport { return transport.send(prepared); }
        call.replies.push_back(prepared);
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
    /// Close only the actual retired call. Each exact PeerReply is revoked
    /// before the transport, media queue or mailbox can be reclaimed.
    pub(crate) fn retain_current(
        &mut self,
        mut keep: impl FnMut(&str, &PhoneSource, Option<&CallerMailbox>) -> bool,
    ) -> Vec<PeerReply> {
        let mut retired = Vec::new();
        self.calls.retain(|call| {
            if keep(call.local.as_str(), &call.source, call.mailbox.as_ref()) { return true; }
            call.signals.end();
            #[cfg(test)]
            if crate::fixture::WATCH_PHONE_PCM.load(std::sync::atomic::Ordering::Relaxed)
                && call.mailbox.is_some() && call.signals.ended() {
                eprintln!("private PHONE retirement: scoped mailbox call ended");
            }
            retired.push(call.signals.reply_target());
            false
        });
        retired
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
