//! Serialized private call ownership; no radio binding, topology, or ASL session.
pub(crate) mod accounts;
mod login;
pub(crate) use login::LoginField;

use crate::{Error, link::private_phone::Digit, services::PeerIo};
use rpt_advanced_core::{
    audio::{PcmStreamReader, Playback, PlaybackOutcome},
    link::{MailboxId, MediaIsolationClaim, PeerReply, PeerSignals, PhoneCallId, PhoneSource},
    messages::PhoneAuthenticationPrompt,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
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
    login_prompt: Option<Arc<LoginPrompt>>,
    auth_success_revision: Option<u64>,
}
impl PrivatePhoneReply {
    pub(crate) fn tracks_login_prompt(&self) -> bool {
        self.login_prompt.is_some()
    }

    pub(crate) fn obsolete_before_output(&self, revision: Option<u64>) -> bool {
        let prepared = self
            .login_prompt
            .as_ref()
            .map(|prompt| prompt.revision)
            .or(self.auth_success_revision);
        !self.playback.has_output() && prepared.is_some_and(|origin| Some(origin) != revision)
    }

    pub(crate) fn played_login_prompt(&self, at: u64, revision: Option<u64>) {
        if let Some(prompt) = &self.login_prompt
            && Some(prompt.revision) == revision
            && self.playback.has_output()
        {
            prompt.played.store(at.saturating_add(1), Ordering::Release);
        }
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

/// An exact prompt's completion in Engine milliseconds. Zero means it has
/// not finished sending. A detached receipt can never arm a later field.
struct LoginPrompt {
    revision: u64,
    played: AtomicU64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attachment {
    Detached,
    Publishing,
    Attached,
}

struct Call {
    local: String,
    io: Option<PeerIo>,
    transport: Option<crate::link::private_phone::Control>,
    login: login::Collector,
    login_revision: u64,
    login_prompt: Option<Arc<LoginPrompt>>,
    attachment: Attachment,
    _isolation: Option<MediaIsolationClaim>,
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
impl Call {
    fn finish_login_prompt(&mut self, now: u64) {
        if let Some(at) = self
            .login_prompt
            .as_ref()
            .filter(|prompt| prompt.revision == self.login_revision)
            .and_then(|prompt| prompt.played.load(Ordering::Acquire).checked_sub(1))
            .filter(|at| *at <= now)
        {
            self.login_prompt = None;
            self.login.prompt_played(at);
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        self.signals.end();
    }
}

/// Private calls owned by station control, separate from radio peer slots.
pub struct PhoneCalls {
    calls: Vec<Call>,
    maximum_replies: usize,
}
impl PhoneCalls {
    /// Reserve initial call storage and bound each call's reply queue.
    pub fn new(initial_reservation: usize, maximum_replies: usize) -> Result<Self, Error> {
        if initial_reservation == 0 || maximum_replies == 0 {
            return Err(Error::Admission);
        }
        let mut calls = Vec::new();
        calls
            .try_reserve_exact(initial_reservation)
            .map_err(|_| Error::Allocation)?;
        Ok(Self {
            calls,
            maximum_replies,
        })
    }
    /// Adopt a validated private handle without starting any ASL/radio media session.
    /// A duplicate is rejected rather than replacing another live call implicitly.
    pub fn admit(&mut self, io: PeerIo, now_ms: u64) -> Result<PhoneSource, Error> {
        self.admit_call(io, now_ms, false, None)
    }
    /// Adopt an answered leg with media only and retain its exact isolation claim.
    pub(crate) fn admit_outgoing(
        &mut self,
        io: PeerIo,
        now_ms: u64,
        isolation: Option<MediaIsolationClaim>,
    ) -> Result<PhoneSource, Error> {
        self.admit_call(io, now_ms, true, isolation)
    }
    fn admit_call(
        &mut self,
        io: PeerIo,
        now_ms: u64,
        outgoing: bool,
        isolation: Option<MediaIsolationClaim>,
    ) -> Result<PhoneSource, Error> {
        let call = io.identity().call_id().ok_or(Error::Admission)?;
        let account = io.phone_account().ok_or(Error::Admission)?.clone();
        let (account_generation, call_generation) = io.generations();
        if self.calls.iter().any(|existing| {
            existing.source.call_id() == call
                || (existing.source.account() == &account
                    && existing.source.account_generation() == account_generation
                    && existing.source.call_generation() == call_generation)
        }) {
            return Err(Error::Admission);
        }
        self.calls.try_reserve(1).map_err(|_| Error::Allocation)?;
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
            login_revision: 0,
            login_prompt: None,
            attachment: if outgoing {
                Attachment::Attached
            } else {
                Attachment::Detached
            },
            _isolation: isolation,
            source: source.clone(),
            signals: std::sync::Arc::new(if outgoing {
                PeerSignals::for_outgoing_phone(source.clone())
            } else {
                PeerSignals::for_reverse_phone(source.clone())
            }),
            mailbox: None,
            attempts: 0,
            login_deadline_ms: now_ms.saturating_add(60_000),
            replies,
        });
        Ok(source)
    }
    pub(crate) fn start_transport(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        worker: &mut crate::link::session::PeerIoWorker,
    ) -> Result<(), Error> {
        let capacity = self.maximum_replies;
        let call = self.current(source, reply)?;
        let io = call.io.take().ok_or(Error::Admission)?;
        call.transport = Some(worker.attach_private(io, reply.clone(), capacity)?);
        Ok(())
    }

    /// Queue the authenticated call for publication on its account's port.
    pub(crate) fn attach_radio(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
    ) -> Result<(), Error> {
        let call = self.current(source, reply)?;
        if call.transport.is_none() || call.mailbox.is_none() || !reply.phone_authenticated() {
            return Err(Error::Admission);
        }
        if call.attachment == Attachment::Detached {
            call.attachment = Attachment::Publishing;
        }
        Ok(())
    }

    pub(crate) fn pending_radio_attachments(&self) -> BTreeMap<String, BTreeSet<usize>> {
        let mut pending = BTreeMap::<String, BTreeSet<usize>>::new();
        for call in &self.calls {
            if call.attachment == Attachment::Publishing {
                let reply = call.signals.reply_target();
                if !reply.ended() {
                    pending
                        .entry(call.local.clone())
                        .or_default()
                        .insert(reply.identity());
                }
            }
        }
        pending
    }

    pub(crate) fn confirm_radio_attachment(&mut self, reply: &PeerReply) {
        if let Some(source) = reply.phone_source()
            && let Ok(call) = self.current(source, reply)
            && call.attachment == Attachment::Publishing
        {
            call.attachment = Attachment::Attached;
        }
    }

    pub(crate) fn fail_radio_attachments(
        &mut self,
        local: &str,
        missing: &BTreeSet<usize>,
    ) -> Vec<(PhoneSource, PeerReply)> {
        let mut failed = Vec::new();
        for call in &mut self.calls {
            if call.local == local
                && call.attachment == Attachment::Publishing
                && missing.contains(&call.signals.reply_target().identity())
            {
                call.attachment = Attachment::Detached;
                failed.push((call.source.clone(), call.signals.reply_target()));
            }
        }
        failed
    }
    pub(crate) fn audio_routes(
        &self,
        local: &str,
    ) -> impl Iterator<
        Item = (
            &PhoneSource,
            &std::sync::Arc<PeerSignals>,
            &crate::link::private_phone::Control,
        ),
    > {
        self.calls.iter().filter_map(move |call| {
            let reply = call.signals.reply_target();
            if call.local != local
                || !reply.phone_media_allowed()
                || call.attachment == Attachment::Detached
            {
                return None;
            }
            Some((&call.source, &call.signals, call.transport.as_ref()?))
        })
    }

    pub(crate) fn settle_inputs(&self, mut idle: impl FnMut(&str, &PeerReply) -> bool) {
        for call in &self.calls {
            let reply = call.signals.reply_target();
            if reply.phone_authenticated() && idle(&call.local, &reply) {
                if let Some(transport) = &call.transport {
                    transport.collection_ended();
                }
            }
        }
    }

    pub(crate) fn poll_inputs(&mut self) -> Vec<(String, PhoneSource, PeerReply, Digit)> {
        let mut inputs = Vec::new();
        for call in &mut self.calls {
            let Some(transport) = &mut call.transport else {
                continue;
            };
            let reply = call.signals.reply_target();
            if reply.ended() || reply.outgoing_phone() {
                continue;
            }
            // Capture each bounded queue length once so newer symbols cannot
            // starve another caller or move its inactivity deadline.
            for _ in 0..transport.pending_digits() {
                if let Some(digit) = transport.digit() {
                    inputs.push((
                        call.local.clone(),
                        call.source.clone(),
                        reply.clone(),
                        digit,
                    ));
                }
            }
        }
        inputs
    }

    pub(crate) fn login_digit(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        digit: Digit,
        timeout: u64,
    ) -> Result<Option<LoginField>, Error> {
        let call = self.current(source, reply)?;
        let Digit {
            symbol,
            received,
            revision,
        } = digit;
        if call.mailbox.is_some()
            || received >= call.login_deadline_ms
            || revision <= call.login_revision
        {
            return Err(Error::Admission);
        }
        call.finish_login_prompt(received);
        call.login_revision = revision;
        call.login_prompt = None;
        if call.login.expired(received, timeout) {
            let _ = call.login.digit(symbol, received);
            return Ok(Some(LoginField::Expired));
        }
        Ok(call.login.digit(symbol, received))
    }

    /// Advance partial mailbox/PIN fields on the owning port clock, including the
    /// pause between the mailbox terminator and the first PIN digit.
    pub(crate) fn expired_login_fields(
        &mut self,
        now: u64,
        mut timeout: impl FnMut(&str) -> Option<u64>,
    ) -> Vec<(PhoneSource, PeerReply)> {
        let mut expired = Vec::new();
        for call in &mut self.calls {
            let reply = call.signals.reply_target();
            if call.mailbox.is_none()
                && !reply.outgoing_phone()
                && !reply.ended()
                && now < call.login_deadline_ms
            {
                call.finish_login_prompt(now);
                if timeout(&call.local).is_some_and(|ms| call.login.expired(now, ms)) {
                    expired.push((call.source.clone(), reply));
                }
            }
        }
        expired
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn outgoing_sources(&self, local: &str) -> impl Iterator<Item = &PhoneSource> {
        self.calls.iter().filter_map(move |call| {
            let reply = call.signals.reply_target();
            (call.local == local && reply.outgoing_phone() && !reply.ended())
                .then_some(&call.source)
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn observations(
        &self,
    ) -> impl Iterator<Item = (&str, crate::management::live::Call)> {
        self.calls.iter().filter_map(|call| {
            let reply = call.signals.reply_target();
            if reply.ended() {
                return None;
            }
            let muted = reply.rx_muted();
            let attached = call.attachment == Attachment::Attached;
            Some((
                call.local.as_str(),
                crate::management::live::Call {
                    call_id: call.source.call_id().to_string(),
                    account: call.source.account().as_str().to_owned(),
                    generation: call.source.call_generation(),
                    direction: if reply.outgoing_phone() {
                        "outgoing"
                    } else {
                        "incoming"
                    },
                    phase: if attached {
                        "attached"
                    } else if reply.phone_authenticated() {
                        "authenticated"
                    } else {
                        "private"
                    },
                    attached,
                    muted: attached.then_some(muted),
                    active: attached.then_some(!muted && reply.active()),
                },
            ))
        })
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
    /// Verify against the effective whole mailbox and its declaring private slot.
    /// The borrowed verifier selects the current permitted record and its authenticated PIN.
    #[cfg(target_os = "linux")]
    pub(crate) fn authenticate_scoped<'s>(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        mailboxes: &rpt_advanced_core::config::resources::profiles::mailbox::ScopedMailboxes,
        selected_pin: impl FnOnce(
            &rpt_advanced_core::config::resources::profiles::mailbox::PublicMailbox,
        )
            -> Option<&'s rptadv_peer_support::sealed_records::SecretBytes>,
        (number, pin): (&str, &str),
        now_ms: u64,
    ) -> Result<PhoneLogin, Error> {
        use rpt_advanced_core::config::verify_mailbox_pin;
        self.authenticate_with(source, reply, now_ms, || {
            let record = mailboxes.effective.get(number)?;
            let secret = selected_pin(record)?;
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
        call.login_prompt = None;
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
            if call.signals.ended()
                || (!call.signals.reply_target().outgoing_phone()
                    && call.mailbox.is_none()
                    && now_ms >= call.login_deadline_ms)
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
        self.queue_prepared_reply(source, reply, stream, None)
    }

    pub(crate) fn queue_login_prompt(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        stream: Box<dyn PcmStreamReader>,
        prompt: PhoneAuthenticationPrompt,
    ) -> Result<(), Error> {
        self.queue_prepared_reply(source, reply, stream, Some(prompt))
    }

    fn queue_prepared_reply(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        stream: Box<dyn PcmStreamReader>,
        prompt: Option<PhoneAuthenticationPrompt>,
    ) -> Result<(), Error> {
        use PhoneAuthenticationPrompt::{Authenticated, Login, Password, Retry};

        let maximum = self.maximum_replies;
        if reply.outgoing_phone() {
            return Err(Error::Admission);
        }
        let call = self.current(source, reply)?;
        let login_prompt = if let Some(prompt @ (Login | Password | Retry)) = prompt {
            if !call.login.waiting_for_prompt(prompt) {
                return Ok(());
            }
            Some(Arc::new(LoginPrompt {
                revision: call.login_revision,
                played: AtomicU64::new(0),
            }))
        } else {
            None
        };
        if call.replies.len() == maximum {
            return Err(Error::Allocation);
        }
        let prepared = PrivatePhoneReply {
            source: source.clone(),
            reply: reply.clone(),
            playback: Playback::new_stream(stream),
            login_prompt: login_prompt.clone(),
            auth_success_revision: (prompt == Some(Authenticated)).then_some(call.login_revision),
        };
        if let Some(transport) = &mut call.transport {
            transport.send(prepared)?;
        } else {
            call.replies.push_back(prepared);
        }
        if let Some(prompt) = login_prompt {
            // An unsuccessful queue request leaves the active timer unchanged.
            call.login.await_prompt();
            call.login_prompt = Some(prompt);
        }
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
            if keep(call.local.as_str(), &call.source, call.mailbox.as_ref()) {
                return true;
            }
            call.signals.end();
            #[cfg(test)]
            if crate::fixture::WATCH_PHONE_PCM.load(std::sync::atomic::Ordering::Relaxed)
                && call.mailbox.is_some()
                && call.signals.ended()
            {
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
