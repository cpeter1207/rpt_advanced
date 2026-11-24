//! Peer registration is synchronized separately from the native media-owner callback.
use super::*;
use crate::iax::playout::{PlayoutControl, Receipt};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Attachment { Prepared, Attaching, Attached, Retiring }

struct Entry {
    authority: Authority,
    delays: Delays,
    phase: Attachment,
    control: PlayoutControl,
    queued: Option<(Delays, Receipt)>,
}

#[derive(Default)]
pub(super) struct PeerRegistry {
    next: u64,
    entries: BTreeMap<NonZeroU64, Entry>,
    fenced: BTreeSet<Station>,
}

impl PeerRegistry {
    pub(super) fn fence(&mut self, affected: &BTreeSet<Station>) { self.fenced.clone_from(affected); }
    pub(super) fn released(&mut self) { self.fenced.clear(); }
    pub(super) fn drained(&self, selected: &Rows) -> bool {
        self.entries.values().all(|entry| {
            !self.fenced.contains(&entry.authority.station)
                || (entry.phase != Attachment::Retiring
                    && selected.get(&entry.authority.station).is_some_and(|row| row.authority == entry.authority))
        })
    }

    pub(super) fn adjust(&mut self, selected: &Rows, reversing: bool) -> Result<bool, ()> {
        let mut ready = true;
        for entry in self.entries.values_mut().filter(|e| self.fenced.contains(&e.authority.station)) {
            if entry.phase == Attachment::Retiring { continue; }
            if entry.phase == Attachment::Attaching { ready = false; continue; }
            let target = selected.get(&entry.authority.station).filter(|r| r.authority == entry.authority);
            if entry.phase == Attachment::Prepared {
                if target.is_none_or(|r| r.delays != entry.delays) { entry.phase = Attachment::Retiring; }
                continue;
            }
            if let Some((tuple, receipt)) = &entry.queued {
                match receipt.applied() {
                    None => { ready = false; continue; }
                    Some(true) => entry.delays = *tuple,
                    Some(false) if !reversing || target.is_some_and(|r| r.delays == *tuple) => {
                        entry.queued = None;
                        return Err(());
                    }
                    Some(false) => {}
                }
                entry.queued = None;
            }
            if let Some(row) = target.filter(|r| r.delays != entry.delays) {
                entry.queued = Some((row.delays, entry.control.request(row.delays).ok_or(())?));
                ready = false;
            }
        }
        Ok(ready)
    }
}

/// The unique peer wrapper owns final deregistration. Attach/stop may take a short borrow.
pub(in super::super) struct Member {
    shared: Arc<Shared>,
    id: NonZeroU64,
}

impl Member {
    pub(in super::super) fn attaching(&self) -> Option<Attach> {
        let mut state = self.shared.lock();
        let entry = state.peers.entries.get(&self.id)?;
        let row = selected(&state, entry.authority)?;
        if row.delays != entry.delays || entry.phase != Attachment::Prepared { return None; }
        state.peers.entries.get_mut(&self.id)?.phase = Attachment::Attaching;
        Some(Attach { shared: self.shared.clone(), id: self.id })
    }
}

impl Drop for Member {
    fn drop(&mut self) { self.shared.lock().peers.entries.remove(&self.id); }
}

pub(in super::super) struct Attach { shared: Arc<Shared>, id: NonZeroU64 }
impl Attach {
    pub(in super::super) fn completed(&self, success: bool) {
        if let Some(entry) = self.shared.lock().peers.entries.get_mut(&self.id) {
            entry.phase = if success { Attachment::Attached } else { Attachment::Retiring };
        }
    }
    pub(in super::super) fn retiring(&self) { self.completed(false); }
}

fn selected(state: &State, a: Authority) -> Option<&Row> {
    if !state.ingress || state.peers.fenced.contains(&a.station) { return None; }
    state.published.get(&a.station).filter(|r| r.authority == a)
}

fn wrap(shared: &Arc<Shared>, a: Authority, remote: &str, mut peer: IaxPeer) -> Option<Box<PeerHandle>> {
    let delays = selected(&shared.lock(), a)?.delays;
    let control = shared.network.open().ok()?.prepare_playout(&mut peer, delays).ok()?;
    let mut state = shared.lock();
    if selected(&state, a)?.delays != delays { return None; }
    let id = serial(&mut state.peers.next)?;
    state.peers.entries.insert(id, Entry {
        authority: a, delays, phase: Attachment::Prepared, control, queued: None,
    });
    drop(state);
    let mut handle = IaxHandle::new(peer, &a.wire.to_string(), remote);
    handle.managed = Some(Member { shared: shared.clone(), id });
    Some(Box::new(PeerHandle::Iax(handle)))
}

unsafe fn text<'a>(span: abi::rptadv_peer_bytes_v1) -> Option<&'a str> { unsafe { borrowed(span, 128) } }
fn span(s: &str) -> abi::rptadv_peer_bytes_v1 { abi::rptadv_peer_bytes_v1 { data: s.as_ptr(), length: s.len() } }

pub(super) unsafe extern "C" fn originate(host: *mut c_void, id: *const abi::rptadv_asl_identity_v1,
    remote: abi::rptadv_peer_bytes_v1, endpoint: abi::rptadv_peer_bytes_v1, maximum: usize,
    current: abi::rptadv_current_v1, context: *mut c_void, out: *mut *mut c_void) -> i32 {
    boundary(-1, || {
        let Some(out) = (unsafe { out.as_mut() }) else { return -1 };
        *out = ptr::null_mut();
        let (Some(host), Some(id), Some(remote), Some(endpoint), Some(current)) =
            (frontend(host), unsafe { id.as_ref() }.and_then(authority), unsafe { text(remote) },
             unsafe { text(endpoint) }.and_then(|s| s.parse::<SocketAddr>().ok()), current) else { return -1 };
        if maximum == 0 || endpoint.port() == 0 || remote.is_empty() || remote.len() > 63
            || !remote.bytes().all(|b| b.is_ascii_digit()) || selected(&host.shared.lock(), id).is_none()
            || unsafe { current(context) } == 0 { return -1; }
        let Ok(ingress) = host.shared.network.open() else { return -1 };
        let Ok(peer) = ingress.client.clone().dial(endpoint, &id.wire.to_string(), remote, "", 4000) else { return -1 };
        if unsafe { current(context) } == 0 { return -1; }
        let Some(wrapper) = wrap(&host.shared, id, remote, peer) else { return -1 };
        *out = Box::into_raw(wrapper).cast();
        0
    })
}

struct Proof {
    table: abi::rptadv_asl_admission_v1,
    native: *mut c_void,
    local: Authority,
    remote: String,
    source: IpAddr,
}

impl Drop for Proof {
    fn drop(&mut self) {
        if !self.native.is_null() { unsafe { self.table.complete.unwrap()(self.native, ptr::null_mut()); } }
    }
}

pub(super) struct Admission<'a> {
    shared: &'a Arc<Shared>,
    table: abi::rptadv_asl_admission_v1,
    proof: Option<Proof>,
}

impl<'a> Admission<'a> {
    pub(super) fn new(shared: &'a Arc<Shared>, table: abi::rptadv_asl_admission_v1) -> Self {
        Self { shared, table, proof: None }
    }
}

struct PollGuard<'a>(&'a Shared);
impl Drop for PollGuard<'_> {
    fn drop(&mut self) { self.0.lock().polling = false; }
}

impl Incoming for Admission<'_> {
    fn individual(&mut self, poll: impl FnOnce(&mut Self) -> Result<bool, IaxError>) -> Result<bool, IaxError> {
        {
            let mut state = self.shared.lock();
            if !state.ingress { return Ok(false); }
            state.polling = true;
        }
        let _guard = PollGuard(self.shared);
        let mut scoped = Self::new(self.shared, self.table);
        poll(&mut scoped)
    }

    fn authorize(&mut self, local: Authority, remote: &str, source: &str) -> bool {
        self.proof.take();
        let Ok(ip) = source.parse::<IpAddr>() else { return false };
        if selected(&self.shared.lock(), local).is_none() { return false; }
        let mut native = ptr::null_mut();
        let status = unsafe { self.table.authorize.unwrap()(&identity(local), span(remote), span(source), &mut native) };
        if native.is_null() { return false; }
        let proof = Proof { table: self.table, native, local, remote: remote.into(), source: ip };
        if status != 0 { return false; }
        self.proof = Some(proof);
        true
    }

    fn accept(&mut self, local: Authority, remote: &str, source: &str, peer: IaxPeer) {
        let Some(mut proof) = self.proof.take() else { return };
        if proof.local != local || proof.remote != remote || source.parse::<IpAddr>() != Ok(proof.source) { return; }
        let Some(wrapper) = wrap(self.shared, local, remote, peer) else { return };
        let raw = Box::into_raw(wrapper);
        let token = std::mem::replace(&mut proof.native, ptr::null_mut());
        if unsafe { self.table.complete.unwrap()(token, raw.cast()) } > 0 {
            unsafe { drop(Box::from_raw(raw)); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withdrawing_one_peer_under_a_retained_authority_waits_for_wrapper_destruction() {
        unsafe extern "C" fn setter(_: *mut c_void, _: u32, _: u32, _: u32) -> i32 { 0 }
        let host = HostServicesOwner::new(Vec::new(), None, Default::default()).unwrap();
        let shared = Arc::new(Shared { state: Default::default(), wake: Condvar::new(), network: host.network_access().unwrap() });
        let authority = Authority { station: [1; 16], account: [2; 16], wire: NonZeroU64::new(716701).unwrap(),
            port: NonZeroU16::new(4569).unwrap(), generation: NonZeroU64::MIN };
        let delays = Delays::new(20, 40, 100).unwrap();
        let selected = Rows::from([(authority.station, Row { authority, delays, registration: None, worker: None })]);
        let (media_owner, control) = crate::iax::playout::Owner::new(setter);
        let id = NonZeroU64::MIN;
        let member = Member { id, shared: shared.clone() };
        {
            let mut state = shared.lock();
            state.published = selected.clone();
            state.ingress = true;
            state.peers.entries.insert(id, Entry { authority, delays, phase: Attachment::Prepared, control, queued: None });
        }
        let registration = member.attaching().unwrap();
        registration.completed(true);
        shared.lock().peers.fence(&BTreeSet::from([authority.station]));
        assert!(shared.lock().peers.drained(&selected), "a permitted retained peer may keep carrying audio");
        registration.retiring();
        assert!(!shared.lock().peers.drained(&selected),
            "the same authority is selected but the ACL-retired wrapper is still owned");
        drop(media_owner);
        drop(member);
        assert!(shared.lock().peers.drained(&selected));
    }
}
