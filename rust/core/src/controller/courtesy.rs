//! Last-unkey courtesy selection with source-specific rekey cancellation.
use super::PreparedMedia;
use crate::link::PhoneCourtesyKey;

/// Control-prepared local, generic-link, and exact-peer courtesy media.
#[derive(Default)]
pub struct CourtesySettings {
    /// Local-receiver courtesy.
    pub receiver: Option<PreparedMedia>,
    /// Generic linked-source courtesy.
    pub link: Option<PreparedMedia>,
    /// Local-only private telephone speech courtesy. Absent means off.
    pub phone: Option<PreparedMedia>,
    /// Advisory identity wins, then direct identity, then the generic link media.
    pub peers: Vec<(String, PreparedMedia)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CourtesySource {
    Public([u8; 64]),
    Phone(PhoneCourtesyKey),
}

#[derive(Clone, Copy)]
struct Pending {
    source: CourtesySource,
    media: usize,
    due: u64,
}

pub(super) struct CourtesyPlanner {
    pub media: Vec<PreparedMedia>,
    peers: Vec<(String, usize)>,
    receiver: Option<usize>,
    link: Option<usize>,
    phone: Option<usize>,
    pending: Option<Pending>,
}

impl CourtesyPlanner {
    pub fn new(settings: CourtesySettings) -> Self {
        let mut media = Vec::new();
        let receiver = settings.receiver.filter(|m| m.1).map(|m| {
            media.push(m);
            media.len() - 1
        });
        let link = settings.link.filter(|m| m.1).map(|m| {
            media.push(m);
            media.len() - 1
        });
        let phone = settings.phone.filter(|m| m.1).map(|m| {
            media.push(m);
            media.len() - 1
        });
        let peers = settings
            .peers
            .into_iter()
            .filter(|(_, m)| m.1)
            .map(|(name, m)| {
                media.push(m);
                (name, media.len() - 1)
            })
            .collect();
        Self {
            media,
            peers,
            receiver,
            link,
            phone,
            pending: None,
        }
    }
    fn identity(source: &str) -> Option<[u8; 64]> {
        if source.len() >= 64 {
            return None;
        }
        let mut key = [0; 64];
        key.get_mut(..source.len())?
            .copy_from_slice(source.as_bytes());
        Some(key)
    }
    pub fn cancel(&mut self, source: &str) {
        let Some(key) = Self::identity(source) else {
            return;
        };
        if self
            .pending
            .is_some_and(|item| item.source == CourtesySource::Public(key))
        {
            self.pending = None;
        }
    }
    /// Replace any waiting courtesy with the latest unkey, including an unconfigured tone.
    pub fn schedule(&mut self, source: &str, selected: &str, due: u64) {
        let Some(key) = Self::identity(source) else {
            return;
        };
        let chosen = if source.is_empty() {
            self.receiver
        } else {
            [selected, source]
                .iter()
                .find_map(|name| {
                    self.peers
                        .iter()
                        .find(|(peer, _)| peer == name)
                        .map(|(_, index)| *index)
                })
                .or(self.link)
        };
        self.pending = chosen.map(|media| Pending {
            source: CourtesySource::Public(key),
            media,
            due,
        });
    }
    /// Latest genuine private closure uses its own prepared media, not link fallback.
    pub fn schedule_phone(&mut self, key: PhoneCourtesyKey, due: u64) {
        if key.valid() {
            self.pending = self.phone.map(|media| Pending {
                source: CourtesySource::Phone(key),
                media,
                due,
            });
        }
    }
    /// Cancel only the matching private input lifetime.
    pub fn cancel_phone(&mut self, key: PhoneCourtesyKey) {
        if self
            .pending
            .is_some_and(|item| item.source == CourtesySource::Phone(key))
        {
            self.pending = None;
        }
    }
    pub fn pending_phone(&self) -> Option<PhoneCourtesyKey> {
        match self.pending?.source {
            CourtesySource::Phone(key) => Some(key),
            CourtesySource::Public(_) => None,
        }
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn ready(&self, now: u64) -> bool {
        self.pending.is_some_and(|p| now >= p.due)
    }
    pub fn pop(&mut self) -> Option<usize> {
        self.pending.take().map(|p| p.media)
    }
}
