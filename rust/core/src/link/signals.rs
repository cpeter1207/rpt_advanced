//! Bounded lock-free snapshots between one audio consumer and one channel owner.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

/// One configured node's retained programme invalidation domain. The node owner
/// reuses this small allocation across prepared mixer generations; unrelated nodes
/// never invalidate it. Saturation is permanently unavailable rather than wrapping.
#[derive(Clone, Debug, Default)]
pub struct RetirementDomain(Arc<AtomicU64>);
impl RetirementDomain {
    /// Whether two prepared owners share the same configured-node domain.
    pub fn same_domain(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Snapshot only; this is not evidence that a source contributed to RF.
    pub fn revision(&self) -> Option<u64> {
        let revision = self.0.load(Ordering::Acquire);
        (revision != u64::MAX).then_some(revision)
    }
    fn invalidate(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |revision| {
                revision.checked_add(1)
            });
    }
}

#[derive(Debug)]
struct RetirementLink {
    domain: RetirementDomain,
    next: OnceLock<Box<RetirementLink>>,
}

const LIVE: u8 = 0;
const RETIRING: u8 = 1;
const RETIRED: u8 = 2;

#[derive(Debug)]
struct ReplyState {
    phone: Option<super::PhoneSource>,
    phone_authenticated: AtomicBool,
    phase: AtomicU8,
    domains: OnceLock<Box<RetirementLink>>,
    radio_keyed: AtomicBool,
    edge: AtomicU64,
    telemetry_allowed: AtomicBool,
    telemetry_until_ms: AtomicU64,
}

/// Exact peer connection retained by control-prepared responses until off-audio reclamation.
#[derive(Clone, Debug)]
pub struct PeerReply(Arc<ReplyState>);
impl PartialEq for PeerReply {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PeerReply {}
impl PeerReply {
    /// Exact private call source, absent for ordinary node peers.
    pub fn phone_source(&self) -> Option<&super::PhoneSource> {
        self.0.phone.as_ref()
    }
    /// Mailbox admission is separate from provider authentication and admin unlock.
    pub fn phone_authenticated(&self) -> bool {
        !self.ended() && self.0.phone_authenticated.load(Ordering::Acquire)
    }
    /// Revoke queued commands and replies before their owner reclaims the connection.
    pub fn retire(&self) {
        // Retirement is off hardware/audio callbacks. Concurrent retiring owners
        // help publish the bounded notification walk rather than waiting for one
        // another. Once RETIRED is visible, all existing domains were notified.
        if self
            .0
            .phase
            .compare_exchange(LIVE, RETIRING, Ordering::AcqRel, Ordering::Acquire)
            == Err(RETIRED)
        {
            return;
        }
        let mut link = self.0.domains.get();
        while let Some(current) = link {
            current.domain.invalidate();
            link = current.next.get();
        }
        self.0.phase.store(RETIRED, Ordering::Release);
    }
    /// Register a configured node off audio before publishing its prepared mixer.
    /// The same node domain is reused across refreshes. Each connection retains at
    /// most one entry per configured node, not one per attachment or generation.
    /// Reload retires private calls before node ownership can be replaced.
    pub fn bind_retirement_domain(&self, domain: &RetirementDomain) {
        let mut slot = &self.0.domains;
        loop {
            let mut inserted = false;
            let link = slot.get_or_init(|| {
                inserted = true;
                Box::new(RetirementLink {
                    domain: domain.clone(),
                    next: OnceLock::new(),
                })
            });
            if Arc::ptr_eq(&link.domain.0, &domain.0) {
                // This RMW joins retirement's release sequence: if it still
                // observes LIVE, a later retirement acquires this published link.
                // Otherwise registration publishes its own notification.
                if inserted && self.0.phase.fetch_or(0, Ordering::AcqRel) != LIVE {
                    domain.invalidate();
                }
                return;
            }
            slot = &link.next;
        }
    }
    /// Stable identity while the prepared response retains this connection.
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    /// A response waits for transport keying and buffered receive activity to clear.
    pub fn active(&self) -> bool {
        self.0.radio_keyed.load(Ordering::Acquire) || self.0.edge.load(Ordering::Acquire) & 1 != 0
    }
    /// Ended connections cannot receive a delayed response.
    pub fn ended(&self) -> bool {
        self.0.phase.load(Ordering::Acquire) != LIVE
    }
    pub(crate) fn set_telemetry_window(&self, until_ms: u64) {
        self.0.telemetry_until_ms.store(until_ms, Ordering::Release);
    }
    pub(crate) fn telemetry_window(&self) -> u64 {
        self.0.telemetry_until_ms.load(Ordering::Acquire)
    }
}

/// PCM publication, terminal EOF, activity edges and advisory selected source.
/// Audio is the sole activity writer; the channel owner is the sole source writer.
pub struct PeerSignals {
    pcm: AtomicU64,
    dtmf_muted: AtomicBool,
    reply: PeerReply,
    source_version: AtomicU64,
    source_edge: AtomicU64,
    source: [AtomicU8; 64],
}
impl Default for PeerSignals {
    fn default() -> Self {
        Self::new()
    }
}
impl PeerSignals {
    /// Prepare empty state before either endpoint is exposed.
    pub fn new() -> Self {
        Self::with_phone(None)
    }
    /// Construct private control state before any radio attachment exists.
    pub fn for_phone(source: super::PhoneSource) -> Self {
        Self::with_phone(Some(source))
    }
    fn with_phone(phone: Option<super::PhoneSource>) -> Self {
        Self {
            pcm: AtomicU64::new(0),
            dtmf_muted: AtomicBool::new(false),
            reply: PeerReply(Arc::new(ReplyState {
                phone,
                phone_authenticated: AtomicBool::new(false),
                phase: AtomicU8::new(LIVE),
                domains: OnceLock::new(),
                radio_keyed: AtomicBool::new(false),
                edge: AtomicU64::new(0),
                telemetry_allowed: AtomicBool::new(true),
                telemetry_until_ms: AtomicU64::new(0),
            })),
            source_version: AtomicU64::new(0),
            source_edge: AtomicU64::new(0),
            source: std::array::from_fn(|_| AtomicU8::new(0)),
        }
    }
    /// Publish only a mailbox PIN result already verified by serialized control.
    /// This never grants node-administration unlock and is not a PIN verifier.
    pub fn authenticate_phone(&self) -> bool {
        if self.reply.phone_source().is_none() || self.ended() {
            return false;
        }
        self.reply
            .0
            .phone_authenticated
            .store(true, Ordering::Release);
        true
    }
    /// Publish only after real decoded samples become visible in the shared ring.
    pub fn publish_pcm(&self) {
        self.pcm.fetch_add(1, Ordering::Release);
    }
    /// Observe the last completed PCM publication.
    pub fn pcm_epoch(&self) -> u64 {
        self.pcm.load(Ordering::Acquire)
    }
    /// Immediately suppress delayed source output; retained ring tags cover its tail.
    pub fn set_dtmf_muted(&self, muted: bool) {
        self.dtmf_muted.store(muted, Ordering::Release);
    }
    /// Current source detector/command state, without waiting for its serial owner.
    pub fn dtmf_muted(&self) -> bool {
        self.dtmf_muted.load(Ordering::Acquire)
    }
    /// Control publishes protocol telemetry eligibility independently of voice.
    pub fn set_telemetry_allowed(&self, allowed: bool) {
        self.reply
            .0
            .telemetry_allowed
            .store(allowed, Ordering::Release);
    }
    /// Audio reads only this atomic gate, never a command timer or account lock.
    pub fn telemetry_allowed(&self) -> bool {
        self.reply.0.telemetry_allowed.load(Ordering::Acquire)
    }
    /// Mark terminal transport failure before detachment or reclaim.
    pub fn end(&self) {
        self.reply.retire();
    }
    /// Terminal failure wins over any buffered activity.
    pub fn ended(&self) -> bool {
        self.reply.ended()
    }
    /// Publish a transport radio-key edge independently of decoded PCM arrival.
    pub fn set_radio_keyed(&self, keyed: bool) {
        self.reply.0.radio_keyed.store(keyed, Ordering::Release);
    }
    /// Read the latest transport radio-key state without waiting for the peer owner.
    pub fn radio_keyed(&self) -> bool {
        self.reply.0.radio_keyed.load(Ordering::Acquire)
    }
    /// Audio-owner activity edge; an odd serial means active. No allocation or wait.
    pub fn set_active(&self, active: bool) -> u64 {
        let edge = self.reply.0.edge.load(Ordering::Relaxed);
        if (edge & 1 != 0) == active {
            return edge;
        }
        let next = edge.wrapping_add(1);
        self.reply.0.edge.store(next, Ordering::Release);
        next
    }
    /// Channel-owner snapshot used to cancel queries at unkey or a newer burst.
    pub fn activity_edge(&self) -> u64 {
        self.reply.0.edge.load(Ordering::Acquire)
    }
    /// Clone a connection token on the control plane, never on an audio callback.
    pub fn reply_target(&self) -> PeerReply {
        self.reply.clone()
    }
    /// Borrow the original media fence without reference-count work on audio.
    pub(crate) fn reply(&self) -> &PeerReply {
        &self.reply
    }
    /// Match addressed PCM without reference-count work on the audio callback.
    pub fn reply_identity(&self) -> usize {
        self.reply.identity()
    }
    /// Publish a source only for the still-current active receive burst.
    pub fn select_source(&self, edge: u64, source: &str) -> bool {
        if edge & 1 == 0 || self.activity_edge() != edge || !super::identity(source) {
            return false;
        }
        self.source_version.fetch_add(1, Ordering::SeqCst);
        for (index, slot) in self.source.iter().enumerate() {
            slot.store(
                source.as_bytes().get(index).copied().unwrap_or(0),
                Ordering::SeqCst,
            );
        }
        self.source_edge.store(edge, Ordering::SeqCst);
        self.source_version.fetch_add(1, Ordering::SeqCst);
        self.activity_edge() == edge
    }
    /// Read once without retrying: concurrent publication safely falls back to direct identity.
    /// Passing the completed active edge preserves the advisory source at unkey.
    pub fn selected_source<'a>(&self, edge: u64, storage: &'a mut [u8; 64]) -> Option<&'a str> {
        let version = self.source_version.load(Ordering::SeqCst);
        if version & 1 != 0 || self.source_edge.load(Ordering::SeqCst) != edge {
            return None;
        }
        for (output, input) in storage.iter_mut().zip(&self.source) {
            *output = input.load(Ordering::SeqCst);
        }
        if self.source_version.load(Ordering::SeqCst) != version {
            return None;
        }
        let len = storage.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&storage[..len]).ok()
    }
}

#[cfg(test)]
#[path = "signals_domain_tests.rs"]
mod domain_tests;
