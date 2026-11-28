//! Bounded lock-free snapshots between one audio consumer and one channel owner.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

/// Station-local PCM cut shared with prepared peer rings and retained egress.
#[derive(Debug, Default)]
pub struct MediaIsolation {
    // Only call owners mutate claims, on the control plane. Audio reads epoch once.
    claims: std::sync::Mutex<usize>,
    epoch: AtomicU64,
}

impl MediaIsolation {
    /// Capture the station media epoch. An odd tag suppresses public programme.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    // The claims mutex serializes writers; readers never take that mutex.
    fn publish(&self, active: bool) {
        let old = self.epoch.load(Ordering::Relaxed);
        self.epoch.store(
            (old.wrapping_add(2) & !1) | u64::from(active),
            Ordering::Release,
        );
    }

    /// Own one suppression claim independently of other pending or live calls.
    /// Acquire and release claims on the control plane, never in an audio callback.
    pub fn hold(self: &Arc<Self>) -> Option<MediaIsolationClaim> {
        let mut claims = self.claims.lock().unwrap();
        *claims = claims.checked_add(1)?;
        self.publish(true);
        Some(MediaIsolationClaim(Arc::clone(self)))
    }
}

/// Release only the suppression acquired by this one call, on its control owner.
pub struct MediaIsolationClaim(Arc<MediaIsolation>);

impl Drop for MediaIsolationClaim {
    fn drop(&mut self) {
        let mut claims = self.0.claims.lock().unwrap();
        *claims -= 1;
        self.0.publish(*claims != 0);
    }
}

// A reverse caller's first unmute belongs to that caller. This odd epoch
// also invalidates any PCM or regenerated DTMF captured before consent.
const WAITING_FOR_CALLER_UNMUTE: u64 = u64::MAX;

#[derive(Debug)]
struct WebRtcSource {
    session: uuid::Uuid,
    receive_epoch: AtomicU64,
    deadline_ns: AtomicU64,
    closed: Arc<AtomicBool>,
}

#[derive(Debug)]
struct ReplyState {
    phone: Option<super::PhoneSource>,
    webrtc: OnceLock<Arc<WebRtcSource>>,
    phone_authenticated: AtomicBool,
    phone_media_only: bool,
    peer_callsign: OnceLock<Arc<str>>,
    rx_muted: Arc<AtomicU64>,
    media_isolation: OnceLock<Arc<MediaIsolation>>,
    ended: AtomicBool,
    radio_keyed: AtomicBool,
    radio_key_epoch: AtomicU64,
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
/// Non-secret revocable provenance for received DTMF. A subsequent unmute cannot
/// restore already queued signaling from a previous mute epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegenerationOrigin {
    source: PeerReply,
    epoch: u64,
}
impl RegenerationOrigin {
    /// Recheck immediately before any audible sample or signaling key demand.
    pub fn current(&self) -> bool {
        !self.source.ended() && self.source.0.rx_muted.load(Ordering::Acquire) == self.epoch
    }
}
impl PeerReply {
    /// Copied public callsign from the authenticated provider session.
    pub(crate) fn peer_callsign(&self) -> Option<&str> {
        self.0.peer_callsign.get().map(AsRef::as_ref)
    }

    /// Capture receipt-time identity before the outcome leaves its actual ingress.
    pub fn regeneration_origin(&self) -> Option<RegenerationOrigin> {
        let epoch = self.0.rx_muted.load(Ordering::Acquire);
        (!self.ended() && epoch & 1 == 0).then(|| RegenerationOrigin {
            source: self.clone(),
            epoch,
        })
    }
    /// Current operational mute of this connection, independent of raw receive/digit state.
    pub fn rx_muted(&self) -> bool {
        self.0.rx_muted.load(Ordering::Acquire) & 1 != 0
    }
    /// Bind the station's programme permit before publishing this peer.
    pub fn bind_media_isolation(&self, isolation: Arc<MediaIsolation>) {
        let _ = self.0.media_isolation.set(isolation);
    }
    pub(crate) fn set_rx_muted(&self, muted: bool) {
        let _ = self
            .0
            .rx_muted
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (old != WAITING_FOR_CALLER_UNMUTE && (old & 1 != 0) != muted)
                    .then(|| old.wrapping_add(1))
            });
    }
    /// Toggle the verified caller's own ingress, including its initial mute.
    pub(crate) fn toggle_caller_rx_mute(&self) {
        self.0.rx_muted.fetch_add(1, Ordering::AcqRel);
    }
    /// Exact private call source, absent for ordinary node peers.
    pub fn phone_source(&self) -> Option<&super::PhoneSource> {
        self.0.phone.as_ref()
    }
    /// WebRTC session that owns this exact media endpoint.
    pub fn webrtc_binding(&self) -> Option<uuid::Uuid> {
        self.0.webrtc.get().map(|source| source.session)
    }
    /// Close this connection and every prepared media redirection it owns.
    pub fn close_webrtc_session(&self) {
        if let Some(source) = self.0.webrtc.get() {
            source.closed.store(true, Ordering::Release);
        }
    }
    /// Mailbox admission is separate from provider authentication and admin unlock.
    pub fn phone_authenticated(&self) -> bool {
        !self.ended() && self.0.phone_authenticated.load(Ordering::Acquire)
    }
    /// Media admission follows either verified mailbox attachment or the outbound leg.
    pub fn phone_media_allowed(&self) -> bool {
        !self.ended()
            && (self.0.phone_media_only || self.0.phone_authenticated.load(Ordering::Acquire))
    }
    /// The called party has a PCM route and no command-input lane.
    pub fn outgoing_phone(&self) -> bool {
        self.0.phone_media_only
    }
    /// Revoke queued commands and replies before their owner reclaims the connection.
    pub fn retire(&self) {
        self.0.ended.store(true, Ordering::Release);
    }
    /// Stable identity while the prepared response retains this connection.
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    /// A response waits for transport keying and buffered receive activity to clear.
    pub fn active(&self) -> bool {
        self.0.edge.load(Ordering::Acquire) & 1 != 0
            || (self.0.webrtc.get().is_none() && self.0.radio_keyed.load(Ordering::Acquire))
    }
    /// Ended connections cannot receive a delayed response.
    pub fn ended(&self) -> bool {
        self.0.ended.load(Ordering::Acquire)
            || self
                .0
                .webrtc
                .get()
                .is_some_and(|source| source.closed.load(Ordering::Acquire))
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
    /// Prepare a WebRTC endpoint for an already authorized local session.
    pub fn for_webrtc(session: uuid::Uuid) -> Self {
        Self::for_revocable_webrtc(session, Arc::new(AtomicBool::new(false)))
    }
    /// Prepare an endpoint with the admitted provider's shared revocation gate.
    pub fn for_revocable_webrtc(session: uuid::Uuid, closed: Arc<AtomicBool>) -> Self {
        let signals = Self::new();
        let _ = signals.reply.0.webrtc.set(Arc::new(WebRtcSource {
            session,
            receive_epoch: AtomicU64::new(0),
            deadline_ns: AtomicU64::new(0),
            closed,
        }));
        signals
    }
    /// Construct private control state before any radio attachment exists.
    pub fn for_phone(source: super::PhoneSource) -> Self {
        Self::with_phone(Some(source))
    }
    /// A reverse caller can hear shared audio after login, while its ingress
    /// stays muted until the same caller explicitly toggles it.
    pub fn for_reverse_phone(source: super::PhoneSource) -> Self {
        Self::with_mute(
            Some(source),
            Arc::new(AtomicU64::new(WAITING_FOR_CALLER_UNMUTE)),
            false,
        )
    }
    /// An outgoing recipient can exchange PCM without any mailbox grant.
    pub fn for_outgoing_phone(source: super::PhoneSource) -> Self {
        Self::with_mute(Some(source), Arc::new(AtomicU64::new(0)), true)
    }
    /// Bind the authenticated provider callsign before publishing the connection.
    pub fn bind_peer_callsign(&self, callsign: &str) {
        if !callsign.is_empty() {
            let _ = self.reply.0.peer_callsign.set(Arc::from(callsign));
        }
    }

    /// Prepare a fresh media endpoint for the same surviving connection. Reply/edge tokens
    /// remain generation-specific while its operator mute follows the transport.
    pub fn redirected(previous: &PeerReply) -> Self {
        let next = Self::with_mute(
            previous.phone_source().cloned(),
            previous.0.rx_muted.clone(),
            previous.0.phone_media_only,
        );
        if let Some(source) = previous.0.webrtc.get() {
            let _ = next.reply.0.webrtc.set(Arc::clone(source));
        }
        if let Some(callsign) = previous.0.peer_callsign.get() {
            let _ = next.reply.0.peer_callsign.set(Arc::clone(callsign));
        }
        if let Some(isolation) = previous.0.media_isolation.get() {
            next.reply.bind_media_isolation(Arc::clone(isolation));
        }
        next
    }
    fn with_phone(phone: Option<super::PhoneSource>) -> Self {
        Self::with_mute(phone, Arc::new(AtomicU64::new(0)), false)
    }
    fn with_mute(
        phone: Option<super::PhoneSource>,
        rx_muted: Arc<AtomicU64>,
        phone_media_only: bool,
    ) -> Self {
        Self {
            pcm: AtomicU64::new(0),
            dtmf_muted: AtomicBool::new(false),
            reply: PeerReply(Arc::new(ReplyState {
                phone,
                webrtc: OnceLock::new(),
                phone_authenticated: AtomicBool::new(false),
                phone_media_only,
                peer_callsign: OnceLock::new(),
                rx_muted,
                media_isolation: OnceLock::new(),
                ended: AtomicBool::new(false),
                radio_keyed: AtomicBool::new(false),
                radio_key_epoch: AtomicU64::new(0),
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
        if self.reply.phone_source().is_none() || self.reply.0.phone_media_only || self.ended() {
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
    /// Read the current source mute without cloning the connection token on audio.
    pub fn receive_epoch(&self) -> u64 {
        let muted = self.reply.0.rx_muted.load(Ordering::Acquire);
        let isolated = self
            .reply
            .0
            .media_isolation
            .get()
            .map_or(0, |gate| gate.epoch());
        let lease = self
            .reply
            .0
            .webrtc
            .get()
            .map_or(0, |source| source.receive_epoch.load(Ordering::Acquire));
        (muted & !1).wrapping_add(isolated & !1).wrapping_add(lease) | ((muted | isolated) & 1)
    }
    /// Release the qualified source and invalidate PCM retained under its old
    /// grant, including during an acknowledged graph redirection.
    pub fn revoke_webrtc_receive(&self) {
        if let Some(source) = self.reply.0.webrtc.get() {
            source.deadline_ns.store(0, Ordering::Release);
            source.receive_epoch.fetch_add(2, Ordering::AcqRel);
            self.set_radio_keyed(false);
        }
    }
    /// Publish an accepted lease's receiver-local deadline on the native stream clock.
    pub fn set_webrtc_deadline(&self, deadline_ns: u64) {
        if let Some(source) = self.reply.0.webrtc.get() {
            source.deadline_ns.store(deadline_ns, Ordering::Release);
        }
    }
    /// Refuse a block whose native time is uncertain or crosses the lease deadline.
    pub(super) fn webrtc_deadline_allows(&self, block_end_ns: Option<u64>) -> bool {
        self.reply
            .0
            .webrtc
            .get()
            .zip(block_end_ns)
            .is_some_and(|(source, end)| end < source.deadline_ns.load(Ordering::Acquire))
    }
    /// Whether the source is operationally muted.
    pub fn rx_muted(&self) -> bool {
        self.reply.rx_muted()
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
        self.set_radio_keyed_at(keyed, self.receive_epoch());
    }
    /// Publish the key edge with the receive epoch captured before the provider handoff.
    pub fn set_radio_keyed_at(&self, keyed: bool, epoch: u64) {
        if keyed {
            self.reply.0.radio_key_epoch.store(epoch, Ordering::Relaxed);
        }
        self.reply.0.radio_keyed.store(keyed, Ordering::Release);
    }
    /// A raw carrier or a finite burst can remain active while its output key is revoked.
    pub fn output_radio_keyed(&self, epoch: u64) -> bool {
        epoch & 1 == 0
            && self.radio_keyed()
            && self.reply.0.radio_key_epoch.load(Ordering::Relaxed) == epoch
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
mod capacity_tests {
    use super::*;
    use std::{sync::Barrier, thread};

    #[test]
    fn independent_isolation_claims_have_no_packed_count_limit() {
        let isolation = Arc::new(MediaIsolation::default());
        let initial = isolation.epoch();
        let anchor = isolation.hold().unwrap();
        let held = Arc::new(Barrier::new(9));
        let release = Arc::new(Barrier::new(9));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let isolation = Arc::clone(&isolation);
                let held = Arc::clone(&held);
                let release = Arc::clone(&release);
                thread::spawn(move || {
                    let claims: Vec<_> = (0..10_000).map(|_| isolation.hold()).collect();
                    let admitted = claims.iter().filter(|claim| claim.is_some()).count();
                    held.wait();
                    release.wait();
                    drop(claims);
                    admitted
                })
            })
            .collect();
        held.wait();
        let active = isolation.epoch();
        release.wait();
        let admitted: usize = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        let retained = isolation.epoch();
        drop(anchor);
        let finished = isolation.epoch();
        eprintln!(
            "ISOLATION_CAPACITY_CLOSED requested=80001 admitted={} active={} retained={} released={} epoch_changed={}",
            admitted + 1,
            active & 1 != 0,
            retained & 1 != 0,
            finished & 1 == 0,
            initial != active && active != retained && retained != finished
        );
        assert_ne!(active & 1, 0);
        assert_ne!(retained & 1, 0);
        assert_eq!(finished & 1, 0);
        assert_ne!(initial, active);
        assert_ne!(active, finished);
        assert_eq!(
            admitted, 80_000,
            "isolation must not impose a packed total-call limit"
        );
    }
}
