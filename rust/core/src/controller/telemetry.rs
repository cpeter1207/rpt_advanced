//! Control-prepared playback and bounded status ownership transfer.

use super::parrot::{
    CapturedParrot, ParrotAck, ParrotCapture, ParrotCaptureControl, ParrotRequest,
};
#[cfg(test)]
use crate::audio::{MorseRenderer, PcmRead, ToneSequence};
use crate::audio::{PcmStreamReader, Playback};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

/// Settings shared by control-prepared Morse fallbacks.
#[derive(Clone, Copy)]
pub struct MorseSettings {
    /// Words per minute.
    pub speed_wpm: u32,
    /// Tone frequency in Hz.
    pub frequency_hz: f32,
    /// Tone level relative to full scale.
    pub level_db: i8,
}

impl Default for MorseSettings {
    fn default() -> Self {
        Self {
            speed_wpm: 20,
            frequency_hz: 800.0,
            level_db: -6,
        }
    }
}

/// Rejected media, timing, or configuration value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerError;

/// Eligible outputs for one configured message, never overriding peer link modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackDestination {
    /// Local transmitter only.
    #[default]
    Local,
    /// Local transmitter and eligible connected peers.
    LocalAndLinks,
}

/// Traffic and routing policy retained for an entire message attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Continue repeating received traffic; command detection always continues.
    pub repeat_during_playback: bool,
    /// Configured message audience.
    pub destination: PlaybackDestination,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self {
            repeat_during_playback: true,
            destination: PlaybackDestination::Local,
        }
    }
}

/// Action when a message cannot start within its polite wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PoliteExpiry {
    /// Consume this occurrence without playback.
    #[default]
    Skip,
    /// Select the complete configured Morse substitute, or skip if unavailable.
    Morse,
}

/// Action when expired Morse is eligible but another message owns playback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PoliteBusy {
    /// Consume this occurrence without overlapping the current message.
    #[default]
    Skip,
    /// Keep the original deadline and Morse selection until the slot becomes free.
    Wait,
}

/// Start-deadline policy; it does not limit playback length once a message begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoliteDelivery {
    /// Milliseconds from the original occurrence; zero permits only immediate admission.
    pub maximum_wait_ms: u64,
    /// Behavior when the start deadline expires.
    pub expiry: PoliteExpiry,
    /// Behavior when expired Morse meets occupied playback.
    pub busy: PoliteBusy,
}

impl Default for PoliteDelivery {
    fn default() -> Self {
        Self {
            maximum_wait_ms: 60_000,
            expiry: PoliteExpiry::Skip,
            busy: PoliteBusy::Skip,
        }
    }
}

/// Media validated and allocated exclusively by the control owner.
pub struct PreparedMedia(
    pub(super) Playback,
    pub(super) bool,
    pub(super) MediaKind,
    pub(super) Option<crate::link::PeerReply>,
    pub(super) Option<Arc<AtomicBool>>,
    pub(super) Option<(u64, PoliteDelivery, bool)>,
    pub(super) Option<Arc<MessageOccurrence>>,
    pub(super) Option<PlaybackPolicy>,
    pub(super) Option<usize>,
);

/// One occurrence's race-free transition: pending (0), audible (1), or cancelled (2).
#[derive(Default)]
pub(crate) struct MessageOccurrence(AtomicU8);

impl MessageOccurrence {
    /// Obsolete pending audio may be cancelled; already audible playback is never replaced.
    pub(crate) fn cancel_pending(&self) {
        let _ = self
            .0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
    /// Terminal cancellation is visible before and after a first-sample render.
    pub(crate) fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == 2
    }
    /// Claim the first audible sample, losing safely to a concurrent replacement.
    pub(super) fn begin(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
/// Output routing behavior attached to prepared playback.
pub(super) enum MediaKind {
    Status,
    Parrot,
}

impl PreparedMedia {
    /// Attach a producer-backed native PCM stream to controller playback.
    pub fn new_stream(stream: Box<dyn PcmStreamReader>) -> Result<Self, ControllerError> {
        Ok(Self(
            Playback::new_stream(stream),
            true,
            MediaKind::Status,
            None,
            None,
            None,
            None,
            None,
            None,
        ))
    }

    /// Retain the originating endpoint through playback and control-side reclamation.
    pub fn reply_to(mut self, peer: Option<crate::link::PeerReply>) -> Self {
        self.3 = peer;
        self
    }

    /// Stop a superseded notice at the next sample without reclaiming it on audio.
    pub fn cancel_when(mut self, cancelled: Arc<AtomicBool>) -> Self {
        self.4 = Some(cancelled);
        self
    }

    /// Retain the occurrence's original start deadline in this controller's sample epoch.
    pub fn with_delivery(mut self, deadline_sample: u64, policy: PoliteDelivery) -> Self {
        self.5 = Some((deadline_sample, policy, false));
        self
    }

    /// Retain a configured message's audience and repeat policy for its entire attempt.
    pub fn with_playback_policy(mut self, policy: PlaybackPolicy) -> Self {
        self.7 = Some(policy);
        self
    }

    /// Retain the scheduler's single occurrence identity through queueing and reclamation.
    pub(crate) fn with_occurrence(mut self, occurrence: Option<Arc<MessageOccurrence>>) -> Self {
        self.6 = occurrence;
        self
    }

    /// Select expiry once; opening streams and later deferrals never renew its budget.
    fn expire(&mut self, now: u64, allowed: bool, busy: bool, primary_ready: bool) {
        let Some((deadline, policy, selected)) = &mut self.5 else {
            return;
        };
        if self.0.has_output()
            || self.0.is_finished()
            || *selected
            || now < *deadline
            || (policy.maximum_wait_ms == 0
                && now == *deadline
                && allowed
                && !busy
                && primary_ready)
        {
            return;
        }
        if policy.expiry == PoliteExpiry::Skip
            || !allowed
            || (busy && policy.busy == PoliteBusy::Skip)
        {
            self.0.cancel();
        } else {
            *selected = true;
        }
    }

    /// Morse expiry remains selected even if a never-audible producer is restarted.
    pub(super) fn force_morse(&self) -> bool {
        self.5.is_some_and(|(_, _, selected)| selected)
    }

    /// Construct the spoken-report then recording sequence for local parrot playback.
    pub fn new_parrot_stream(
        report: Option<Box<dyn PcmStreamReader>>,
        recording: Box<dyn PcmStreamReader>,
    ) -> Self {
        let stream = ParrotSequence::new(report, recording);
        Self(
            Playback::new_stream(Box::new(stream)),
            true,
            MediaKind::Parrot,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// Build deterministic in-memory media for controller unit tests only.
    #[cfg(test)]
    pub fn new(
        audio: Option<Vec<f32>>,
        text: &str,
        settings: MorseSettings,
    ) -> Result<Self, ControllerError> {
        Self::new_with_tone(audio, None, text, settings)
    }

    /// Build deterministic in-memory media for controller unit tests only.
    #[cfg(test)]
    pub(crate) fn new_with_tone(
        audio: Option<Vec<f32>>,
        tone: Option<ToneSequence>,
        text: &str,
        settings: MorseSettings,
    ) -> Result<Self, ControllerError> {
        if audio.as_ref().is_some_and(|pcm| {
            pcm.is_empty() || pcm.iter().any(|v| !v.is_finite() || v.abs() > 1.0)
        }) {
            return Err(ControllerError);
        }
        let available = audio.is_some() || tone.is_some() || !text.is_empty();
        let fallback = render_morse(text, settings)?;
        let primary = if let Some(audio) = audio {
            audio
        } else if let Some(mut tone) = tone {
            render_source(&mut tone)?
        } else {
            fallback.clone()
        };
        Ok(Self(
            Playback::new_stream(Box::new(TestPcmStream {
                primary,
                fallback,
                offset: 0,
                use_fallback: false,
            })),
            available,
            MediaKind::Status,
            None,
            None,
            None,
            None,
            None,
            None,
        ))
    }
}

/// PCM ownership returned when a status submission is invalid or queue capacity is exhausted.
#[derive(Debug)]
pub struct StatusRejected {
    /// Original caller-owned PCM, retained on every rejected submission.
    pub audio: Option<Vec<f32>>,
}

/// Lock-free qualifying-receive snapshot shared with the control owner.
#[derive(Clone)]
pub struct ActivitySnapshot {
    last_sample: Arc<AtomicU64>,
    active: Arc<AtomicBool>,
    current_sample: Arc<AtomicU64>,
}

impl ActivitySnapshot {
    pub(super) fn new() -> Self {
        Self {
            last_sample: Arc::new(AtomicU64::new(0)),
            active: Arc::new(AtomicBool::new(false)),
            current_sample: Arc::new(AtomicU64::new(0)),
        }
    }
    pub(super) fn publish(&self, sample: u64) {
        self.last_sample
            .store(sample.saturating_add(1), Ordering::Release);
    }
    pub(super) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }
    /// Publish once per completed callback, independently of receive activity.
    pub(super) fn advance_clock(&self, sample: u64) {
        self.current_sample.store(sample, Ordering::Release);
    }
    /// Conservative command-admission time at the last completed native callback.
    pub(crate) fn current_sample(&self) -> u64 {
        self.current_sample.load(Ordering::Acquire)
    }
    /// Last local or linked receive sample since startup; telemetry never updates it.
    #[must_use]
    pub fn last_sample(&self) -> Option<u64> {
        self.last_sample.load(Ordering::Acquire).checked_sub(1)
    }
    /// Whether a local-receiver or linked-peer signal is active in the current callback.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

/// Sole status producer and completed-media reclaimer; retain until audio stops.
pub struct ControllerControl {
    site_inhibit: Arc<AtomicBool>,
    cancel_message: Arc<AtomicBool>,
    messages: Option<super::MessagePreparation>,
    pending: Producer<PreparedMedia>,
    completed: Consumer<PreparedMedia>,
    #[cfg(test)]
    settings: MorseSettings,
    outstanding: usize,
    pub(super) parrot: Option<ParrotCaptureControl>,
    parrot_pending: Producer<PreparedMedia>,
    parrot_completed: Consumer<PreparedMedia>,
    parrot_busy: Arc<AtomicBool>,
    pub(super) parrot_enabled: bool,
    parrot_requests: Option<Producer<ParrotRequest>>,
    parrot_acknowledgements: Option<Consumer<ParrotAck>>,
    pending_parrot_control: Option<ParrotCaptureControl>,
    parrot_lifecycle_pending: bool,
    parrot_flag: Arc<AtomicBool>,
}

impl ControllerControl {
    /// Request cancellation of the active attempt without reclaiming media on audio.
    pub fn cancel_message(&self) {
        self.cancel_message.store(true, Ordering::Release);
    }

    /// Set station protection independently of telemetry admission and physical interlocks.
    pub fn set_site_inhibit(&self, inhibited: bool) {
        self.site_inhibit.store(inhibited, Ordering::Release);
    }

    pub(crate) fn set_message_preparation(&mut self, messages: super::MessagePreparation) {
        self.messages = Some(messages);
    }

    /// Reclaim retired composition streams and take one scalar preparation request on control.
    pub fn take_message_request(&mut self) -> Option<super::MessageRequest> {
        self.messages.as_mut()?.take_request()
    }

    /// Allocate a one-shot reader on control; values are requested only when playback starts.
    pub(crate) fn prepare_message(
        &mut self,
        content_index: usize,
    ) -> Result<PreparedMedia, ControllerError> {
        self.messages
            .as_mut()
            .ok_or(ControllerError)?
            .register(content_index)
    }

    /// Complete the exact attempt, retaining rejected ownership on control for retry or disposal.
    pub fn complete_message(
        &mut self,
        attempt: super::MessageAttemptId,
        result: Result<PreparedMedia, ControllerError>,
    ) -> Result<(), super::MessageCompletion> {
        match &mut self.messages {
            Some(messages) => messages.complete(attempt, result),
            None => Err(super::MessageCompletion { attempt, result }),
        }
    }

    pub(super) fn configure_parrot_lifecycle(
        &mut self,
        requests: Producer<ParrotRequest>,
        acknowledgements: Consumer<ParrotAck>,
        flag: Arc<AtomicBool>,
    ) {
        self.parrot_requests = Some(requests);
        self.parrot_acknowledgements = Some(acknowledgements);
        self.parrot_flag = flag;
    }

    /// Request live capture allocation or reclamation without work or ownership changes on audio.
    pub fn set_parrot_enabled(&mut self, enabled: bool) -> bool {
        self.reclaim_parrot_lifecycle();
        if enabled == self.parrot_enabled {
            return true;
        }
        if self.parrot_lifecycle_pending {
            return false;
        }
        let Some(requests) = &mut self.parrot_requests else {
            return false;
        };
        if enabled {
            let (capture, control) = ParrotCapture::new();
            if let Err(rtrb::PushError::Full(ParrotRequest::Enable(_))) =
                requests.push(ParrotRequest::Enable(capture))
            {
                return false;
            }
            self.pending_parrot_control = Some(control);
        } else if requests.push(ParrotRequest::Disable).is_err() {
            return false;
        }
        self.parrot_enabled = enabled;
        self.parrot_flag.store(enabled, Ordering::Release);
        self.parrot_lifecycle_pending = true;
        true
    }

    fn reclaim_parrot_lifecycle(&mut self) {
        while let Some(acknowledgements) = &mut self.parrot_acknowledgements {
            let Ok(acknowledgement) = acknowledgements.pop() else {
                break;
            };
            match acknowledgement {
                ParrotAck::Enabled => {
                    self.parrot = self.pending_parrot_control.take();
                }
                ParrotAck::Disabled(capture) => {
                    self.parrot.take();
                    drop(capture);
                }
            }
            self.parrot_lifecycle_pending = false;
        }
    }

    /// Current live switch state (separate from the file value restored on reload).
    #[must_use]
    pub fn parrot_enabled(&self) -> bool {
        self.parrot_enabled
    }

    /// Take a completed parrot burst without entering the callback owner.
    pub fn take_parrot_capture(&mut self) -> Option<CapturedParrot> {
        self.parrot.as_mut()?.take_completed()
    }

    /// Recycle a capture slot after control/media has finished with its samples.
    pub fn recycle_parrot_capture(&mut self, clip: CapturedParrot) {
        if let Some(parrot) = &mut self.parrot {
            parrot.recycle(clip);
        }
    }

    /// Enqueue polite status media; all receive activity must end before playout.
    /// Once audible, receive interruption selects the complete Morse fallback.
    pub fn queue_prepared_status(&mut self, media: PreparedMedia) -> bool {
        if self.outstanding == 4 {
            return false;
        }
        if self.pending.push(media).is_err() {
            return false;
        }
        self.outstanding += 1;
        true
    }

    /// Queue one completed recording for serialized parrot playback.
    pub fn queue_prepared_parrot(&mut self, media: PreparedMedia) -> bool {
        if !self.parrot_enabled
            || media.2 != MediaKind::Parrot
            || self
                .parrot_busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        if self.parrot_pending.push(media).is_err() {
            self.parrot_busy.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// Whether another prepared status can be queued without exceeding the bounded ring.
    #[must_use]
    pub fn can_queue_status(&self) -> bool {
        self.outstanding < 4
    }

    /// Prepare and enqueue printable status, retaining input ownership on failure.
    #[cfg(test)]
    pub fn queue_status(
        &mut self,
        text: &str,
        audio: Option<Vec<f32>>,
    ) -> Result<(), StatusRejected> {
        let valid = !text.is_empty()
            && text.len() < 128
            && MorseRenderer::new(
                text,
                self.settings.speed_wpm,
                self.settings.frequency_hz,
                self.settings.level_db,
            )
            .is_ok()
            && audio.as_ref().is_none_or(|pcm| {
                !pcm.is_empty() && pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.0)
            });
        if !valid || self.outstanding == 4 {
            return Err(StatusRejected { audio });
        }
        // Validation above guarantees construction; preparation and allocation stay here.
        let media = PreparedMedia::new(audio, text, self.settings).expect("validated status media");
        self.pending.push(media).ok();
        self.outstanding += 1;
        Ok(())
    }

    /// Reclaim completed media on the control owner; dropping each item releases its PCM.
    pub fn reclaim(&mut self) -> impl Iterator<Item = PreparedMedia> + '_ {
        self.reclaim_parrot_lifecycle();
        std::iter::from_fn(|| {
            if let Ok(media) = self.completed.pop() {
                self.outstanding -= 1;
                return Some(media);
            }
            let media = self.parrot_completed.pop().ok()?;
            self.parrot_busy.store(false, Ordering::Release);
            Some(media)
        })
    }
}

/// Optional speech prefix followed by the captured native-rate recording.
struct ParrotSequence {
    report: Option<Box<dyn PcmStreamReader>>,
    recording: Box<dyn PcmStreamReader>,
    report_started: bool,
}

impl ParrotSequence {
    fn new(report: Option<Box<dyn PcmStreamReader>>, recording: Box<dyn PcmStreamReader>) -> Self {
        Self {
            report,
            recording,
            report_started: false,
        }
    }

    fn start_recording(&mut self) {
        self.recording.start();
    }
}

impl PcmStreamReader for ParrotSequence {
    fn start(&mut self) {
        self.report_started = false;
        if let Some(report) = &mut self.report {
            report.start();
            self.report_started = true;
        } else {
            self.start_recording();
        }
    }

    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        if output.is_empty() {
            return crate::audio::PcmRead::Pending;
        }
        if self.report_started {
            match self
                .report
                .as_mut()
                .expect("started report exists")
                .render(output)
            {
                crate::audio::PcmRead::Samples(count)
                | crate::audio::PcmRead::FinalSamples(count) => {
                    if count != 0 {
                        return crate::audio::PcmRead::Samples(count.min(output.len()));
                    }
                }
                crate::audio::PcmRead::Pending => return crate::audio::PcmRead::Pending,
                crate::audio::PcmRead::Finished | crate::audio::PcmRead::Failed => {}
            }
            self.report_started = false;
            self.start_recording();
        }
        self.recording.render(output)
    }

    fn cancel(&mut self) {
        if let Some(report) = &mut self.report {
            report.cancel();
        }
        self.recording.cancel();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Status,
    Parrot,
    Courtesy(usize),
    Identifier(usize),
    Announcement(usize),
    Tail(usize),
}

pub(super) struct TelemetryPlanner {
    pub site_inhibit: Arc<AtomicBool>,
    pub cancel_message: Arc<AtomicBool>,
    pub active: Option<Source>,
    pub status: Option<PreparedMedia>,
    /// An admitted opening attempt temporarily released its slot for receive activity.
    pub held_status: bool,
    pending: Consumer<PreparedMedia>,
    completed: Producer<PreparedMedia>,
    parrot_pending: Consumer<PreparedMedia>,
    parrot_completed: Producer<PreparedMedia>,
    pub parrot: Option<PreparedMedia>,
    pub gain: f32,
    duck_gain: f32,
}

impl TelemetryPlanner {
    pub fn new(settings: MorseSettings, duck_db: i8) -> (Self, ControllerControl) {
        #[cfg(not(test))]
        let _ = settings;
        let (producer, pending) = RingBuffer::new(4);
        let (completed, consumer) = RingBuffer::new(4);
        let (parrot_pending, parrot_consumer) = RingBuffer::new(1);
        let (parrot_completed, parrot_reclaimer) = RingBuffer::new(1);
        let parrot_busy = Arc::new(AtomicBool::new(false));
        let site_inhibit = Arc::new(AtomicBool::new(false));
        let cancel_message = Arc::new(AtomicBool::new(false));
        (
            Self {
                site_inhibit: site_inhibit.clone(),
                cancel_message: cancel_message.clone(),
                active: None,
                status: None,
                held_status: false,
                pending,
                completed,
                parrot_pending: parrot_consumer,
                parrot_completed,
                parrot: None,
                gain: 1.0,
                duck_gain: 10_f32.powf(f32::from(duck_db) / 20.0),
            },
            ControllerControl {
                site_inhibit,
                cancel_message,
                messages: None,
                pending: producer,
                completed: consumer,
                #[cfg(test)]
                settings,
                outstanding: 0,
                parrot: None,
                parrot_pending,
                parrot_completed: parrot_reclaimer,
                parrot_busy,
                parrot_enabled: false,
                parrot_requests: None,
                parrot_acknowledgements: None,
                pending_parrot_control: None,
                parrot_lifecycle_pending: false,
                parrot_flag: Arc::new(AtomicBool::new(false)),
            },
        )
    }
    pub fn status_pending(&self) -> bool {
        self.status.is_some() || !self.pending.is_empty()
    }
    /// Observe every bounded pending deadline without removing or dropping queue ownership.
    pub fn expire_status(&mut self, now: u64, allowed: bool, occupied: bool, primary_ready: bool) {
        let occupied = occupied || self.active.is_some_and(|source| source != Source::Status);
        if let Some(media) = &mut self.status {
            media.expire(now, allowed, occupied, primary_ready);
        }
        let mut blocked = occupied
            || self
                .status
                .as_ref()
                .is_some_and(|media| !media.0.is_finished());
        let count = self.pending.slots();
        let mut chunk = self
            .pending
            .read_chunk(count)
            .expect("sole consumer retains its readable slots");
        let (first, second) = chunk.as_mut_slices();
        for media in first.iter_mut().chain(second) {
            media.expire(now, allowed, blocked, primary_ready);
            blocked |= !media.0.is_finished();
        }
        // No commit: cancelled envelopes remain owned until normal control-side retirement.
    }
    /// Whether the head occurrence may start its expired Morse despite receive activity.
    pub fn status_morse(&self) -> bool {
        self.status.as_ref().is_some_and(PreparedMedia::force_morse)
    }
    /// Skip ended endpoints and superseded notices, returning their ownership to control.
    pub fn poll_status(&mut self) {
        loop {
            if self.status.is_none() {
                self.status = self.pending.pop().ok();
            }
            let Some(media) = self.status.as_mut().filter(|media| {
                media.0.is_finished()
                    || media
                        .6
                        .as_ref()
                        .is_some_and(|occurrence| occurrence.cancelled())
                    || media.3.as_ref().is_some_and(|peer| peer.ended())
                    || media
                        .4
                        .as_ref()
                        .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
            }) else {
                break;
            };
            media.0.cancel();
            if self.active == Some(Source::Status) {
                self.active = None;
            }
            self.finish_status();
        }
    }
    pub fn peer_status(&self) -> Option<&crate::link::PeerReply> {
        self.status.as_ref().and_then(|media| media.3.as_ref())
    }
    pub fn parrot_pending(&self) -> bool {
        self.parrot.is_some() || !self.parrot_pending.is_empty()
    }
    pub fn start_parrot(&mut self) {
        self.parrot = self.parrot_pending.pop().ok();
        if let Some(media) = &mut self.parrot {
            media.0.restart(false);
        }
    }
    pub fn discard_pending_parrot(&mut self) {
        if let Ok(media) = self.parrot_pending.pop() {
            let _ = self.parrot_completed.push(media);
        }
    }
    /// Interrupt live parrot playback and return queued media to control for reclamation.
    pub fn disable_parrot(&mut self) {
        self.discard_pending_parrot();
        if self.active == Some(Source::Parrot) {
            self.active = None;
            self.finish_parrot();
        }
    }
    pub fn finish_parrot(&mut self) {
        if let Some(media) = self.parrot.take() {
            let _ = self.parrot_completed.push(media);
        }
    }
    pub fn start_status(&mut self) {
        self.poll_status();
        self.active = self.status.as_ref().map(|_| Source::Status);
        self.held_status = false;
    }
    pub fn finish_status(&mut self) {
        self.held_status = false;
        self.status.take().into_iter().for_each(|media| {
            // Admission permits at most four statuses across pending, active and completed.
            // Owning this status proves the four-slot completed queue has room.
            let _ = self.completed.push(media);
        });
    }
    pub fn gain_step(&mut self, receive: bool) -> f32 {
        let target = if receive { self.duck_gain } else { 1.0 };
        self.gain = if self.gain > target {
            (self.gain - 1.0 / 480.0).max(target)
        } else {
            (self.gain + 1.0 / 4800.0).min(target)
        };
        self.gain
    }
}

#[cfg(test)]
struct TestPcmStream {
    primary: Vec<f32>,
    fallback: Vec<f32>,
    offset: usize,
    use_fallback: bool,
}

#[cfg(test)]
impl PcmStreamReader for TestPcmStream {
    fn start(&mut self) {
        self.offset = 0;
        self.use_fallback = false;
    }
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        let source = if self.use_fallback {
            &self.fallback
        } else {
            &self.primary
        };
        let count = output.len().min(source.len().saturating_sub(self.offset));
        output[..count].copy_from_slice(&source[self.offset..self.offset + count]);
        self.offset += count;
        if count == 0 {
            PcmRead::Finished
        } else if self.offset == source.len() {
            PcmRead::FinalSamples(count)
        } else {
            PcmRead::Samples(count)
        }
    }
    fn select_morse_fallback(&mut self) -> crate::audio::MorseSwitch {
        self.use_fallback = true;
        self.offset = 0;
        if self.fallback.is_empty() {
            crate::audio::MorseSwitch::Unavailable
        } else {
            crate::audio::MorseSwitch::Selected
        }
    }
}

#[cfg(test)]
fn render_morse(text: &str, settings: MorseSettings) -> Result<Vec<f32>, ControllerError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut source = MorseRenderer::new(
        text,
        settings.speed_wpm,
        settings.frequency_hz,
        settings.level_db,
    )
    .map_err(|_| ControllerError)?;
    render_source(&mut source)
}

#[cfg(test)]
fn render_source(source: &mut impl crate::audio::AudioSource) -> Result<Vec<f32>, ControllerError> {
    let mut result = Vec::new();
    let mut block = [0.0; 2048];
    loop {
        let count = source.render(&mut block);
        result.try_reserve(count).map_err(|_| ControllerError)?;
        result.extend_from_slice(&block[..count]);
        if count < block.len() {
            return Ok(result);
        }
    }
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
