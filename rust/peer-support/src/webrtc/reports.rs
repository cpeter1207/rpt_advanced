//! Relative sender clock evidence from authenticated RTCP sender reports.

use serde::Serialize;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use str0m::media::{Frequency, SenderFeedback};
use str0m::rtp::Ssrc;

use super::channel::Coordinate;

#[derive(Clone, Copy)]
struct Report {
    rtp: u64,
    sender_time: SystemTime,
    received: Instant,
}

/// A rational relative clock observation from two reports by the same SSRC.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SenderRate {
    pub ssrc: u32,
    pub rtp_ticks: Coordinate,
    pub sender_elapsed_ns: Coordinate,
    pub receiver_elapsed_ns: Coordinate,
}

/// Reconcile each sender report with RTP observed on the active media source.
/// No estimate from this owner supplies TAI, a capture age or a transmit grant.
#[derive(Default)]
pub struct SenderReports {
    previous: Option<(Ssrc, Report)>,
}

impl SenderReports {
    pub fn record(
        &mut self,
        feedback: &SenderFeedback,
        current_source: Ssrc,
        current_rtp: u64,
    ) -> Option<SenderRate> {
        let packet = &feedback.sender_info;
        if packet.ssrc != current_source
            || packet.rtp_time.frequency() != Frequency::FORTY_EIGHT_KHZ
            || packet.ntp_time <= UNIX_EPOCH
        {
            return None;
        }
        let rtp = extend_near(current_rtp, packet.rtp_time.numer() as u32)?;
        self.update(
            current_source,
            Report {
                rtp,
                sender_time: packet.ntp_time,
                received: feedback.received_at,
            },
        )
    }

    fn update(&mut self, source: Ssrc, current: Report) -> Option<SenderRate> {
        let previous = match self.previous {
            Some((id, previous)) if id == source => previous,
            _ => {
                self.previous = Some((source, current));
                return None;
            }
        };
        if current.received <= previous.received || current.rtp <= previous.rtp {
            return None;
        }
        self.previous = Some((source, current));
        let sender = current
            .sender_time
            .duration_since(previous.sender_time)
            .ok()?;
        let receiver = current.received.duration_since(previous.received);
        let sender_elapsed_ns = u64::try_from(sender.as_nanos()).ok()?;
        let receiver_elapsed_ns = u64::try_from(receiver.as_nanos()).ok()?;
        let ticks = current.rtp - previous.rtp;
        // A step in the peer's wall clock is distinguishable from the ordinary
        // nominal RTP rate. Only the relative interval survives this test.
        let expected = u128::from(sender_elapsed_ns) * 48_000;
        let actual = u128::from(ticks) * 1_000_000_000;
        if expected == 0 || actual.abs_diff(expected) > expected / 100 {
            return None;
        }
        Some(SenderRate {
            ssrc: *source,
            rtp_ticks: Coordinate(ticks),
            sender_elapsed_ns: Coordinate(sender_elapsed_ns),
            receiver_elapsed_ns: Coordinate(receiver_elapsed_ns),
        })
    }
}

fn extend_near(extended: u64, wire: u32) -> Option<u64> {
    let offset = wire.wrapping_sub(extended as u32) as i32;
    if offset == i32::MIN {
        return None;
    }
    extended.checked_add_signed(i64::from(offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sender_intervals_ignore_clock_steps_and_stale_reports_across_wrap() {
        let now = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let crossing = u64::from(u32::MAX) + 1;
        assert_eq!(extend_near(crossing - 10, 10), Some(crossing + 10));
        assert_eq!(extend_near(0, u32::MAX), None);
        assert_eq!(extend_near(0, 1u32 << 31), None);
        let sample = |rtp, sender, receiver| Report {
            rtp,
            sender_time: wall + Duration::from_millis(sender),
            received: now + Duration::from_millis(receiver),
        };
        let mut reports = SenderReports::default();
        let id = Ssrc::from(33);
        assert!(
            reports
                .update(id, sample(crossing - 14_400, 0, 0))
                .is_none()
        );
        let rate = reports
            .update(id, sample(crossing + 33_600, 1000, 1010))
            .unwrap();
        assert_eq!(rate.rtp_ticks, Coordinate(48_000));
        assert_eq!(rate.sender_elapsed_ns, Coordinate(1_000_000_000));
        assert_eq!(rate.receiver_elapsed_ns, Coordinate(1_010_000_000));
        assert!(
            reports
                .update(id, sample(crossing + 33_600, 1000, 1010))
                .is_none()
        );
        assert!(
            reports
                .update(id, sample(crossing + 81_600, 9000, 2010))
                .is_none()
        );
        assert!(
            reports
                .update(id, sample(crossing + 129_600, 10000, 3010))
                .is_some()
        );
        assert!(
            reports
                .update(Ssrc::from(34), sample(1, 10000, 3010))
                .is_none()
        );
    }
}
