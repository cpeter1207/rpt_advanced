//! Source-specific delayed courtesy selection and cancellation.
use super::PreparedMedia;

/// Control-prepared local, generic-link, and exact-peer courtesy media.
#[derive(Default)]
pub struct CourtesySettings {
    /// Local-receiver courtesy.
    pub receiver: Option<PreparedMedia>,
    /// Generic linked-source courtesy.
    pub link: Option<PreparedMedia>,
    /// Advisory identity wins, then direct identity, then the generic link media.
    pub peers: Vec<(String, PreparedMedia)>,
}

#[derive(Clone, Copy)]
struct Pending {
    source: [u8; 64],
    media: usize,
    due: u64,
}

pub(super) struct CourtesyPlanner {
    pub media: Vec<PreparedMedia>,
    peers: Vec<(String, usize)>,
    receiver: Option<usize>,
    link: Option<usize>,
    queue: [Option<Pending>; 16],
    len: usize,
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
            queue: [None; 16],
            len: 0,
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
        let mut retained = 0;
        // Copy the fixed-size active prefix so compaction cannot overwrite an unread item.
        for item in self.queue.into_iter().take(self.len).flatten() {
            if item.source != key {
                self.queue[retained] = Some(item);
                retained += 1;
            }
        }
        self.len = retained;
    }
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
        if let (Some(media), Some(slot)) = (chosen, self.queue.get_mut(self.len)) {
            *slot = Some(Pending {
                source: key,
                media,
                due,
            });
            self.len += 1;
        }
    }
    pub fn pending(&self) -> bool {
        self.len != 0
    }
    pub fn ready(&self, now: u64) -> bool {
        self.len != 0
            && self
                .queue
                .first()
                .copied()
                .flatten()
                .is_some_and(|p| now >= p.due)
    }
    pub fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let selected = self.queue.first().copied().flatten()?.media;
        self.queue.copy_within(1..self.len, 0);
        self.len -= 1;
        Some(selected)
    }
}
