//! Managed ASL selections on Asterisk-owned IAX2 channels.
use super::*;
use crate::registration::{ManagedRegistrar, RegistrarIdentity, RegistrarState};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU16, NonZeroU64},
    sync::{Condvar, Mutex},
    thread::JoinHandle,
    time::Duration,
};
const MEDIA_KIND: u32 = 2;
// PBX callbacks drive inbound calls. Pending phase changes and shutdown notify the owner.
const POLL_DELAY: Duration = Duration::from_secs(3600);
type NetworkAccess = fn(NonZeroU16) -> bool;
#[derive(Clone, Copy, PartialEq, Eq)]
struct Authority {
    station: Station,
    account: Station,
    generation: NonZeroU64,
    wire: NonZeroU64,
    port: NonZeroU16,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Delays;
impl Delays {
    fn new(min: u32, initial: u32, max: u32) -> Option<Self> {
        (min <= initial && initial <= max).then_some(Self)
    }
}
#[derive(Default)]
struct Listeners {
    stage: Option<(NonZeroU64, Option<NonZeroU16>)>,
}
impl Listeners {
    fn prepare(
        &mut self,
        _basis: Option<NonZeroU64>,
        ticket: NonZeroU64,
        rows: &[Authority],
    ) -> Result<(), ()> {
        let port = rows.first().map(|r| r.port);
        if self.stage.is_some() || rows.iter().any(|r| Some(r.port) != port) {
            return Err(());
        }
        self.stage = Some((ticket, port));
        Ok(())
    }
    fn publish(&mut self, ticket: NonZeroU64) -> Result<(), ()> {
        self.stage
            .filter(|(t, _)| *t == ticket)
            .map(|_| ())
            .ok_or(())
    }
    fn finish(&mut self, ticket: NonZeroU64) -> Result<(), ()> {
        self.publish(ticket)?;
        self.stage = None;
        Ok(())
    }
    fn reverse(&mut self, ticket: NonZeroU64) -> Result<(), ()> {
        self.publish(ticket)?;
        self.stage = None;
        Ok(())
    }
}
fn apply_transport(listeners: &mut Listeners, shared: &Shared, job: &Selection) -> Result<(), ()> {
    let (_, port) = listeners
        .stage
        .filter(|(t, _)| *t == job.attempt)
        .ok_or(())?;
    if port.is_some_and(|p| !(shared.network)(p)) {
        return Err(());
    }
    Ok(())
}
fn poll_transport(_: &mut Listeners, _: &Arc<Shared>, _: abi::rptadv_asl_admission_v1) {}
#[cfg(test)]
fn before_observing_worker() {}
fn frontend<'a>(raw: *mut c_void) -> Option<&'a Frontend> {
    unsafe { raw.cast::<HostContext>().as_ref() }?.asl.as_ref()
}
pub(super) static OPERATIONS: abi::rptadv_asl_provider_v2 = abi::rptadv_asl_provider_v2 {
    struct_size: size_of::<abi::rptadv_asl_provider_v2>() as u32,
    abi_version: 2,
    media_kind: MEDIA_KIND,
    reserved: 0,
    prepare: Some(prepare),
    step: Some(step),
    observe: Some(observe),
    originate: Some(originate),
};
include!("../../../peer-support/frontend/managed_asl.rs");

#[derive(Default)]
struct PeerRegistry {
    next: u64,
    calls: BTreeMap<NonZeroU64, Authority>,
    fenced: BTreeSet<Station>,
    admissions: BTreeMap<Station, usize>,
}
impl PeerRegistry {
    fn fence(&mut self, ids: &BTreeSet<Station>) {
        self.fenced.clone_from(ids);
    }
    fn released(&mut self) {
        self.fenced.clear();
    }
    fn adjust(&mut self, _: &Rows, _: bool) -> Result<bool, ()> {
        Ok(self
            .fenced
            .iter()
            .all(|station| !self.admissions.contains_key(station)))
    }
    fn drained(&self, rows: &Rows) -> bool {
        self.fenced
            .iter()
            .all(|station| !self.admissions.contains_key(station))
            && self.calls.values().all(|a| {
                !self.fenced.contains(&a.station)
                    || rows.get(&a.station).is_some_and(|r| r.authority == *a)
            })
    }
}
fn idle_admissions(peers: &PeerRegistry) -> bool {
    peers.admissions.is_empty()
}
fn selected(state: &State, a: Authority) -> bool {
    state.ingress
        && !state.peers.fenced.contains(&a.station)
        && state
            .published
            .get(&a.station)
            .is_some_and(|r| r.authority == a)
}
/// Pins the frontend while call setup executes outside its registry lock.
pub(crate) struct Handoff {
    shared: Arc<Shared>,
    a: Authority,
    callbacks: abi::rptadv_asl_admission_v1,
}
impl Handoff {
    fn capture(shared: &Arc<Shared>, a: Authority) -> Option<Self> {
        let mut state = shared.lock();
        if !selected(&state, a) {
            return None;
        }
        let callbacks = state.callbacks?;
        let count = state.peers.admissions.entry(a.station).or_default();
        *count = count.checked_add(1)?;
        Some(Self {
            shared: Arc::clone(shared),
            a,
            callbacks,
        })
    }
    fn member(&self) -> Option<Member> {
        let mut state = self.shared.lock();
        if !selected(&state, self.a) {
            return None;
        }
        let id = serial(&mut state.peers.next)?;
        state.peers.calls.insert(id, self.a);
        Some(Member {
            shared: self.shared.clone(),
            id,
        })
    }
    pub(crate) fn answer(
        &self,
        source: &crate::application::IncomingIdentity,
        channel: *mut c_void,
    ) -> Result<(), crate::Error> {
        fn bytes(s: &str) -> abi::rptadv_peer_bytes_v1 {
            abi::rptadv_peer_bytes_v1 {
                data: s.as_ptr(),
                length: s.len(),
            }
        }
        let mut proof = ptr::null_mut();
        let accepted = unsafe {
            self.callbacks.authorize.unwrap()(
                &identity(self.a),
                bytes(&source.remote),
                bytes(&source.address),
                &mut proof,
            )
        };
        if accepted != 0 || proof.is_null() {
            return Err(crate::Error::Admission);
        }
        struct Completion(abi::rptadv_asl_admission_v1, *mut c_void);
        impl Drop for Completion {
            fn drop(&mut self) {
                if !self.1.is_null() {
                    unsafe { self.0.complete.unwrap()(self.1, ptr::null_mut()) };
                }
            }
        }
        let mut completion = Completion(self.callbacks, proof);
        let member = self.member().ok_or(crate::Error::Admission)?;
        let io = unsafe { source.handoff(channel, 4096) }?;
        if !selected(&self.shared.lock(), self.a) {
            return Err(crate::Error::Admission);
        }
        let peer = wrap(io, &source.local, &source.remote, member);
        let result =
            unsafe { self.callbacks.complete.unwrap()(std::mem::take(&mut completion.1), peer) };
        if result > 0 {
            unsafe { destroy_raw(peer) }
        }
        (result == 0).then_some(()).ok_or(crate::Error::Admission)
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        let count = state
            .peers
            .admissions
            .get_mut(&self.a.station)
            .expect("captured handoff");
        *count -= 1;
        if *count == 0 {
            state.peers.admissions.remove(&self.a.station);
        }
        drop(state);
        self.shared.wake.notify_one();
    }
}
pub(super) struct Member {
    shared: Arc<Shared>,
    id: NonZeroU64,
}
impl Drop for Member {
    fn drop(&mut self) {
        self.shared.lock().peers.calls.remove(&self.id);
        self.shared.wake.notify_one();
    }
}
impl Frontend {
    pub(super) fn incoming(&self, local: &str) -> Option<Handoff> {
        let wire = local.parse::<NonZeroU64>().ok()?;
        if wire.to_string() != local {
            return None;
        }
        let a = self
            .shared
            .lock()
            .published
            .values()
            .find(|r| r.authority.wire == wire)?
            .authority;
        Handoff::capture(&self.shared, a)
    }
}
fn wrap(io: PeerIo, local: &str, remote: &str, member: Member) -> *mut c_void {
    Box::into_raw(Box::new(PeerHandle::Iax(IaxHandle {
        io,
        local: local.into(),
        remote: remote.into(),
        _member: Some(member),
    })))
    .cast()
}
unsafe extern "C" fn originate(
    host: *mut c_void,
    id: *const abi::rptadv_asl_identity_v1,
    remote: abi::rptadv_peer_bytes_v1,
    endpoint: abi::rptadv_peer_bytes_v1,
    maximum: usize,
    current: abi::rptadv_current_v1,
    context: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        if out.is_null() || !out.is_aligned() {
            return -1;
        }
        unsafe { out.write(ptr::null_mut()) };
        let (Some(host), Some(id), Some(remote), Some(endpoint), Some(current)) = (
            frontend(host),
            unsafe { id.as_ref() }.and_then(authority),
            unsafe { borrowed(remote, 63) },
            unsafe { borrowed(endpoint, 128) }.and_then(|s| s.parse::<std::net::SocketAddr>().ok()),
            current,
        ) else {
            return -1;
        };
        if maximum == 0
            || endpoint.port() == 0
            || remote.is_empty()
            || !remote.bytes().all(|b| b.is_ascii_digit())
            || unsafe { current(context) } == 0
        {
            return -1;
        }
        let Some(handoff) = Handoff::capture(&host.shared, id) else {
            return -1;
        };
        let destination =
            CString::new(format!("radio@{endpoint}/{remote}")).expect("validated numeric endpoint");
        let local = CString::new(id.wire.to_string()).unwrap();
        let Ok(io) = PeerIo::dial(&destination, &local, maximum, || unsafe {
            current(context) != 0
        }) else {
            return -1;
        };
        if unsafe { current(context) } == 0 {
            return -1;
        }
        let Some(member) = handoff.member() else {
            return -1;
        };
        unsafe { out.write(wrap(io, local.to_str().unwrap(), remote, member)) };
        0
    })
}

/// Match a PBX process-owned UDP socket and check an IAX2 PONG on that exact address.
/// The driver retains its fd and its global bind configuration.
pub(super) fn bound_endpoint(port: NonZeroU16) -> bool {
    use std::net::{Ipv4Addr, UdpSocket};
    fn sockets() -> BTreeSet<String> {
        std::fs::read_dir("/proc/self/fd")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| std::fs::read_link(e.path()).ok())
            .filter_map(|p| {
                p.to_str()?
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')
                    .map(str::to_owned)
            })
            .collect()
    }
    let owned = sockets();
    let Ok(table) = std::fs::read_to_string("/proc/self/net/udp") else {
        return false;
    };
    for line in table.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let Some((ip, p)) = fields.nth(1).and_then(|s| s.split_once(':')) else {
            continue;
        };
        let Some(inode) = fields.nth(7) else { continue };
        if !owned.contains(inode) || u16::from_str_radix(p, 16).ok() != Some(port.get()) {
            continue;
        }
        let Ok(raw) = u32::from_str_radix(ip, 16) else {
            continue;
        };
        let address = Ipv4Addr::from(raw.to_ne_bytes());
        let destination = if address.is_unspecified() {
            Ipv4Addr::LOCALHOST
        } else {
            address
        };
        let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
            continue;
        };
        if socket.connect((destination, port.get())).is_err()
            || socket
                .set_read_timeout(Some(Duration::from_millis(300)))
                .is_err()
        {
            continue;
        }
        let Ok(local) = socket.local_addr() else {
            continue;
        };
        let number = 1 + (local.port() & 0x3fff);
        let mut poke = [0u8; 12];
        poke[..2].copy_from_slice(&(0x8000 | number).to_be_bytes());
        poke[4..8].copy_from_slice(&(local.port() as u32).to_be_bytes());
        poke[10] = 6;
        poke[11] = 30;
        if !matches!(socket.send(&poke), Ok(12)) {
            continue;
        }
        let mut reply = [0u8; 512];
        if socket.recv(&mut reply).is_ok_and(|n| n >= 12)
            && reply[0] & 0x80 != 0
            && (u16::from_be_bytes([reply[2], reply[3]]) & 0x7fff) == number
            && reply[4..8] == poke[4..8]
            && reply[10..12] == [6, 3]
        {
            // chan_iax2 answers POKE without allocating a call number.
            return sockets().contains(inode);
        }
    }
    false
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn configured_playout_is_rejected_before_native_adoption_and_inherited_defaults_remain_dormant()
    {
        unsafe extern "C" fn authorize(
            _: *const abi::rptadv_asl_identity_v1,
            _: abi::rptadv_peer_bytes_v1,
            _: abi::rptadv_peer_bytes_v1,
            _: *mut *mut c_void,
        ) -> i32 {
            -1
        }
        unsafe extern "C" fn complete(_: *mut c_void, _: *mut c_void) -> i32 {
            1
        }
        let table = abi::rptadv_asl_admission_v1 {
            struct_size: size_of::<abi::rptadv_asl_admission_v1>() as u32,
            abi_version: 1,
            authorize: Some(authorize),
            complete: Some(complete),
        };
        let mut state = State::default();
        let mut row: abi::rptadv_asl_account_v2 = unsafe { std::mem::zeroed() };
        row.struct_size = size_of::<abi::rptadv_asl_account_v2>() as u32;
        row.abi_version = 2;
        row.id.station_uuid = [1; 16];
        row.id.account_uuid = [2; 16];
        row.id.generation = 1;
        row.id.wire = 1000;
        row.id.udp_port = 4569;
        row.jitter_min_ms = 20;
        row.jitter_initial_ms = 40;
        row.jitter_max_ms = 100;
        row.jitter_authored = 1;
        assert!(unsafe { state.selection(0, 1, &[row], table) }.is_none());
        row.jitter_authored = 0;
        assert!(unsafe { state.selection(0, 1, &[row], table) }.is_some());
        assert!(state.published.is_empty());
    }
}
