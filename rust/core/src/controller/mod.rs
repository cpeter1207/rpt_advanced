//! Node transmit ownership and serialized prepared telemetry at 48 kHz.

mod announcement;
mod courtesy;
mod telemetry;
mod timeout;

use crate::policy::{DuplexPolicy, IdentifierPolicy, IdentifierRule};
pub use announcement::Announcement;
use announcement::AnnouncementState;
use courtesy::CourtesyPlanner;
pub use courtesy::CourtesySettings;
pub use telemetry::{
    ActivitySnapshot, ControllerControl, ControllerError, MorseSettings, PreparedMedia,
    StatusRejected,
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
    /// Trigger only on a qualifying transmitter rising edge.
    pub first_key_only: bool,
    /// Periodic IDs can run without qualifying receiver activity.
    pub regardless_of_activity: bool,
    /// Bounded polite deferral; `None` disables deferral.
    pub polite_maximum_wait_ms: Option<u64>,
}

/// Resolved node timing and transmit settings; native audio is always 48 kHz.
#[derive(Default)]
pub struct ControllerSettings {
    /// Allow local receive while transmitting and repeat local receive audio.
    pub full_duplex: bool,
    /// Ordinary transmitter hang time.
    pub hang_ms: u64,
    /// Maximum continuous keyed interval without any source unkey; zero disables.
    pub transmit_timeout_ms: u64,
    /// Post-timeout lockout; recovery additionally requires receive inactivity.
    pub timeout_lockout_ms: u64,
    /// Suppress tail telemetry for local transmissions at most this long; zero disables.
    pub kerchunk_max_ms: u64,
    /// Per-source courtesy delay after unkey.
    pub courtesy_delay_ms: u64,
    /// Receive-active attenuation from -60 through 0 dB.
    pub telemetry_duck_db: i8,
    /// Status Morse fallback parameters.
    pub status_morse: MorseSettings,
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
    last_audio: u64,
    release_hang: u64,
    release_pending: bool,
    suppress_release: bool,
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
            settings.transmit_timeout_ms,
            settings.timeout_lockout_ms,
            settings.kerchunk_max_ms,
            settings.courtesy_delay_ms,
        ];
        if !(-60..=0).contains(&settings.telemetry_duck_db)
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
        let (telemetry, control) =
            TelemetryPlanner::new(settings.status_morse, settings.telemetry_duck_db);
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
                        release_pending: false,
                    })
                    .collect(),
                timeout: TimeoutPolicy::default(),
                activity: ActivitySnapshot::new(),
                now: 0,
                receiver: false,
                linked: false,
                receiver_key: 0,
                receiver_unkey: 0,
                last_activity: 0,
                key_idle: 0,
                keyed: false,
                last_audio: 0,
                release_hang: 0,
                release_pending: false,
                suppress_release: false,
            },
            control,
        ))
    }

    /// Clone the atomic receive-only activity handle outside audio processing.
    #[must_use]
    pub fn activity(&self) -> ActivitySnapshot {
        self.activity.clone()
    }

    /// Cancel only the resumed direct peer's pending courtesy; active playback continues.
    pub fn link_keyed(&mut self, direct: &str) {
        if !direct.is_empty() {
            self.courtesy.cancel(direct);
        }
    }

    /// Apply a direct-peer unkey on the audio owner, selecting advisory/direct/generic media.
    pub fn link_unkeyed(&mut self, direct: &str, selected: &str, kerchunk: bool) {
        self.timeout.unkey(self.now);
        if kerchunk {
            self.suppress_release = true;
        } else if !direct.is_empty() {
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
        self.step(receiving, linked, None, 0.0);
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
        self.process_audio_outputs(receiving, linked, link_audio, audio, None)
    }

    /// Render RF and locally generated program in one native sample loop.
    /// Program excludes received peer/local PCM and downstream RF access tones.
    /// Unequal output lengths reject the block without advancing controller state.
    pub fn process_audio_with_program(
        &mut self,
        receiving: bool,
        linked: bool,
        link_audio: &[f32],
        audio: &mut [f32],
        program: &mut [f32],
    ) -> Result<bool, ControllerError> {
        if program.len() != audio.len() {
            return Err(ControllerError);
        }
        Ok(self.process_audio_outputs(receiving, linked, link_audio, audio, Some(program)))
    }

    fn process_audio_outputs(
        &mut self,
        receiving: bool,
        linked: bool,
        link_audio: &[f32],
        audio: &mut [f32],
        mut program: Option<&mut [f32]>,
    ) -> bool {
        if audio.is_empty() {
            return self.process_event(receiving, linked);
        }
        for (offset, sample) in audio.iter_mut().enumerate() {
            let (rf, generated) = self.step(
                receiving,
                linked,
                Some(*sample),
                link_audio.get(offset).copied().unwrap_or(0.0),
            );
            *sample = rf;
            if let Some(output) = program.as_mut() {
                output[offset] = generated;
            }
            self.now = self.now.saturating_add(1);
        }
        self.keyed
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

    fn step(
        &mut self,
        receiving: bool,
        linked: bool,
        input: Option<f32>,
        link_sample: f32,
    ) -> (f32, f32) {
        let receive = receiving || linked;
        let idle = self.now.saturating_sub(self.last_activity);
        if receive {
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
            self.timeout.unkey(self.now);
            let kerchunk = self.settings.kerchunk_max_ms != 0
                && self.now.saturating_sub(self.receiver_key)
                    <= samples(self.settings.kerchunk_max_ms);
            if kerchunk {
                self.suppress_release = true;
            } else {
                self.courtesy.schedule(
                    "",
                    "",
                    self.now
                        .saturating_add(samples(self.settings.courtesy_delay_ms)),
                );
            }
        }
        self.receiver = receiving;
        self.linked = linked;
        let may_transmit = self.settings.full_duplex || !receiving;
        let active_before = self.telemetry.active;
        if receive || matches!(active_before, Some(Source::Status | Source::Courtesy(_))) {
            self.release_pending = false;
        }
        let announcement_due = self.announcements.iter().position(|a| a.due(self.now));
        let ordinary = receive
            || matches!(
                active_before,
                Some(Source::Status | Source::Courtesy(_) | Source::Identifier(_))
            );
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
        let pending_status = self.telemetry.status_pending();
        let status_ready =
            !receive && self.now.saturating_sub(self.receiver_unkey) >= 12_000 && pending_status;
        let courtesy_ready = !receive && self.courtesy.ready(self.now);
        if may_transmit && self.telemetry.active.is_none() {
            if courtesy_ready {
                self.telemetry.active = self.courtesy.pop().and_then(|index| {
                    self.courtesy.media.get_mut(index).map(|media| {
                        media.0.restart(false);
                        Source::Courtesy(index)
                    })
                });
            } else if status_ready {
                self.telemetry.start_status();
            }
        }
        let busy =
            receive || pending_status || self.courtesy.pending() || self.telemetry.active.is_some();
        let after_hang =
            announcement_due.is_none() || !self.keyed || release_expired || self.release_pending;
        let mut selected =
            self.identifiers
                .select(&self.rules, self.now, receiving, self.settings.full_duplex);
        let mut ready = selected.is_some_and(|id| self.id_ready(id, busy)) && after_hang;
        let demand = linked
            || (self.settings.full_duplex && receiving)
            || self.telemetry.active.is_some()
            || self.courtesy.pending()
            // A ready status has already become active above when transmission is allowed.
            || (input.is_some() && ready)
            || (self.release_pending && announcement_due.is_some());
        if may_transmit && demand && !self.keyed {
            let key_idle = self.key_idle.max(idle);
            self.identifiers.first_key(&self.rules, key_idle);
            for (id, due) in self.ids.iter().zip(&mut self.first_due) {
                if id.first_key_only && key_idle >= samples(id.interval_ms) {
                    *due = self.now;
                }
            }
            self.key_idle = 0;
            selected = self.identifiers.select(
                &self.rules,
                self.now,
                receiving,
                self.settings.full_duplex,
            );
            ready = selected.is_some_and(|id| self.id_ready(id, busy));
        }
        if input.is_some() && may_transmit && self.telemetry.active.is_none() {
            if ready {
                self.telemetry.active = selected.and_then(|index| {
                    self.ids.get_mut(index).map(|id| {
                        id.media.0.restart(receive);
                        Source::Identifier(index)
                    })
                });
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
        let playback = match rendered_source {
            Some(Source::Status) => self.telemetry.status.as_mut().map(|m| &mut m.0),
            Some(Source::Courtesy(index)) => self.courtesy.media.get_mut(index).map(|m| &mut m.0),
            Some(Source::Identifier(index)) => self.ids.get_mut(index).map(|id| &mut id.media.0),
            Some(Source::Announcement(index)) => self
                .announcements
                .get_mut(index)
                .map(|a| &mut a.announcement.media.0),
            None => None,
        };
        let mut telemetry_sample = [0.0];
        let mut made = 0;
        if let Some(playback) = playback {
            let interrupt = receive
                && matches!(
                    rendered_source,
                    Some(Source::Identifier(_) | Source::Status)
                );
            if may_transmit && input.is_some() {
                made = playback.render(interrupt, &mut telemetry_sample);
            } else {
                playback.render(interrupt, &mut []);
            }
            if playback.is_finished() {
                self.telemetry.active = None;
                match rendered_source {
                    Some(Source::Identifier(index)) => {
                        self.identifiers.complete(&self.rules, index, self.now);
                        self.ids
                            .get(index)
                            .map(|id| id.priority)
                            .into_iter()
                            .for_each(|priority| {
                                for (i, (id, satisfied)) in
                                    self.ids.iter().zip(&mut self.satisfied).enumerate()
                                {
                                    if i == index || id.priority < priority {
                                        *satisfied = self.now;
                                    }
                                }
                            });
                    }
                    Some(Source::Announcement(index)) => {
                        self.announcements.get_mut(index).into_iter().for_each(|a| {
                            a.satisfied = self.now;
                        });
                    }
                    Some(Source::Status) => self.telemetry.finish_status(),
                    _ => {}
                }
            }
        }
        if !self.announcements.iter().any(|a| a.due(self.now))
            && !matches!(self.telemetry.active, Some(Source::Announcement(_)))
        {
            self.release_pending = false;
        }
        let telemetry_audio = made != 0;
        let short_tail = matches!(
            rendered_source,
            Some(Source::Identifier(_) | Source::Announcement(_))
        );
        let other = linked
            || (self.settings.full_duplex && receiving)
            || self.courtesy.pending()
            || status_ready;
        let transmit = other || telemetry_audio || self.telemetry.active.is_some();
        let hang = if short_tail && !other {
            2400
        } else {
            samples(self.settings.hang_ms)
        };
        let requested = self.duplex.update(
            self.settings.full_duplex,
            receiving,
            transmit,
            self.now,
            hang,
        );
        let was_keyed = self.keyed;
        self.keyed = self.timeout.apply(
            requested,
            was_keyed,
            receive,
            self.now,
            samples(self.settings.transmit_timeout_ms),
            samples(self.settings.timeout_lockout_ms),
        );
        if !self.keyed {
            self.duplex = DuplexPolicy::default();
        }
        if transmit && may_transmit {
            self.last_audio = self.now;
            self.release_hang = hang;
        }
        if self.keyed
            && !self.suppress_release
            && (ordinary
                || (telemetry_audio && !matches!(rendered_source, Some(Source::Announcement(_)))))
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
        let local = if self.settings.full_duplex && receiving {
            input.unwrap_or(0.0)
        } else {
            0.0
        };
        let link = if may_transmit && linked {
            link_sample
        } else {
            0.0
        };
        let program = telemetry_sample[0] * gain;
        ((local + link + program).clamp(-1.0, 1.0), program)
    }
}

#[cfg(test)]
mod tests;
