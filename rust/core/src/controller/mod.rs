//! Node transmit ownership and serialized producer-backed telemetry at 48 kHz.

mod announcement;
mod courtesy;
mod message;
mod parrot;
mod tail;
mod telemetry;
mod timeout;
pub use tail::Tail;
use tail::TailPlanner;

#[cfg(test)]
mod tail_tests;

use crate::{
    audio::PlaybackOutcome,
    policy::{DuplexPolicy, IdentifierPolicy, IdentifierRule},
};
pub use announcement::Announcement;
use announcement::AnnouncementState;
use courtesy::CourtesyPlanner;
pub use courtesy::CourtesySettings;
pub(crate) use message::MessagePreparation;
pub use message::{MessageAttemptId, MessageCompletion, MessageRequest};
pub use parrot::{CapturedParrot, ParrotLevels};
use parrot::{ParrotAck, ParrotCapture, ParrotLifecycle, ParrotRequest};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub(crate) use telemetry::MessageOccurrence;
pub use telemetry::{
    ActivitySnapshot, ControllerControl, ControllerError, MorseSettings, PlaybackDestination,
    PlaybackPolicy, PoliteBusy, PoliteDelivery, PoliteExpiry, PreparedMedia, StatusRejected,
};
use telemetry::{Source, TelemetryPlanner};
use timeout::TimeoutPolicy;

fn samples(ms: u64) -> u64 {
    ms.saturating_mul(48)
}

/// An identifier's prepared audio and existing interval/priority policy.
pub struct Identifier {
    /// Validated speech/file PCM and Morse fallback.
    pub media: PreparedMedia,
    /// Minimum interval or qualifying idle time for first-key IDs.
    pub interval_ms: u64,
    /// Higher priorities satisfy lower-priority IDs on completion.
    pub priority: i32,
    /// Join the per-node round-robin choice on a qualifying transmitter rising edge.
    pub first_key_only: bool,
    /// Periodic IDs can run without qualifying receiver activity.
    pub regardless_of_activity: bool,
    /// Bounded polite deferral; `None` disables deferral.
    pub polite_maximum_wait_ms: Option<u64>,
}

/// Resolved node timing and transmit settings; native audio is always 48 kHz.
#[derive(Default)]
pub struct ControllerSettings {
    /// Enable fixed-capacity receive burst capture for parrot playback.
    pub parrot_enabled: bool,
    /// Allow local receive while transmitting and repeat local receive audio.
    pub full_duplex: bool,
    /// Ordinary transmitter hang time.
    pub hang_ms: u64,
    /// Restrict transmitted CTCSS to received traffic and command responses.
    pub ctcss_encode_on_input: bool,
    /// Retain received-traffic CTCSS after unkey, bounded by transmitter hang.
    pub ctcss_hang_ms: u64,
    /// Maximum continuous keyed interval without any source unkey; zero disables.
    pub transmit_timeout_ms: u64,
    /// Post-timeout lockout; recovery additionally requires receive inactivity.
    pub timeout_lockout_ms: u64,
    /// Suppress hang and release telemetry for local bursts at most this long; zero disables.
    pub kerchunk_max_ms: u64,
    /// Per-source courtesy delay after unkey.
    pub courtesy_delay_ms: u64,
    /// Receive-active attenuation from -60 through 0 dB.
    pub telemetry_duck_db: i8,
    /// Status Morse fallback parameters.
    pub status_morse: MorseSettings,
}

/// Callback-owned parrot fanout and per-sample addressed command replies.
pub(crate) struct ParrotOutput<'a> {
    pub samples: &'a mut [f32],
    pub active: &'a mut bool,
    pub replies: Option<&'a mut [PeerReplySample]>,
    /// Local-only finite signaling; real zero samples keep their gap timing.
    pub regeneration: Option<&'a [Option<f32>]>,
}

/// Per-sample addressed media, independent of RF and ordinary mix-minus routing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PeerReplySample {
    pub target: usize,
    pub sample: f32,
    /// Suppress ordinary traffic only after its input rings and detectors have advanced.
    pub suppress_repeat: bool,
    /// Configured message PCM, separate from addressed command replies and RF signaling.
    pub broadcast: Option<f32>,
}

struct ProgramSample<'a> {
    parrot: &'a mut f32,
    parrot_active: &'a mut bool,
    reply: Option<&'a mut PeerReplySample>,
    regeneration: Option<f32>,
}

/// Single audio-owner node aggregate; construction and media preparation belong to control.
///
/// Processing consumes elapsed native samples, never callback count or wall-clock time.
/// All configured media remains allocated until this controller is dropped off the audio path.
pub struct NodeController {
    settings: ControllerSettings,
    duplex: DuplexPolicy,
    identifiers: IdentifierPolicy,
    ids: Vec<Identifier>,
    rules: Vec<IdentifierRule>,
    satisfied: Vec<u64>,
    first_due: Vec<u64>,
    telemetry: TelemetryPlanner,
    courtesy: CourtesyPlanner,
    announcements: Vec<AnnouncementState>,
    tails: TailPlanner,
    timeout: TimeoutPolicy,
    activity: ActivitySnapshot,
    now: u64,
    receiver: bool,
    linked: bool,
    receiver_key: u64,
    receiver_unkey: u64,
    last_activity: u64,
    key_idle: u64,
    keyed: bool,
    traffic_keyed: bool,
    exempt_release: bool,
    last_audio: u64,
    release_hang: u64,
    ctcss_hang_until: u64,
    ctcss_enabled: bool,
    release_pending: bool,
    suppress_release: bool,
    parrot_capture: Option<parrot::ParrotCapture>,
    parrot_lifecycle: ParrotLifecycle,
    parrot_enabled_flag: Arc<AtomicBool>,
    parrot_pending_ack: Option<ParrotAck>,
}

impl NodeController {
    /// Validate immutable settings and allocate bounded queues and per-rule state on control.
    pub fn new(
        mut settings: ControllerSettings,
        ids: Vec<Identifier>,
        announcements: Vec<Announcement>,
        courtesy: CourtesySettings,
    ) -> Result<(Self, ControllerControl), ControllerError> {
        if settings.status_morse.speed_wpm == 0
            && settings.status_morse.frequency_hz == 0.0
            && settings.status_morse.level_db == 0
        {
            settings.status_morse = MorseSettings::default();
        }
        let durations = [
            settings.hang_ms,
            settings.ctcss_hang_ms,
            settings.transmit_timeout_ms,
            settings.timeout_lockout_ms,
            settings.kerchunk_max_ms,
            settings.courtesy_delay_ms,
        ];
        if !(-60..=0).contains(&settings.telemetry_duck_db)
            || settings.ctcss_hang_ms > settings.hang_ms
            || durations.iter().any(|d| *d > u64::MAX / 48)
            || ids.iter().any(|id| {
                id.interval_ms > u64::MAX / 48
                    || id.polite_maximum_wait_ms.is_some_and(|d| d > u64::MAX / 48)
            })
            || announcements.iter().any(|a| a.interval_ms > u64::MAX / 48)
            || courtesy
                .peers
                .iter()
                .any(|(name, _)| name.is_empty() || name.len() >= 64 || name.contains('\0'))
            || crate::audio::MorseRenderer::new(
                "E",
                settings.status_morse.speed_wpm,
                settings.status_morse.frequency_hz,
                settings.status_morse.level_db,
            )
            .is_err()
        {
            return Err(ControllerError);
        }
        let rules: Vec<_> = ids
            .iter()
            .map(|id| {
                IdentifierRule::new(
                    samples(id.interval_ms),
                    id.priority,
                    id.first_key_only,
                    id.regardless_of_activity,
                )
            })
            .collect();
        let identifiers = IdentifierPolicy::new(&rules);
        let satisfied = vec![0; ids.len()];
        let first_due = vec![0; ids.len()];
        let (telemetry, mut control) =
            TelemetryPlanner::new(settings.status_morse, settings.telemetry_duck_db);
        let (parrot_requests, parrot_acknowledgements, parrot_lifecycle) =
            ParrotCapture::lifecycle();
        let parrot_enabled_flag = Arc::new(AtomicBool::new(settings.parrot_enabled));
        control.configure_parrot_lifecycle(
            parrot_requests,
            parrot_acknowledgements,
            parrot_enabled_flag.clone(),
        );
        let parrot_capture = if settings.parrot_enabled {
            let (capture, capture_control) = parrot::ParrotCapture::new();
            control.parrot = Some(capture_control);
            Some(capture)
        } else {
            None
        };
        let ctcss_enabled = !settings.ctcss_encode_on_input;
        control.parrot_enabled = settings.parrot_enabled;
        Ok((
            Self {
                settings,
                duplex: DuplexPolicy::default(),
                identifiers,
                ids,
                rules,
                satisfied,
                first_due,
                telemetry,
                courtesy: CourtesyPlanner::new(courtesy),
                announcements: announcements
                    .into_iter()
                    .map(|announcement| AnnouncementState {
                        announcement,
                        satisfied: 0,
                        retry_after: 0,
                        release_pending: false,
                    })
                    .collect(),
                timeout: TimeoutPolicy::default(),
                tails: TailPlanner::default(),
                activity: ActivitySnapshot::new(),
                now: 0,
                receiver: false,
                linked: false,
                receiver_key: 0,
                receiver_unkey: 0,
                last_activity: 0,
                key_idle: 0,
                keyed: false,
                traffic_keyed: false,
                exempt_release: false,
                last_audio: 0,
                release_hang: 0,
                ctcss_hang_until: 0,
                ctcss_enabled,
                release_pending: false,
                suppress_release: false,
                parrot_capture,
                parrot_lifecycle,
                parrot_enabled_flag,
                parrot_pending_ack: None,
            },
            control,
        ))
    }

    /// Tail media is installed only during control-side construction, before publication.
    pub(crate) fn set_tails(&mut self, tails: Vec<Tail>) -> Result<(), ControllerError> {
        self.tails = TailPlanner::new(tails)?;
        Ok(())
    }

    /// Clone the atomic receive-only activity handle outside audio processing.
    #[must_use]
    pub fn activity(&self) -> ActivitySnapshot {
        self.activity.clone()
    }

    /// Feed the already mixed receive tap to the preallocated parrot capture owner.
    pub fn observe_parrot(&mut self, active: bool, samples: &[f32]) {
        self.sync_parrot_lifecycle();
        if let Some(capture) = &mut self.parrot_capture {
            capture.observe(active, samples);
        }
    }

    /// Whether a parrot segment currently owns the transmitter media slot.
    #[cfg(test)]
    pub(crate) fn parrot_playback_active(&self) -> bool {
        self.telemetry.active == Some(Source::Parrot)
    }

    /// Cancel only the resumed direct peer's pending courtesy; active playback continues.
    pub fn link_keyed(&mut self, direct: &str) {
        if !direct.is_empty() {
            self.courtesy.cancel(direct);
        }
    }

    /// A kerchunk cannot retain traffic hang; normal releases clear prior suppression.
    fn receive_unkeyed(&mut self, kerchunk: bool) {
        self.timeout.unkey(self.now);
        self.suppress_release = kerchunk;
        if kerchunk {
            self.duplex = DuplexPolicy::default();
            self.release_hang = 0;
        }
    }

    /// Apply a direct-peer unkey on the audio owner, selecting advisory/direct/generic media.
    pub fn link_unkeyed(&mut self, direct: &str, selected: &str, kerchunk: bool) {
        self.receive_unkeyed(kerchunk);
        if !kerchunk && !direct.is_empty() {
            self.courtesy.schedule(
                direct,
                selected,
                self.now
                    .saturating_add(samples(self.settings.courtesy_delay_ms)),
            );
        }
    }

    /// Apply a sample-free receive edge without advancing playback or elapsed time.
    pub fn process_event(&mut self, receiving: bool, linked: bool) -> bool {
        self.process_event_with_activity(receiving, linked, receiving || linked)
    }

    /// Whether the current native output tick should transmit the configured CTCSS tone.
    ///
    /// Disabled activity scoping preserves the radio's existing tone behavior.
    #[must_use]
    pub fn ctcss_enabled(&self) -> bool {
        self.ctcss_enabled
    }

    fn process_event_with_activity(
        &mut self,
        receiving: bool,
        linked: bool,
        source_activity: bool,
    ) -> bool {
        self.sync_parrot_lifecycle();
        self.apply_parrot_disable();
        self.activity.set_active(source_activity);
        self.step(receiving, linked, source_activity, None, 0.0, None);
        self.keyed
    }

    /// Replace local receive PCM with transmit PCM; return final requested PTT.
    ///
    /// Linked PCM may be shorter than the output; missing samples are silence. Each sample
    /// advances exactly one 48-kHz tick, so arbitrary callback partitioning has identical output.
    /// The caller supplies finite normalized PCM and applies the returned PTT to its adapter.
    pub fn process_audio(
        &mut self,
        receiving: bool,
        linked: bool,
        link_audio: &[f32],
        audio: &mut [f32],
    ) -> bool {
        self.process_audio_outputs(
            receiving,
            linked,
            receiving || linked,
            link_audio,
            audio,
            None,
        )
    }

    /// Render RF, parrot fanout, and addressed replies into separate preallocated outputs.
    pub(crate) fn process_audio_with_parrot(
        &mut self,
        receiving: bool,
        linked: bool,
        source_activity: bool,
        link_audio: &[f32],
        audio: &mut [f32],
        parrot: ParrotOutput<'_>,
    ) -> bool {
        self.process_audio_outputs(
            receiving,
            linked,
            source_activity,
            link_audio,
            audio,
            Some(parrot),
        )
    }

    fn process_audio_outputs(
        &mut self,
        receiving: bool,
        linked: bool,
        source_activity: bool,
        link_audio: &[f32],
        audio: &mut [f32],
        mut parrot_output: Option<ParrotOutput<'_>>,
    ) -> bool {
        self.sync_parrot_lifecycle();
        self.apply_parrot_disable();
        if let Some(output) = &mut parrot_output {
            *output.active = false;
        }
        if audio.is_empty() {
            return self.process_event_with_activity(receiving, linked, source_activity);
        }
        if parrot_output.as_ref().is_some_and(|output| {
            output.samples.len() != audio.len()
                || output
                    .regeneration
                    .is_some_and(|samples| samples.len() != audio.len())
        }) {
            return self.process_event_with_activity(receiving, linked, source_activity);
        }
        self.activity.set_active(source_activity);
        if let Some(output) = &mut parrot_output {
            output.samples.fill(0.0);
        }
        for (offset, sample) in audio.iter_mut().enumerate() {
            let parrot_sample = parrot_output.as_mut().map(|output| ProgramSample {
                parrot: &mut output.samples[offset],
                parrot_active: &mut *output.active,
                reply: output
                    .replies
                    .as_deref_mut()
                    .and_then(|replies| replies.get_mut(offset)),
                regeneration: output.regeneration.and_then(|samples| samples[offset]),
            });
            let rf = self.step(
                receiving,
                linked,
                source_activity,
                Some(*sample),
                link_audio.get(offset).copied().unwrap_or(0.0),
                parrot_sample,
            );
            *sample = rf;
            self.now = self.now.saturating_add(1);
        }
        self.activity.advance_clock(self.now);
        self.keyed
    }

    fn sync_parrot_lifecycle(&mut self) {
        if let Some(acknowledgement) = self.parrot_pending_ack.take() {
            if let Err(rtrb::PushError::Full(acknowledgement)) =
                self.parrot_lifecycle.acknowledgements.push(acknowledgement)
            {
                self.parrot_pending_ack = Some(acknowledgement);
                return;
            }
        }
        let acknowledgement = match self.parrot_lifecycle.requests.pop() {
            Ok(ParrotRequest::Enable(capture)) => {
                self.parrot_capture = Some(capture);
                ParrotAck::Enabled
            }
            Ok(ParrotRequest::Disable) => {
                self.telemetry.disable_parrot();
                let capture = self
                    .parrot_capture
                    .take()
                    .expect("disable requests follow an acknowledged enable");
                ParrotAck::Disabled(capture)
            }
            Err(_) => return,
        };
        if let Err(rtrb::PushError::Full(acknowledgement)) =
            self.parrot_lifecycle.acknowledgements.push(acknowledgement)
        {
            self.parrot_pending_ack = Some(acknowledgement);
        }
    }

    fn apply_parrot_disable(&mut self) {
        if !self.parrot_enabled_flag.load(Ordering::Acquire) {
            self.telemetry.disable_parrot();
        }
    }

    fn id_ready(&self, selected: usize, busy: bool) -> bool {
        self.ids.get(selected).is_some_and(|id| {
            let Some(maximum) = id.polite_maximum_wait_ms.filter(|_| busy) else {
                return true;
            };
            let due = if id.first_key_only {
                self.first_due.get(selected).copied().unwrap_or(self.now)
            } else {
                self.satisfied
                    .get(selected)
                    .copied()
                    .unwrap_or(self.now)
                    .saturating_add(samples(id.interval_ms))
            };
            self.now.saturating_sub(due) >= samples(maximum)
        })
    }

    fn select_identifier(&self, receiving: bool, busy: bool, after_hang: bool) -> Option<usize> {
        let expired = |index: usize| {
            self.ids[index].polite_maximum_wait_ms.is_some() && self.id_ready(index, true)
        };
        let selected =
            self.identifiers
                .select(&self.rules, self.now, receiving, self.settings.full_duplex);
        let minimum_priority = selected
            .filter(|&index| {
                self.ids[index].first_key_only && self.ids[index].polite_maximum_wait_ms.is_some()
            })
            .map_or(i32::MIN, |index| self.ids[index].priority);
        selected
            .filter(|&index| expired(index) || (after_hang && self.id_ready(index, busy)))
            .or_else(|| {
                // A polite initial covers lower-priority IDs; their deadlines cannot bypass it.
                // Periodic IDs retain their hard-deadline arbitration.
                self.identifiers.select_where(
                    &self.rules,
                    self.now,
                    receiving,
                    self.settings.full_duplex,
                    |index| expired(index) && self.ids[index].priority >= minimum_priority,
                )
            })
    }

    /// Borrow the generation-owned envelope without moving or reclaiming it on audio.
    fn media(&mut self, source: Option<Source>) -> Option<&mut PreparedMedia> {
        match source? {
            Source::Status => self.telemetry.status.as_mut(),
            Source::Parrot => self.telemetry.parrot.as_mut(),
            Source::Courtesy(index) => self.courtesy.media.get_mut(index),
            Source::Identifier(index) => self.ids.get_mut(index).map(|id| &mut id.media),
            Source::Announcement(index) => self
                .announcements
                .get_mut(index)
                .map(|a| &mut a.announcement.media),
            Source::Tail(index) => self
                .tails
                .items
                .get_mut(index)
                .map(|item| &mut item.tail.media),
        }
    }

    /// Satisfy the selected ID and lower priorities through the existing timing policy.
    fn satisfy_identifier(&mut self, index: usize) {
        self.identifiers.complete(&self.rules, index, self.now);
        let priority = self.ids[index].priority;
        for (i, (id, satisfied)) in self.ids.iter().zip(&mut self.satisfied).enumerate() {
            if i == index || id.priority < priority {
                *satisfied = self.now;
            }
        }
    }

    /// Finish exactly one attempt; cancellation never advances successful cooldowns.
    fn complete_playback(&mut self, source: Option<Source>, outcome: PlaybackOutcome) {
        match source {
            Some(Source::Identifier(index)) if outcome == PlaybackOutcome::Succeeded => {
                self.satisfy_identifier(index);
            }
            Some(Source::Announcement(index)) if outcome == PlaybackOutcome::Succeeded => {
                self.announcements[index].satisfied = self.now;
            }
            Some(Source::Identifier(index)) => {
                self.identifiers.defer(&self.rules, index, self.now);
            }
            Some(Source::Announcement(index)) => {
                let a = &mut self.announcements[index];
                a.retry_after = self.now.saturating_add(samples(a.announcement.interval_ms));
            }
            Some(Source::Status) => {
                if outcome == PlaybackOutcome::Succeeded {
                    if let Some(recipe) = self.telemetry.status.as_ref().and_then(|media| media.8) {
                        if let Some(index) =
                            self.ids.iter().position(|id| id.media.8 == Some(recipe))
                        {
                            self.ids[index].media.0.cancel();
                            self.satisfy_identifier(index);
                        }
                        if let Some(index) = self
                            .tails
                            .items
                            .iter()
                            .position(|tail| tail.tail.media.8 == Some(recipe))
                        {
                            self.tails.items[index].tail.media.0.cancel();
                            self.tails.complete(index, self.now, true);
                        }
                    }
                }
                self.telemetry.finish_status();
            }
            Some(Source::Parrot) => self.telemetry.finish_parrot(),
            Some(Source::Tail(index)) => {
                self.tails
                    .complete(index, self.now, outcome == PlaybackOutcome::Succeeded);
            }
            _ => {}
        }
    }

    fn step(
        &mut self,
        receiving: bool,
        linked: bool,
        source_activity: bool,
        input: Option<f32>,
        link_sample: f32,
        parrot_output: Option<ProgramSample<'_>>,
    ) -> f32 {
        if self.telemetry.cancel_message.swap(false, Ordering::AcqRel) {
            let source = self.telemetry.active.take();
            if let Some(media) = self.media(source) {
                media.0.cancel();
                self.complete_playback(source, PlaybackOutcome::Cancelled);
            }
        }
        self.telemetry.poll_status();
        let receive = receiving || linked;
        let status_idle = !receive
            && !source_activity
            && self
                .telemetry
                .peer_status()
                .is_none_or(|peer| !peer.active());
        let active_status = self.telemetry.active == Some(Source::Status);
        if let Some(media) = self.telemetry.status.as_mut().filter(|media| {
            !status_idle && active_status && !media.0.has_output() && !media.force_morse()
        }) {
            // Retain the message, but release its producer and playback slot.
            // A new attempt after unkey must not block an identifier deadline.
            media.0.restart(false);
            media.0.defer(false);
            self.telemetry.active = None;
        }
        let idle = self.now.saturating_sub(self.last_activity);
        if receive {
            self.ctcss_hang_until = self
                .now
                .saturating_add(1)
                .saturating_add(samples(self.settings.ctcss_hang_ms));
            if !self.receiver && !self.linked {
                self.key_idle = idle;
                self.suppress_release = false;
            }
            self.last_activity = self.now;
            self.activity.publish(self.now);
            self.identifiers.activity();
        }
        if receiving && !self.receiver {
            self.receiver_key = self.now;
            self.courtesy.cancel("");
        }
        if !receiving && self.receiver {
            self.receiver_unkey = self.now;
            let kerchunk = self.settings.kerchunk_max_ms != 0
                && self.now.saturating_sub(self.receiver_key)
                    <= samples(self.settings.kerchunk_max_ms);
            self.receive_unkeyed(kerchunk);
            if !kerchunk {
                self.courtesy.schedule(
                    "",
                    "",
                    self.now
                        .saturating_add(samples(self.settings.courtesy_delay_ms)),
                );
            }
        }
        if self.suppress_release && !receive && (self.receiver || self.linked) {
            // Clear only the final suppressed release, not other ongoing traffic's opportunity.
            let _ = self.courtesy.pop();
            for announcement in &mut self.announcements {
                if announcement.announcement.interval_ms == 0 {
                    announcement.release_pending = false;
                }
            }
        }
        self.tails.observe(
            receive || source_activity,
            self.now,
            samples(self.settings.hang_ms),
            self.suppress_release,
        );
        if let Some(Source::Tail(index)) = self.telemetry.active {
            if !self.tails.items[index].started && !self.tails.pending() {
                self.telemetry.active = None;
            }
        }
        self.receiver = receiving;
        self.linked = linked;
        let inhibited = self.telemetry.site_inhibit.load(Ordering::Acquire);
        let may_transmit = !inhibited && (self.settings.full_duplex || !receiving);
        let courtesy_ready = !receive && self.courtesy.ready(self.now);
        self.telemetry.expire_status(
            self.now,
            may_transmit,
            courtesy_ready,
            status_idle && self.now.saturating_sub(self.receiver_unkey) >= 12_000,
        );
        self.telemetry.poll_status();
        let active_before = self.telemetry.active;
        let peer_before = active_before == Some(Source::Status)
            && self
                .telemetry
                .status
                .as_ref()
                .is_some_and(|media| media.3.is_some() && media.7.is_none());
        if source_activity && self.telemetry.parrot_pending() {
            self.telemetry.discard_pending_parrot();
        }
        if receive
            || (!peer_before && matches!(active_before, Some(Source::Status | Source::Courtesy(_))))
        {
            self.release_pending = false;
        }
        let announcement_due = self.announcements.iter().position(|a| a.due(self.now));
        let ordinary = receive
            || (!peer_before
                && matches!(
                    active_before,
                    Some(Source::Status | Source::Courtesy(_) | Source::Identifier(_))
                ));
        let release_expired = self.keyed
            && !ordinary
            && active_before.is_none()
            && self.now.saturating_sub(self.last_audio) >= self.release_hang;
        if let Some(index) = announcement_due {
            if release_expired
                || (!self.keyed
                    && !receive
                    && self
                        .announcements
                        .get(index)
                        .is_some_and(|a| a.announcement.interval_ms != 0))
            {
                self.release_pending = true;
            }
        }
        let pending_status =
            self.telemetry.status_pending() && self.telemetry.peer_status().is_none();
        let peer_ready = self
            .telemetry
            .peer_status()
            .is_some_and(|peer| !peer.active());
        let status_ready = self.telemetry.status_morse()
            || (status_idle
                && (peer_ready
                    || (self.now.saturating_sub(self.receiver_unkey) >= 12_000 && pending_status)));
        let parrot_ready = !source_activity && self.telemetry.parrot_pending();
        if (may_transmit || peer_ready) && self.telemetry.active.is_none() {
            if courtesy_ready {
                self.telemetry.active = self.courtesy.pop().and_then(|index| {
                    self.courtesy.media.get_mut(index).map(|media| {
                        media.0.restart(false);
                        Source::Courtesy(index)
                    })
                });
            } else if status_ready {
                self.telemetry.start_status();
            } else if parrot_ready {
                self.telemetry.start_parrot();
                self.telemetry.active = self.telemetry.parrot.as_ref().map(|_| Source::Parrot);
            }
        }
        let peer_status = self.telemetry.active == Some(Source::Status)
            && self
                .telemetry
                .status
                .as_ref()
                .is_some_and(|media| media.3.is_some() && media.7.is_none());
        let local_telemetry = self.telemetry.active.is_some() && !peer_status;
        let busy = receive
            || source_activity
            || pending_status
            || parrot_ready
            || self.courtesy.pending()
            || self.telemetry.active.is_some()
            || self.tails.pending()
            || self.tails.hanging(self.now);
        let after_hang =
            announcement_due.is_none() || !self.keyed || release_expired || self.release_pending;
        let mut selected = self.select_identifier(receiving, busy, after_hang);
        let mut ready = selected.is_some();
        let active_stream = self
            .media(active_before)
            .is_some_and(|media| media.0.is_streaming());
        let selected_stream = selected
            .and_then(|index| self.ids.get(index))
            .is_some_and(|id| id.media.0.is_streaming());
        let regeneration = parrot_output
            .as_ref()
            .and_then(|output| output.regeneration);
        let demand = regeneration.is_some() || linked
            || (self.settings.full_duplex && receiving)
            || (local_telemetry && !active_stream)
            || self.courtesy.pending()
            || parrot_ready
            // A ready status has already become active above when transmission is allowed.
            || (input.is_some() && ready && !selected_stream)
            || (self.release_pending && announcement_due.is_some());
        if may_transmit && demand && !self.keyed {
            let key_idle = self.key_idle.max(idle);
            if let Some(index) = self.identifiers.first_key(&self.rules, key_idle) {
                self.first_due[index] = self.now;
            }
            self.key_idle = 0;
            selected = self.select_identifier(receiving, busy, after_hang);
            ready = selected.is_some();
        }
        if may_transmit && ready && input.is_some() {
            if let Some(Source::Tail(index)) = self.telemetry.active {
                let item = &mut self.tails.items[index];
                if item.started {
                    item.tail.media.0.rewind();
                    item.suspended = true;
                } else {
                    item.tail.media.0.cancel();
                }
                self.telemetry.active = None;
            }
        }
        if input.is_some() && may_transmit && self.telemetry.active.is_none() {
            if ready {
                self.telemetry.active = selected.and_then(|index| {
                    self.ids.get_mut(index).map(|id| {
                        id.media.0.restart(receive);
                        Source::Identifier(index)
                    })
                });
            } else if let Some(index) = self.tails.ready(self.now) {
                let item = &mut self.tails.items[index];
                if !item.suspended {
                    // A fresh tail is ready only after all traffic has released.
                    item.tail.media.0.restart(false);
                }
                item.suspended = false;
                self.telemetry.active = Some(Source::Tail(index));
            } else if self.release_pending && !pending_status && !self.courtesy.pending() {
                self.telemetry.active = announcement_due.and_then(|index| {
                    self.announcements.get_mut(index).map(|a| {
                        a.release_pending = false;
                        a.announcement.media.0.restart(false);
                        Source::Announcement(index)
                    })
                });
            }
        }
        let rendered_source = self.telemetry.active;
        let policy = self.media(rendered_source).and_then(|media| media.7);
        let exempt = matches!(rendered_source, Some(Source::Tail(_)))
            || (rendered_source == Some(Source::Status) && policy.is_some());
        let reply_target = if peer_status {
            self.telemetry
                .peer_status()
                .map(|peer| (peer.identity(), peer.active()))
        } else {
            None
        };
        self.ctcss_enabled = if !self.settings.ctcss_encode_on_input {
            true
        } else {
            match rendered_source {
                Some(Source::Status) if !peer_status => true,
                Some(_) => false,
                None => pending_status || receive || self.now < self.ctcss_hang_until,
            }
        };
        let ordinary_playback_allowed = self.timeout.allows_playback(
            self.traffic_keyed,
            receive,
            self.now,
            samples(self.settings.transmit_timeout_ms),
        );
        let local_playback_allowed = may_transmit && (exempt || ordinary_playback_allowed);
        let force_morse = rendered_source == Some(Source::Status) && self.telemetry.status_morse();
        let playback = self
            .media(rendered_source)
            .map(|media| (&mut media.0, media.6.as_deref()));
        let mut telemetry_sample = [0.0];
        let mut made = 0;
        let mut waiting_for_media = false;
        let mut continuing_media = false;
        if let Some((playback, occurrence)) = playback {
            let first_output = !playback.has_output();
            let interrupt = force_morse
                || ((receive || source_activity)
                    && matches!(
                        rendered_source,
                        Some(
                            Source::Identifier(_)
                                | Source::Announcement(_)
                                | Source::Tail(_)
                                | Source::Status
                        )
                    ))
                || (source_activity && rendered_source == Some(Source::Parrot));
            if (if peer_status {
                peer_ready
            } else {
                local_playback_allowed
            }) && input.is_some()
            {
                made = playback.render(interrupt, &mut telemetry_sample);
            } else {
                playback.defer(interrupt);
            }
            if first_output && made != 0 && occurrence.is_some_and(|occurrence| !occurrence.begin())
            {
                playback.cancel();
                telemetry_sample[0] = 0.0;
                made = 0;
            }
            waiting_for_media = playback.waiting_for_stream();
            // Preparation alone does not key. Once audible, a composition owns
            // PTT through pending segment boundaries until completion or failure.
            continuing_media = playback.has_output() && !playback.is_finished();
            if playback.is_finished() {
                let outcome = playback.outcome();
                self.telemetry.active = None;
                self.complete_playback(rendered_source, outcome);
            }
        }
        if let Some(Source::Tail(index)) = self.telemetry.active {
            self.tails.items[index].started |= made != 0;
        }
        if !self.announcements.iter().any(|a| a.due(self.now))
            && !matches!(self.telemetry.active, Some(Source::Announcement(_)))
        {
            self.release_pending = false;
        }
        let telemetry_audio = made != 0 && !peer_status;
        let message_audio = exempt && (telemetry_audio || (continuing_media && !peer_status));
        let suppress_repeat = policy.is_some_and(|policy| !policy.repeat_during_playback)
            && (made != 0 || continuing_media);
        let short_tail = matches!(
            rendered_source,
            Some(Source::Identifier(_) | Source::Announcement(_) | Source::Tail(_))
        );
        let other = linked
            || (self.settings.full_duplex && receiving)
            || self.courtesy.pending()
            || (status_ready && !peer_status && !exempt);
        let transmit = regeneration.is_some()
            || other
            || (telemetry_audio && !exempt)
            || (continuing_media && !peer_status && !exempt)
            || (self.telemetry.active.is_some()
                && !peer_status
                && !exempt
                && !waiting_for_media
                && !active_stream);
        let hang = if short_tail && !other {
            2400
        } else {
            samples(self.settings.hang_ms)
        };
        let mut requested = self.duplex.update(
            self.settings.full_duplex,
            receiving,
            transmit,
            self.now,
            hang,
        );
        if may_transmit && self.traffic_keyed && self.tails.hanging(self.now) {
            requested = true;
        }
        if !transmit
            && self.telemetry.active.is_none()
            && self.tails.waiting_for_unkey(self.now)
            && self.now.saturating_sub(self.last_audio) >= self.release_hang.min(2400)
        {
            requested = false;
        }
        if may_transmit && self.traffic_keyed && self.tails.ready(self.now).is_some() && !exempt {
            requested = true;
        }
        self.traffic_keyed = self.timeout.apply(
            requested && !inhibited,
            self.traffic_keyed,
            receive,
            self.now,
            samples(self.settings.transmit_timeout_ms),
            samples(self.settings.timeout_lockout_ms),
        );
        if !self.traffic_keyed {
            self.duplex = DuplexPolicy::default();
        }
        if transmit && may_transmit && self.traffic_keyed {
            self.last_audio = self.now;
            self.release_hang = hang;
            self.exempt_release = false;
        }
        if message_audio && may_transmit {
            self.last_audio = self.now;
            self.release_hang = 2400;
            self.exempt_release = !self.traffic_keyed || !other;
        }
        self.keyed = self.traffic_keyed
            || (may_transmit
                && (message_audio
                    || (self.exempt_release
                        && self.now.saturating_sub(self.last_audio) < self.release_hang)));
        if !self.keyed {
            self.exempt_release = false;
            self.tails.unkeyed(self.now);
        }
        if self.keyed
            && !self.suppress_release
            && (ordinary
                || (telemetry_audio
                    && !matches!(
                        rendered_source,
                        Some(Source::Announcement(_) | Source::Tail(_))
                    )))
        {
            for announcement in &mut self.announcements {
                if announcement.announcement.interval_ms == 0 {
                    announcement.release_pending = true;
                }
            }
        }
        let gain = if input.is_some() {
            self.telemetry.gain_step(receive)
        } else {
            self.telemetry.gain
        };
        let local =
            if self.settings.full_duplex && receiving && self.traffic_keyed && !suppress_repeat {
                input.unwrap_or(0.0)
            } else {
                0.0
            };
        let link = if may_transmit && linked && self.traffic_keyed && !suppress_repeat {
            link_sample
        } else {
            0.0
        };
        let telemetry = if peer_status {
            0.0
        } else {
            telemetry_sample[0] * gain
        };
        if let Some(output) = parrot_output {
            if rendered_source == Some(Source::Parrot) && made != 0 {
                *output.parrot_active = true;
            }
            *output.parrot = if rendered_source == Some(Source::Parrot) && made != 0 {
                telemetry
            } else {
                0.0
            };
            if let Some(reply) = output.reply {
                *reply = PeerReplySample {
                    suppress_repeat,
                    broadcast: (made != 0
                        && policy.is_some_and(|policy| {
                            policy.destination == PlaybackDestination::LocalAndLinks
                        }))
                    .then_some(telemetry_sample[0] * gain),
                    ..Default::default()
                };
                if let Some((target, false)) = reply_target {
                    if made != 0 {
                        reply.target = target;
                        reply.sample = telemetry_sample[0];
                    }
                }
            }
        }
        if let Some(sample) = regeneration {
            if may_transmit && self.traffic_keyed {
                sample
            } else {
                0.0
            }
        } else if inhibited {
            0.0
        } else {
            (local + link + telemetry).clamp(-1.0, 1.0)
        }
    }
}

#[cfg(test)]
mod tests;
