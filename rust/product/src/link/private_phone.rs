//! Private phone I/O on the existing serial peer owner, independent of RF.
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
        SourceTimeClock, SourceTimeReference,
    },
};
use rpt_advanced_core::{
    audio::{AudioSpan, LinkAudioConsumer, LinkAudioProducer, LinkAudioQueue, PlaybackOutcome},
    link::PeerReply,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    cell::Cell,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Instant,
};

#[path = "phone_media.rs"]
pub(crate) mod media;

const BLOCK: usize = 960;

/// Coherent identity of one source request, never a digit or a media generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryTicket {
    revision: NonZeroU32,
    cancellation_epoch: NonZeroU32,
}
impl RecoveryTicket {
    fn packed(self) -> u64 {
        (u64::from(self.cancellation_epoch.get()) << 32) | u64::from(self.revision.get())
    }
    fn unpack(value: u64) -> Option<Self> {
        Some(Self {
            revision: NonZeroU32::new(value as u32)?,
            cancellation_epoch: NonZeroU32::new((value >> 32) as u32)?,
        })
    }
}

/// Setup-allocated source publication; control alone advances cancellation.
pub(super) struct SourceReady {
    revision: AtomicU32,
    request: AtomicU64,
    cancellation_epoch: AtomicU32,
    attached: AtomicBool,
}
impl SourceReady {
    fn new() -> Self {
        Self {
            revision: AtomicU32::new(0),
            request: AtomicU64::new(0),
            cancellation_epoch: AtomicU32::new(1),
            attached: AtomicBool::new(false),
        }
    }
    fn epoch(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.cancellation_epoch.load(Ordering::Acquire))
    }
    fn invalidate(&self) {
        // One control writer; zero is permanent exhaustion, never an ABA epoch.
        let next = self
            .epoch()
            .and_then(|e| e.get().checked_add(1))
            .unwrap_or(0);
        self.cancellation_epoch.store(next, Ordering::Release);
        self.request.store(0, Ordering::Release);
        // Existing settled digit readiness remains valid for ordinary replacement.
        // A recovery revision cannot authorize it after its exact request is erased.
    }
}

/// Captured at preparation, not recaptured when an old origin is cut.
pub(super) struct MediaOrigin {
    revision: NonZeroU32,
    epoch: NonZeroU32,
    source: Arc<SourceReady>,
}
impl MediaOrigin {
    fn current(&self) -> bool {
        self.source.epoch() == Some(self.epoch)
    }
}

struct RecoveryCut {
    ticket: RecoveryTicket,
    reference: SourceTimeReference,
    start: u64,
    count: usize,
    fresh: bool,
    approved: bool,
}

/// Private revision only: typed call/account/actual-ingress fences stay independent.
/// None is terminal exhaustion, never a wrapping generation.
struct CollectionRevision(Cell<Option<NonZeroU32>>);
impl CollectionRevision {
    fn new() -> Self {
        Self(Cell::new(NonZeroU32::new(1)))
    }
    fn advance(&self) -> Option<NonZeroU32> {
        let next = self
            .0
            .get()
            .and_then(|current| current.get().checked_add(1))
            .and_then(NonZeroU32::new);
        self.0.set(next);
        next
    }
    fn emit(&self, digits: &mut Producer<PrivateDigit>, symbol: char, received_ms: u64) {
        if let Some(revision) = self.advance() {
            digits
                .push(PrivateDigit {
                    received_ms,
                    revision,
                    symbol,
                })
                .unwrap_or_else(|_| unreachable!("reserved private digit slot"));
        }
    }
}
struct PrivateDigit {
    received_ms: u64,
    revision: NonZeroU32,
    symbol: char,
}

enum PrivateCommand {
    TelephoneDigit(char),
    Reply(PrivatePhoneReply),
    Media(Box<media::Ingress>),
    CollectionEnded(NonZeroU32),
    Recover(RecoveryTicket),
}

/// Unpublished source endpoint owned by its graph candidate, not the call queue.
pub(crate) struct PreparedPhoneMedia(Box<media::Ingress>);

/// Control endpoint retains the exact source's revocation fence, never a radio handle.
pub(crate) struct PrivatePhoneControl {
    replies: Producer<PrivateCommand>,
    digits: Consumer<PrivateDigit>,
    consumed_revision: Option<NonZeroU32>,
    approved_recovery: Option<RecoveryTicket>,
    reply: PeerReply,
    wake: PeerWake,
    stop: Arc<AtomicBool>,
    source_ready: Arc<SourceReady>,
    rate: u32,
    source_media: bool,
    digit_mode: PhoneDigitMode,
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
    /// Only explicit initiating-side control can queue a symbol to this exact call.
    pub(crate) fn send_digit(&mut self, reply: &PeerReply, symbol: char) -> Result<(), Error> {
        if self.reply != *reply
            || !reply.phone_outgoing()
            || !reply.phone_media_authorized()
            || self.stop.load(Ordering::Acquire)
            || !"0123456789*#ABCD".contains(symbol)
        {
            return Err(Error::Admission);
        }
        if self.replies.slots() == 0 {
            return Err(Error::Allocation);
        }
        let command = match self.digit_mode {
            PhoneDigitMode::NativeEvents => PrivateCommand::TelephoneDigit(symbol),
            PhoneDigitMode::InBand => {
                PrivateCommand::Reply(PrivatePhoneReply::telephone_digit(reply, symbol)?)
            }
        };
        self.replies
            .push(command)
            .unwrap_or_else(|_| unreachable!("reserved private digit slot"));
        self.wake.notify();
        Ok(())
    }
    /// Prepare and immediately queue the existing direct media API.
    pub(crate) fn prepare_media(
        &mut self,
        processor: Option<PcmProcessor>,
    ) -> Result<media::PhoneMediaInput, Error> {
        if self.replies.slots() == 0 {
            return Err(Error::Allocation);
        }
        let (pending, input) = self.prepare_pending_media(processor)?;
        self.adopt_media(pending).map_err(|_| Error::Allocation)?;
        Ok(input)
    }
    /// Graph preparation retains the ingress itself until publication succeeds.
    pub(crate) fn prepare_pending_media(
        &mut self,
        processor: Option<PcmProcessor>,
    ) -> Result<(PreparedPhoneMedia, media::PhoneMediaInput), Error> {
        self.prepare_media_candidate(processor, None)
    }
    pub(crate) fn prepare_pending_voiced_media(
        &mut self,
        processor: Option<PcmProcessor>,
        settings: rpt_advanced_core::config::PhoneSpeechSettings,
    ) -> Result<(PreparedPhoneMedia, media::PhoneMediaInput), Error> {
        self.prepare_media_candidate(processor, Some(settings))
    }
    fn prepare_media_candidate(
        &mut self,
        processor: Option<PcmProcessor>,
        speech: Option<rpt_advanced_core::config::PhoneSpeechSettings>,
    ) -> Result<(PreparedPhoneMedia, media::PhoneMediaInput), Error> {
        if !self.media_capable() {
            return Err(Error::Admission);
        }
        if self.media_occupied() {
            return Err(Error::Reservation);
        }
        // An outgoing call has no command collector. Its serial source owner
        // publishes the exact settled cut, including resets without a digit.
        // Initial preparation remains inert until that owner validates it.
        let revision = if self.reply.phone_outgoing() {
            NonZeroU32::new(self.source_ready.revision.load(Ordering::Acquire))
        } else {
            self.acknowledged_recovery()
                .map(|t| t.revision)
                .or(self.consumed_revision)
        }
        .unwrap_or(NonZeroU32::MIN);
        let origin = MediaOrigin {
            revision,
            epoch: self.source_ready.epoch().ok_or(Error::Admission)?,
            source: Arc::clone(&self.source_ready),
        };
        let (ingress, input, shared) = match speech {
            Some(settings) => {
                media::prepare_voiced(self.reply.clone(), self.rate, origin, processor, settings)
            }
            None => media::prepare(self.reply.clone(), self.rate, origin, processor),
        }?;
        // Successful preparation supersedes the old destination before its new
        // box reaches the serial queue. Failed preparation above preserves it.
        if let Some(previous) = self.media.replace(shared) {
            previous.retire();
        }
        Ok((PreparedPhoneMedia(ingress), input))
    }
    /// Return the same prepared endpoint when the bounded owner queue is full.
    pub(crate) fn adopt_media(
        &mut self,
        media: PreparedPhoneMedia,
    ) -> Result<(), PreparedPhoneMedia> {
        if !media.0.prepared() || !self.media_capable() {
            return Ok(());
        }
        if self.replies.slots() == 0 {
            return Err(media);
        }
        self.replies
            .push(PrivateCommand::Media(media.0))
            .unwrap_or_else(|_| unreachable!("reserved private command slot"));
        self.wake.notify();
        Ok(())
    }
    /// Existing serial wake and source qualification, without copying its handle.
    pub(crate) fn media_capable(&self) -> bool {
        self.source_media
            && self.reply.phone_media_authorized()
            && !self.stop.load(Ordering::Acquire)
    }
    /// Rearm only after the serial owner has acknowledged this exact source cut.
    pub(crate) fn media_rearm_ready(&self) -> bool {
        let ready = self.source_ready.revision.load(Ordering::Acquire);
        if self.reply.phone_outgoing() {
            ready != 0
        } else {
            self.acknowledged_recovery().is_some()
                || ready == self.consumed_revision.unwrap_or(NonZeroU32::MIN).get()
        }
    }
    pub(crate) fn media_occupied(&self) -> bool {
        self.media.as_ref().is_some_and(|media| media.occupied())
    }
    pub(crate) fn media_wake(&self) -> PeerWake {
        self.wake.clone()
    }
    pub(crate) fn retire_media(&mut self) {
        self.source_ready.invalidate();
        self.approved_recovery = None;
        self.retire_generation();
    }
    fn retire_generation(&mut self) {
        if let Some(media) = self.media.take() {
            media.retire();
        }
        self.wake.notify();
    }
    pub(crate) fn set_attached(&self, attached: bool) {
        self.source_ready
            .attached
            .store(attached, Ordering::Release);
    }
    pub(crate) fn recovery_request(&self) -> Option<RecoveryTicket> {
        let ticket = RecoveryTicket::unpack(self.source_ready.request.load(Ordering::Acquire))?;
        (self.media_capable()
            && !self.reply.phone_outgoing()
            && self.source_ready.attached.load(Ordering::Acquire)
            && self.source_ready.epoch() == Some(ticket.cancellation_epoch))
        .then_some(ticket)
    }
    pub(crate) fn approve_recovery(&mut self, reply: &PeerReply, ticket: RecoveryTicket) -> bool {
        if self.reply != *reply || self.recovery_request() != Some(ticket) {
            return false;
        }
        if self.approved_recovery == Some(ticket) {
            return true;
        }
        if self.replies.push(PrivateCommand::Recover(ticket)).is_err() {
            return false;
        }
        self.approved_recovery = Some(ticket);
        self.wake.notify();
        true
    }
    fn acknowledged_recovery(&self) -> Option<RecoveryTicket> {
        self.approved_recovery.filter(|ticket| {
            self.recovery_request() == Some(*ticket)
                && self.source_ready.revision.load(Ordering::Acquire) == ticket.revision.get()
        })
    }
    pub(crate) fn retire_for_graph_replacement(&mut self) {
        if self.acknowledged_recovery().is_some() {
            self.retire_generation();
        } else {
            self.retire_media();
        }
    }
    pub(crate) fn digit(&mut self) -> Option<(char, u64)> {
        let event = self.digits.pop().ok()?;
        self.consumed_revision = Some(event.revision);
        Some((event.symbol, event.received_ms))
    }
    /// Called after incoming collectors are idle, or a callee digit has no command sink.
    /// Refusal retains the same consumed revision; no new source activity is inferred.
    pub(crate) fn collection_ended(&mut self, reply: &PeerReply) -> bool {
        if self.reply != *reply || !reply.phone_media_authorized() {
            return false;
        }
        let Some(revision) = self.consumed_revision.filter(|_| self.source_media) else {
            return true;
        };
        if self
            .replies
            .push(PrivateCommand::CollectionEnded(revision))
            .is_err()
        {
            return false;
        }
        self.wake.notify();
        true
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
    source_ready: Arc<SourceReady>,
    wake: PeerWake,
    epoch: Option<Instant>,
    source_clock: Option<SourceTimeClock>,
    source_completion: SourceCompletion,
    digit_mode: PhoneDigitMode,
    timed_loss: bool,
    // A 20 ms offer needs at most three 8 ms partitions at either released rate.
    source: Option<SourceIngress<4>>,
    replies: Consumer<PrivateCommand>,
    digits: Producer<PrivateDigit>,
    revision: CollectionRevision,
    settled_revision: NonZeroU32,
    pending_release: Option<NonZeroU32>,
    recovery: Option<RecoveryCut>,
    pending_recovery: Option<RecoveryTicket>,
    cut_pending: bool,
    current: Option<PrivatePhoneReply>,
    media: Option<Box<media::Ingress>>,
    source_end: u64,
    output: LinkAudioProducer,
    outbound: Outbound,
    spare_output: Option<(LinkAudioConsumer, u64)>,
    programme: bool,
    source_position: u64,
    // Inactive cadence anchors at the next actual rendered samples.
    next_audio_ns: u64,
    audio_cadence_active: bool,
    pcm: [f32; BLOCK],
}
impl PrivatePhoneSession {
    /// Prepare bounded private media/control without radio binding or ASL negotiation.
    pub(crate) fn prepare(
        io: PeerIo,
        reply: PeerReply,
        protocol: rpt_advanced_core::config::PhoneAccountProtocol,
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
        let transmit_digit_mode = io.phone_transmit_digit_mode().ok_or(Error::Admission)?;
        let rate = io.rate();
        let source = if io.source_media() {
            let mut source = SourceIngress::<4>::at_rate(true, rate, rate as usize / 50)?;
            source.require_control_release();
            Some(source)
        } else {
            None
        };
        let (output, input) = LinkAudioQueue::new(BLOCK, 1)
            .expect("fixed nonzero private block and span capacities")
            .into_endpoints();
        let outbound = if io.source_media() {
            Outbound::private(rate, input)?
        } else {
            Outbound::new(rate, input)?
        };
        let (replies, incoming) = RingBuffer::new(16);
        let (digits, events) = RingBuffer::new(64);
        let wake = PeerWake::default();
        let stop = Arc::new(AtomicBool::new(false));
        let source_ready = Arc::new(SourceReady::new());
        let control = PrivatePhoneControl {
            replies,
            digits: events,
            consumed_revision: None,
            approved_recovery: None,
            reply: reply.clone(),
            wake: wake.clone(),
            stop: Arc::clone(&stop),
            source_ready: Arc::clone(&source_ready),
            rate,
            source_media: io.source_media(),
            digit_mode: transmit_digit_mode,
            media: None,
        };
        Ok((
            Self {
                io,
                reply,
                stop,
                source_ready,
                wake,
                epoch: None,
                source_clock: None,
                source_completion: SourceCompletion::Pending,
                digit_mode,
                timed_loss: protocol == rpt_advanced_core::config::PhoneAccountProtocol::Sip,
                source,
                replies: incoming,
                digits,
                revision: CollectionRevision::new(),
                settled_revision: NonZeroU32::MIN,
                pending_release: None,
                recovery: None,
                pending_recovery: None,
                cut_pending: false,
                current: None,
                media: None,
                source_end: 0,
                output,
                outbound,
                spare_output: None,
                programme: false,
                source_position: 0,
                next_audio_ns: 0,
                audio_cadence_active: false,
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
            self.revision.emit(&mut self.digits, digit, now_ms);
            self.revision.0.get().ok_or(Error::Admission)?;
        }
        Ok(seen)
    }
    fn read_source(
        &mut self,
        now_ms: u64,
        admission: &mut SourceTimeReference,
    ) -> Result<(bool, bool), Error> {
        let reference = self
            .source_clock
            .as_ref()
            .map(SourceTimeClock::reference)
            .unwrap_or_default();
        // Reuse the actual last source-read reference at post-read adoption.
        // Original receipt birth is never changed or synthesized.
        *admission = reference;
        let outgoing = self.reply.phone_outgoing();
        let pending_release = &mut self.pending_release;
        let recovery = &mut self.recovery;
        let cut_pending = &mut self.cut_pending;
        let ready = &self.source_ready;
        let settled = self.settled_revision;
        let authorized = self.reply.phone_media_authorized();
        let digits = &mut self.digits;
        let revision = &self.revision;
        let media = &mut self.media;
        let source_end = &mut self.source_end;
        let source = self.source.as_mut().ok_or(Error::Operation)?;
        let mode = self.digit_mode;
        let timed_loss = self.timed_loss;
        let scratch = &mut self.pcm;
        let mut failure = None;
        let mut retire_egress = false;
        let result = self
            .io
            .read_source_causal_at(self.source_completion, reference, |event| {
                let adaptation = match &event {
                    crate::services::SourceDispatch::AdaptationCut { start, count } => {
                        Some((*start, *count))
                    }
                    _ => None,
                };
                let input = match event {
                    crate::services::SourceDispatch::Input(input) => input,
                    crate::services::SourceDispatch::AdaptationCut { start, count } => {
                        debug_assert!(count != 0 && start.checked_add(count as u64).is_some());
                        SourceInput::Discontinuity
                    }
                };
                let credit = match input {
                    SourceInput::Digit(digit) => {
                        *recovery = None;
                        ready.request.store(0, Ordering::Release);
                        ready.revision.store(0, Ordering::Release);
                        if mode == PhoneDigitMode::InBand {
                            return 1;
                        }
                        if digits.slots() == 0 {
                            return 0;
                        }
                        if let Some(media) = media.as_ref() {
                            media.retire();
                        }
                        source.hold_command();
                        revision.emit(digits, digit, now_ms);
                        return usize::from(revision.0.get().is_some());
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
                                    revision.emit(digits, digit, now_ms);
                                }
                            },
                            || {
                                // Exhaustion is checked before this source read returns.
                                let _ = revision.advance();
                                if let Some(media) = current_media {
                                    media.retire();
                                }
                            },
                        );
                        if recovery
                            .as_ref()
                            .is_some_and(|cut| revision.0.get() != Some(cut.ticket.revision))
                        {
                            *recovery = None;
                            ready.request.store(0, Ordering::Release);
                            ready.revision.store(0, Ordering::Release);
                        }
                        analyzed.and_then(|()| {
                            // A prepared candidate may wait for its matching
                            // release through the old tone tail. New onset/digit
                            // callbacks retire it above; only active programme
                            // needs blanket suppression retirement here.
                            if let Some(media) = media
                                .as_ref()
                                .filter(|media| source.muted() && !media.prepared())
                            {
                                media.retire();
                            }
                            let accepted = match media.as_mut() {
                                Some(media) if concealed && !timed_loss => {
                                    media.retire();
                                    count
                                }
                                Some(media) if concealed => {
                                    media.concealed(start, &scratch[..count], birth)?
                                }
                                Some(media) => media.audio(start, &scratch[..count], birth)?,
                                None => count,
                            };
                            source.accepted(accepted)?;
                            if let Some(cut) = recovery.as_mut()
                                && accepted > 0
                                && !concealed
                                && !source.muted()
                                && birth.is_some_and(|birth| {
                                    birth.host_clock_epoch == cut.reference.host_clock_epoch
                                        && birth.host_clock_epoch == reference.host_clock_epoch
                                        && birth.born_ns >= cut.reference.now_ns
                                        && birth.born_ns <= reference.now_ns
                                })
                            {
                                cut.fresh = true;
                            }
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
                                    revision.emit(digits, digit, now_ms);
                                }
                            })
                            .and_then(|()| {
                                *source_end = (*source_end).max(start + count as u64);
                                media.as_mut().map_or(Ok(count), |media| {
                                    if source.muted() && !media.prepared() {
                                        media.retire();
                                    }
                                    if timed_loss {
                                        media.gap(start, count)
                                    } else {
                                        // IAX still supplies unresolved loss, not a SIP loss lease.
                                        media.retire();
                                        Ok(count)
                                    }
                                })
                            })
                    }
                    SourceInput::Discontinuity => {
                        retire_egress = true;
                        let keep_downlink = adaptation.is_none()
                            && !outgoing
                            && authorized
                            && ready.attached.load(Ordering::Acquire)
                            && pending_release.is_none()
                            && !source.muted()
                            && media.as_ref().is_some_and(|media| {
                                media.downlink_active()
                                    && (media.downlink_only() || revision.0.get() == Some(settled))
                            });
                        if !*cut_pending {
                            let origin_epoch = recovery
                                .as_ref()
                                .filter(|cut| revision.0.get() == Some(cut.ticket.revision))
                                .map(|cut| cut.ticket.cancellation_epoch)
                                .or_else(|| {
                                    media
                                        .as_ref()
                                        .filter(|m| {
                                            m.active()
                                                && revision.0.get() == Some(settled)
                                                && m.expected_revision() == settled
                                                && pending_release.is_none()
                                                && source.ready_for_media()
                                        })
                                        .map(|m| m.origin_epoch())
                                });
                            *recovery = adaptation.and_then(|(start, count)| {
                                let epoch = origin_epoch?;
                                if !timed_loss
                                    || outgoing
                                    || !authorized
                                    || !ready.attached.load(Ordering::Acquire)
                                    || ready.epoch() != Some(epoch)
                                    || reference.host_clock_epoch == 0
                                {
                                    return None;
                                }
                                Some(RecoveryCut {
                                    ticket: RecoveryTicket {
                                        revision: NonZeroU32::new(
                                            revision.0.get()?.get().checked_add(1)?,
                                        )?,
                                        cancellation_epoch: epoch,
                                    },
                                    reference,
                                    start,
                                    count,
                                    fresh: false,
                                    approved: false,
                                })
                            });
                            *cut_pending = true;
                            ready.request.store(0, Ordering::Release);
                            ready.revision.store(0, Ordering::Release);
                        } else if recovery
                            .as_ref()
                            .is_some_and(|cut| adaptation != Some((cut.start, cut.count)))
                        {
                            *recovery = None;
                        }
                        // Source/RF retirement is unconditional. Only an already
                        // authorized destination survives a cause-free receive cut.
                        if let Some(media) = media.as_ref() {
                            if keep_downlink {
                                media.retire_source();
                            } else {
                                media.retire();
                            }
                        }
                        if mode == PhoneDigitMode::InBand && digits.slots() == 0 {
                            return 0;
                        }
                        if revision.advance().is_none() {
                            failure = Some(Error::Admission);
                            return 0;
                        }
                        match source.reset(|digit| {
                            if mode == PhoneDigitMode::InBand {
                                revision.emit(digits, digit, now_ms);
                                if let Some(media) = media.as_ref() {
                                    media.retire();
                                }
                            }
                        }) {
                            Ok(()) => {
                                *cut_pending = false;
                                *source_end = 0;
                                if outgoing {
                                    // No incoming collector owns a callee reset.
                                    // Capture AFTER reset may complete a digit;
                                    // end_command/drain still qualify this cut.
                                    *pending_release = revision.0.get();
                                }
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
        if retire_egress || result.finish_pending {
            self.outbound.retire(&mut self.io)?;
        }
        revision.0.get().ok_or(Error::Admission)?;
        if let Some(error) = failure {
            return Err(error);
        }
        if result.closed {
            return Err(Error::Hangup);
        }
        if result.finish_pending {
            *recovery = None;
            ready.request.store(0, Ordering::Release);
            if let Some(media) = self.media.take() {
                media.retire();
            }
            if mode == PhoneDigitMode::InBand && digits.slots() == 0 {
                return Ok((false, false));
            }
            source.discontinuity(|digit| {
                if mode == PhoneDigitMode::InBand {
                    revision.emit(digits, digit, now_ms);
                }
            })?;
        }
        revision.0.get().ok_or(Error::Admission)?;
        let acknowledge =
            result.finish_pending && self.source_completion == SourceCompletion::Pending;
        self.source_completion = if result.finish_pending {
            SourceCompletion::Cancelled
        } else {
            SourceCompletion::Pending
        };
        Ok((
            matches!(result.progress, 1 | 3) || acknowledge,
            matches!(result.progress, 0 | 3) && !result.finish_pending,
        ))
    }
    /// Private replies take priority without another encoder or pending packet.
    /// A completed prompt drains its finite tail before programme resumes.
    fn select_programme(&mut self, wanted: bool) -> Result<(), Error> {
        if self.programme == wanted || (wanted && !self.outbound.ready_for_input()) {
            return Ok(());
        }
        let queue = self.spare_output.take().ok_or(Error::Admission)?;
        self.outbound.retire(&mut self.io)?;
        self.spare_output = Some(self.outbound.exchange(queue)?);
        self.outbound.guard_programme(
            wanted.then(|| self.media.as_ref().expect("active programme").guard()),
            self.reply.phone_outgoing(),
        );
        self.programme = wanted;
        Ok(())
    }

    fn media_now(&self, now_ms: u64) -> Result<u64, Error> {
        let Some(clock) = &self.source_clock else {
            return Ok(now_ms.saturating_mul(1_000_000));
        };
        let reference = clock.reference();
        (reference.host_clock_epoch != 0)
            .then_some(reference.now_ns)
            .ok_or(Error::Admission)
    }

    fn step_inner(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.stop.load(Ordering::Acquire) || self.reply.ended() {
            return Err(Error::Hangup);
        }
        let now_ns = self.media_now(now_ms)?;
        if self.current.is_none() {
            for _ in 0..16 {
                if let Ok(PrivateCommand::TelephoneDigit(symbol)) = self.replies.peek() {
                    if !self.io.try_send_digit(*symbol)? {
                        break;
                    }
                    let _ = self.replies.pop();
                    continue;
                }
                match self.replies.pop().ok() {
                    Some(PrivateCommand::TelephoneDigit(_)) => {
                        unreachable!("single serial consumer")
                    }
                    Some(PrivateCommand::Reply(reply)) => {
                        self.current = Some(reply);
                        self.next_audio_ns = 0;
                        self.audio_cadence_active = false;
                        break;
                    }
                    Some(PrivateCommand::Media(mut media)) => {
                        // Cancelled queued boxes cannot replace a newer live or
                        // prepared generation. Retain only the existing one slot.
                        if !media.prepared()
                            || self
                                .media
                                .as_ref()
                                .is_some_and(|current| current.occupied())
                        {
                            media.retire();
                        } else {
                            self.select_programme(false)?;
                            if let Some(previous) = &self.media {
                                previous.retire();
                            }
                            self.spare_output = media.take_output().map(|queue| (queue, 0));
                            self.media = Some(media);
                        }
                    }
                    Some(PrivateCommand::CollectionEnded(revision)) => {
                        self.pending_release = Some(revision);
                    }
                    Some(PrivateCommand::Recover(ticket)) => {
                        self.pending_recovery = Some(ticket);
                    }
                    None => break,
                }
            }
        }
        let mut admission = SourceTimeReference::default();
        let mut source_drained = false;
        // One serial provider reader. Source callbacks retain native digits or
        // unanalyzed PCM under backpressure; non-authoritative symbols can drain.
        for _ in 0..64 {
            if !self.io.source_media() && self.digits.slots() == 0 {
                break;
            }
            let progressed = if self.io.source_media() {
                let (progressed, drained) = self.read_source(now_ms, &mut admission)?;
                source_drained = drained;
                progressed
            } else {
                self.read_legacy(now_ms)?
            };
            if !progressed {
                break;
            }
        }
        // Activity, immutable origin epoch and real detector drain precede approval.
        if self.recovery.as_ref().is_some_and(|cut| {
            (!self.cut_pending && self.revision.0.get() != Some(cut.ticket.revision))
                || self.source_ready.epoch() != Some(cut.ticket.cancellation_epoch)
                || admission.host_clock_epoch != cut.reference.host_clock_epoch
                || !self.reply.phone_media_authorized()
                || !self.source_ready.attached.load(Ordering::Acquire)
        }) {
            self.recovery = None;
        }
        let ready_recovery = self
            .recovery
            .as_ref()
            .filter(|cut| {
                cut.fresh
                    && source_drained
                    && !self.cut_pending
                    && self
                        .source
                        .as_ref()
                        .is_some_and(SourceIngress::ready_for_media)
            })
            .map(|cut| cut.ticket);
        self.source_ready.request.store(
            ready_recovery.map_or(0, RecoveryTicket::packed),
            Ordering::Release,
        );
        if let Some(approval) = self.pending_recovery {
            if ready_recovery == Some(approval) {
                self.recovery.as_mut().expect("matching recovery").approved = true;
                self.pending_recovery = None;
            } else if self
                .recovery
                .as_ref()
                .is_none_or(|cut| cut.ticket != approval)
            {
                self.pending_recovery = None;
            }
            // Same-ticket raw suffixes retain this single acknowledgement.
            // Control already queued it once and must not manufacture another.
        }
        let recovery_revision = self
            .recovery
            .as_ref()
            .filter(|cut| cut.approved && ready_recovery == Some(cut.ticket))
            .map(|cut| cut.ticket.revision);
        // Read source activity first: a queued release cannot clear a newer onset,
        // event or reset. A matching release waits for stable raw-retry completion
        // and the detector's actual qualified tail, never a private timer.
        if let Some(release) = self.pending_release {
            if self.revision.0.get() != Some(release) {
                self.pending_release = None;
            } else if self.source.as_mut().is_some_and(SourceIngress::end_command) {
                self.settled_revision = release;
                self.pending_release = None;
            }
        }
        self.source_ready.revision.store(
            if source_drained
                && (self.revision.0.get() == Some(self.settled_revision)
                    || recovery_revision == self.revision.0.get())
                && self
                    .source
                    .as_ref()
                    .is_some_and(SourceIngress::ready_for_media)
            {
                recovery_revision.unwrap_or(self.settled_revision).get()
            } else {
                0
            },
            Ordering::Release,
        );
        if let Some(media) = self.media.as_mut().filter(|media| media.prepared()) {
            let expected = media.expected_revision();
            if self.revision.0.get() != Some(expected) {
                media.retire();
            } else if self.settled_revision == expected || recovery_revision == Some(expected) {
                // A provider may retain an unanalysed suffix beyond the cached
                // 20 ms prefix. Only its real idle/drained result closes that cut.
                if source_drained
                    && self
                        .source
                        .as_ref()
                        .is_some_and(SourceIngress::ready_for_media)
                {
                    media.adopt(&self.reply, self.source_end, admission);
                    if media.active() && recovery_revision == Some(expected) {
                        self.settled_revision = expected;
                        self.recovery = None;
                        self.source_ready.request.store(0, Ordering::Release);
                    }
                }
            }
            // Matching Media may reach this serial queue before CollectionEnded.
            // Retain it inactive; only the settled matching release plus real
            // drained/ready source above grants adoption. A newer revision still
            // retires it immediately, so waiting cannot resurrect old custody.
        }
        self.select_programme(
            self.current.is_none()
                && self
                    .media
                    .as_ref()
                    .is_some_and(|media| media.downlink_active()),
        )?;
        if now_ns >= self.next_audio_ns && self.outbound.ready_for_input() {
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
                            born_ns: Some(match &self.source_clock {
                                Some(clock) => {
                                    let reference = clock.reference();
                                    if reference.host_clock_epoch == 0 {
                                        return Err(Error::Admission);
                                    }
                                    reference.now_ns
                                }
                                None => now_ns,
                            }),
                            phone_expires_ns: u64::MAX,
                        },
                        &self.pcm[..count],
                    );
                    // Advance the planned sample cadence, not this wake's lateness.
                    // Phase preserves sub-millisecond durations for partial readers.
                    let phase = self.source_position % 48_000;
                    let duration_ns = (phase + count as u64) * 1_000_000_000 / 48_000
                        - phase * 1_000_000_000 / 48_000;
                    self.source_position = self.source_position.wrapping_add(count as u64);
                    let due = if self.audio_cadence_active {
                        self.next_audio_ns
                    } else {
                        now_ns
                    };
                    self.next_audio_ns = due.saturating_add(duration_ns);
                    self.audio_cadence_active = true;
                } else {
                    // An unready producer has no new samples to pace or age.
                    self.audio_cadence_active = false;
                    self.next_audio_ns = now_ns.saturating_add(10_000_000);
                }
                match reply.outcome() {
                    PlaybackOutcome::Active => {}
                    PlaybackOutcome::Succeeded => {
                        self.current = None;
                        self.outbound.finish_private();
                    }
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
        let next_audio = (self.current.is_some() && self.outbound.ready_for_input())
            .then_some(self.next_audio_ns);
        let media_now = self.media_now(now_ms)?;
        let next = deadline
            .into_iter()
            .chain(
                next_audio
                    .map(|due| std::time::Duration::from_nanos(due.saturating_sub(media_now))),
            )
            .chain(Some(std::time::Duration::from_millis(10)))
            .min()
            .unwrap();
        self.io
            .set_deadline(next.as_nanos().min(u128::from(u64::MAX)) as u64)
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
        if let Some(media) = &self.media {
            media.retire();
        }
        self.reply.retire();
    }
    fn fail(&self, cause: &str, error: Error) {
        let first = !self.stop.swap(true, Ordering::AcqRel);
        if let Some(media) = &self.media {
            media.retire();
        }
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
        self.outbound.set_epoch(epoch);
    }
    fn set_source_clock(&mut self, clock: SourceTimeClock) {
        if self.outbound.bind_media_clock(clock.clone()).is_err() {
            self.stop();
            return;
        }
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
pub(crate) mod tests;
