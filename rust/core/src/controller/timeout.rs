//! Sample-timed traffic watchdog and local-only, producer-backed recovery notices.

use super::{NodeController, PeerReplySample, PreparedMedia, ProgramSample};

#[derive(Default)]
enum Phase {
    #[default]
    Normal,
    Expired,
    Waiting,
    Cleared,
}

#[derive(Default)]
pub(super) struct TimeoutPolicy {
    key: u64,
    phase: Phase,
}
impl TimeoutPolicy {
    /// Do not consume local telemetry that this tick's watchdog will suppress.
    pub fn allows_playback(&self, was_keyed: bool, now: u64, timeout: u64) -> bool {
        !was_keyed || timeout == 0 || now.saturating_sub(self.key) < timeout
    }

    pub fn blocked(&self) -> bool {
        !matches!(self.phase, Phase::Normal)
    }

    pub fn unkey(&mut self, now: u64) {
        self.key = now;
    }
    pub fn apply(&mut self, requested: bool, was_keyed: bool, now: u64, timeout: u64) -> bool {
        if requested && !was_keyed {
            self.key = now;
        } else if requested && timeout != 0 && now.saturating_sub(self.key) >= timeout {
            self.phase = Phase::Expired;
            return false;
        }
        requested
    }
}

impl NodeController {
    /// Control owns notice registration; the audio owner only borrows these readers.
    pub(crate) fn set_timeout_media(&mut self, media: [PreparedMedia; 2]) {
        self.timeout_media = Some(media);
    }

    /// Release the ordinary producer without satisfying or retiring its interrupted item.
    pub(super) fn begin_timeout(&mut self) {
        let source = self.telemetry.active;
        if let Some(media) = self.media(source) {
            // Terminal cancellation retires one-shot recipes; a hold must leave them restartable.
            media.0.restart(false);
            media.0.defer(false);
        }
        for tail in self.tails.items.iter_mut().filter(|tail| tail.suspended) {
            tail.tail.media.0.restart(false);
            tail.tail.media.0.defer(false);
        }
        if let Some(media) = &mut self.timeout_media {
            for notice in media {
                notice.0.restart(false);
            }
        }
        self.duplex = Default::default();
        self.traffic_keyed = false;
        self.exempt_release = false;
        self.release_pending = false;
        self.release_hang = 0;
    }

    /// Priority notices never repeat input, route to peers, duck, or select Morse on receive.
    /// Detection and ring consumption have already advanced before this local RF gate.
    pub(super) fn timeout_sample(
        &mut self,
        activity: bool,
        inhibited: bool,
        input: Option<f32>,
        output: Option<ProgramSample<'_>>,
    ) -> f32 {
        let _ = self.courtesy.pop();
        if matches!(self.timeout.phase, Phase::Waiting) && !activity {
            self.timeout.phase = Phase::Cleared;
        }
        let index = match self.timeout.phase {
            Phase::Expired => Some(0),
            Phase::Cleared => Some(1),
            _ => None,
        };
        let mut sample = [0.0];
        let mut made = 0;
        let mut continuing = false;
        if let Some(index) = index {
            if let Some(media) = &mut self.timeout_media {
                let playback = &mut media[index].0;
                if !inhibited && input.is_some() {
                    made = playback.render(false, &mut sample);
                }
                continuing = playback.has_output() && !playback.is_finished();
                if playback.is_finished() {
                    self.timeout.phase = if index == 0 {
                        Phase::Waiting
                    } else {
                        Phase::Normal
                    };
                    self.timeout.unkey(self.now);
                }
            } else {
                // A bare controller has no station producer; never latch it on missing media.
                self.timeout.phase = if activity {
                    Phase::Waiting
                } else {
                    Phase::Normal
                };
                self.timeout.unkey(self.now);
            }
        }
        if made != 0 {
            self.last_audio = self.now;
            self.release_hang = 2400;
            self.exempt_release = true;
        }
        self.traffic_keyed = false;
        self.ctcss_enabled = !self.settings.ctcss_encode_on_input;
        self.keyed = !inhibited
            && (made != 0
                || continuing
                || (self.exempt_release
                    && self.now.saturating_sub(self.last_audio) < self.release_hang));
        if !self.keyed {
            self.exempt_release = false;
            self.tails.unkeyed(self.now);
        }
        if let Some(output) = output {
            *output.parrot = 0.0;
            if let Some(reply) = output.reply {
                *reply = PeerReplySample::default();
            }
        }
        sample[0].clamp(-1.0, 1.0)
    }
}
