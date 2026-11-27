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
        let coordinate = |sample| {
            let offset = i128::from(sample) - i128::from(self.source_sample);
            u64::try_from(i128::from(self.rtp_timestamp) + offset).ok()
        };
        Some(coordinate(native.start)?..coordinate(native.end)?)
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
