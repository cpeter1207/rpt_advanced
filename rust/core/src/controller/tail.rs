//! Release opportunities use traffic time, never telemetry's transmitter hang clock.

use super::{ControllerError, PreparedMedia, samples};

/// One configured release message prepared on control, with an explicit audience.
pub struct Tail {
    /// Ordered content and its optional complete Morse fallback.
    pub media: PreparedMedia,
    /// Minimum time since successful completion; zero permits every traffic release.
    pub interval_ms: u64,
    /// Delay from actual controller unkey; zero retains PTT at hang expiry.
    pub delay_ms: u64,
}

pub(super) struct TailState {
    pub tail: Tail,
    pending: bool,
    pub started: bool,
    pub suspended: bool,
    deadline: Option<u64>,
    completed: Option<u64>,
}

/// Preallocated ordered states; generated audio can neither arm nor restart a release.
#[derive(Default)]
pub(super) struct TailPlanner {
    pub items: Vec<TailState>,
    traffic: bool,
    hang_until: Option<u64>,
}

impl TailPlanner {
    pub fn new(tails: Vec<Tail>) -> Result<Self, ControllerError> {
        if tails
            .iter()
            .any(|tail| tail.interval_ms > u64::MAX / 48 || tail.delay_ms > u64::MAX / 48)
        {
            return Err(ControllerError);
        }
        Ok(Self {
            items: tails
                .into_iter()
                .map(|tail| TailState {
                    tail,
                    pending: false,
                    started: false,
                    suspended: false,
                    deadline: None,
                    completed: None,
                })
                .collect(),
            ..Self::default()
        })
    }

    pub fn observe(&mut self, active: bool, now: u64, hang: u64, suppressed: bool) {
        if active {
            self.hang_until = None;
            for item in &mut self.items {
                if item.pending && !item.started {
                    item.tail.media.0.cancel();
                    item.pending = false;
                    item.deadline = None;
                } else if item.suspended {
                    // A receive edge during ID preemption still latches Morse for this attempt.
                    item.tail.media.0.render(true, &mut []);
                }
            }
        } else if self.traffic {
            self.hang_until = (!suppressed).then(|| now.saturating_add(hang));
            for item in &mut self.items {
                if !suppressed
                    && !item.pending
                    && item.completed.is_none_or(|last| {
                        now.saturating_sub(last) >= samples(item.tail.interval_ms)
                    })
                {
                    item.pending = true;
                    item.deadline = None;
                }
            }
        }
        self.traffic = active;
    }

    pub fn hanging(&self, now: u64) -> bool {
        self.hang_until.is_some_and(|deadline| now < deadline)
    }

    pub fn pending(&self) -> bool {
        self.items.iter().any(|item| item.pending)
    }

    pub fn ready(&self, now: u64) -> Option<usize> {
        let (index, item) = self
            .items
            .iter()
            .enumerate()
            .find(|(_, item)| item.pending)?;
        (item.suspended
            || (!self.traffic
                && !self.hanging(now)
                && (item.tail.delay_ms == 0
                    || item.deadline.is_some_and(|deadline| now >= deadline))))
        .then_some(index)
    }

    /// Pending positive delays deliberately release PTT, then start at that actual edge.
    pub fn waiting_for_unkey(&self, now: u64) -> bool {
        !self.traffic
            && !self.hanging(now)
            && self
                .items
                .iter()
                .find(|item| item.pending)
                .is_some_and(|item| {
                    !item.started && item.tail.delay_ms != 0 && item.deadline.is_none()
                })
    }

    pub fn unkeyed(&mut self, now: u64) {
        if self.waiting_for_unkey(now) {
            let item = self
                .items
                .iter_mut()
                .find(|item| item.pending)
                .expect("waiting for unkey requires a pending tail");
            item.deadline = Some(now.saturating_add(samples(item.tail.delay_ms)));
        }
    }

    pub fn complete(&mut self, index: usize, now: u64, succeeded: bool) {
        let item = &mut self.items[index];
        if succeeded {
            item.completed = Some(now);
        }
        item.pending = false;
        item.started = false;
        item.suspended = false;
        item.deadline = None;
    }
}
