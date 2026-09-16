//! Ordered periodic and transmission-release announcements.
use super::PreparedMedia;

/// Prepared announcement and its successful-playback interval.
pub struct Announcement {
    /// Validated speech/file PCM and Morse fallback.
    pub media: PreparedMedia,
    /// Zero plays once after a transmission; positive values also key from idle.
    pub interval_ms: u64,
}

pub(super) struct AnnouncementState {
    pub announcement: Announcement,
    pub satisfied: u64,
    pub release_pending: bool,
}

impl AnnouncementState {
    pub fn due(&self, now: u64) -> bool {
        if self.announcement.interval_ms == 0 {
            self.release_pending
        } else {
            now.saturating_sub(self.satisfied) >= super::samples(self.announcement.interval_ms)
        }
    }
}
