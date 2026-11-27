//! Media coordinates and receive-local sample eligibility for one WebRTC grant.

/// Actual correspondence at accepted native 48 kHz media, within one source
/// generation and RTP media epoch. Both coordinates address the first sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaAnchor {
    pub source_generation: u64,
    pub media_epoch: u64,
    pub source_sample: u64,
    pub rtp_timestamp: u64,
}

/// Map only a known native span. Boundaries are end-exclusive and may be used
/// for meter windows with a verified relationship to this codec input.
impl MediaAnchor {
    pub fn rtp_window(
        self,
        source_generation: u64,
        media_epoch: u64,
        native: std::ops::Range<u64>,
    ) -> Option<std::ops::Range<u64>> {
        if source_generation != self.source_generation
            || media_epoch != self.media_epoch
            || native.is_empty()
        {
            return None;
        }
        Self::window(native, self.source_sample, self.rtp_timestamp)
    }

    /// Map a packet's extended RTP positions back to the owned source epoch.
    /// Expand the result by the established uncertainty before testing a lease.
    pub fn possible_source_window(
        self,
        media_epoch: u64,
        rtp: Range<u64>,
        uncertainty_frames: u64,
    ) -> Option<Range<u64>> {
        if media_epoch != self.media_epoch {
            return None;
        }
        let source = Self::window(rtp, self.rtp_timestamp, self.source_sample)?;
        Some(
            source.start.checked_sub(uncertainty_frames)?
                ..source.end.checked_add(uncertainty_frames)?,
        )
    }

    fn window(range: Range<u64>, from: u64, to: u64) -> Option<Range<u64>> {
        if range.is_empty() {
            return None;
        }
        let coordinate = |position| {
            let offset = i128::from(position) - i128::from(from);
            u64::try_from(i128::from(to) + offset).ok()
        };
        Some(coordinate(range.start)?..coordinate(range.end)?)
    }
}

/// Convert a native sample offset with the rational 48 kHz period. This
/// produces elapsed nanoseconds, without advancing or verifying a clock epoch.
pub fn native_delta_ns(from: u64, to: u64) -> i128 {
    ((i128::from(to) - i128::from(from)) * 1_000_000_000).div_euclid(48_000)
}

use std::{
    ops::Range,
    time::{Duration, Instant},
};

/// Receive-local interpretation of a sample-key lease after clock validation.
pub struct ReceiveLease {
    pub grant: u64,
    pub media_epoch: u64,
    pub sequence: u64,
    pub samples: Range<u64>,
    pub expires_at: Instant,
}

/// One authorized talk grant. Expiry or revocation permanently closes this grant.
pub struct ReceiveGate {
    grant: u64,
    media_epoch: u64,
    maximum_lifetime: Duration,
    accepted: Option<ReceiveLease>,
    revoked: bool,
}

impl ReceiveGate {
    pub fn new(grant: u64, media_epoch: u64, maximum_lifetime: Duration) -> Self {
        Self {
            grant,
            media_epoch,
            maximum_lifetime,
            accepted: None,
            revoked: false,
        }
    }

    pub fn update(&mut self, now: Instant, lease: ReceiveLease) -> bool {
        self.expire(now);
        if self.revoked
            || lease.grant != self.grant
            || lease.media_epoch != self.media_epoch
            || lease.expires_at <= now
            || lease.expires_at.duration_since(now) > self.maximum_lifetime
            || lease.samples.is_empty()
            || self
                .accepted
                .as_ref()
                .is_some_and(|last| lease.sequence <= last.sequence)
        {
            return false;
        }
        self.accepted = Some(lease);
        true
    }

    /// Check the entire possible sample span, including timing uncertainty.
    /// An unknown or ambiguous mapping never qualifies for routing.
    pub fn permits(&mut self, now: Instant, possible_samples: Option<Range<u64>>) -> bool {
        self.expire(now);
        self.accepted
            .as_ref()
            .zip(possible_samples)
            .is_some_and(|(lease, span)| {
                !span.is_empty()
                    && lease.samples.start <= span.start
                    && span.end <= lease.samples.end
            })
    }

    /// The receiver's timer must be able to release without waiting for media.
    pub fn deadline(&self) -> Option<Instant> {
        self.accepted.as_ref().map(|lease| lease.expires_at)
    }

    /// Process the receiver-local deadline and return whether this grant is open.
    pub fn current(&mut self, now: Instant) -> bool {
        self.expire(now);
        !self.revoked && self.accepted.is_some()
    }

    pub fn revoke(&mut self) {
        self.accepted = None;
        self.revoked = true;
    }

    fn expire(&mut self, now: Instant) {
        if self
            .accepted
            .as_ref()
            .is_some_and(|lease| now >= lease.expires_at)
        {
            self.revoke();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_packet_must_fit_the_entire_uncertain_source_window() {
        let now = Instant::now();
        let expires_at = now + Duration::from_millis(20);
        let first_rtp = u64::from(u32::MAX) - 119;
        let anchor = MediaAnchor {
            source_generation: 1,
            media_epoch: 3,
            source_sample: 2000,
            rtp_timestamp: first_rtp,
        };
        let packet = first_rtp..first_rtp + 960;
        let mut gate = ReceiveGate::new(10, 3, Duration::from_millis(20));
        assert!(gate.update(
            now,
            ReceiveLease {
                grant: 10,
                media_epoch: 3,
                sequence: 1,
                samples: 1999..2961,
                expires_at,
            }
        ));
        assert_eq!(gate.deadline(), Some(expires_at));
        assert!(gate.permits(now, anchor.possible_source_window(3, packet.clone(), 1)));
        assert!(!gate.permits(now, anchor.possible_source_window(3, packet.clone(), 2)));
        assert!(!gate.permits(now, anchor.possible_source_window(2, packet, 0)));
        assert!(!gate.current(expires_at));
        assert_eq!(gate.deadline(), None);
    }

    #[test]
    fn only_a_current_grant_can_admit_an_entire_known_sample_span() {
        let now = Instant::now();
        let tick = |millis| now + Duration::from_millis(millis);
        let lease = |grant, media_epoch, sequence, expires_at| ReceiveLease {
            grant,
            media_epoch,
            sequence,
            samples: 1_000..2_000,
            expires_at,
        };
        let mut gate = ReceiveGate::new(2, 3, Duration::from_millis(15));
        assert!(!gate.update(tick(0), lease(2, 4, 1, tick(15))));
        assert!(!gate.update(tick(0), lease(2, 3, 1, tick(16))));
        assert!(gate.update(tick(0), lease(2, 3, 1, tick(10))));
        assert!(!gate.permits(tick(1), None));
        assert!(!gate.permits(tick(1), Some(999..1_100)));
        assert!(gate.permits(tick(1), Some(1_000..2_000)));
        assert!(!gate.update(tick(2), lease(2, 3, 1, tick(30))));
        assert!(gate.update(tick(2), lease(2, 3, 2, tick(12))));
        assert!(!gate.permits(tick(12), Some(1_000..1_100)));
        assert!(!gate.update(tick(13), lease(2, 3, 3, tick(20))));
        let mut new_burst = ReceiveGate::new(4, 3, Duration::from_millis(15));
        assert!(new_burst.update(tick(13), lease(4, 3, 1, tick(23))));
        new_burst.revoke();
        assert!(!new_burst.permits(tick(14), Some(1_000..1_100)));
    }
}
