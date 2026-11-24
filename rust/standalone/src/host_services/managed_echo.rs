//! Registry and bounded phase dispatch for the existing direct EchoLink account owners.
use super::*;
use rptadv_peer_support::{control::Client, echolink::{JitterTiming, host::{DirectEntry, Peer, Provider}}};
use std::{collections::BTreeMap, num::NonZeroU64, path::Path, sync::atomic::{AtomicBool, Ordering}, thread::JoinHandle};
mod transition;
use transition::Journal;

type Station = [u8; 16];
type RowMap = BTreeMap<Station, Row>;
type Published = Arc<Mutex<RowMap>>;
type EntryPoint = unsafe extern "C" fn(*const abi::rptadv_incoming_peer_v1, *mut c_void) -> i32;

#[derive(Clone)]
struct Row {
    station: Station, account: [u8; 16], version: [u8; 16], label: u64, wire: u32,
    native: DirectEntry, generation: u64,
}
impl Row {
    fn matches(&self, peer: &Peer) -> bool {
        peer.belongs_to(self.native.account()) && self.generation != 0 &&
            self.generation == peer.identity().account_generation && peer.identity().qso_generation != 0
    }
    fn observe(&self) -> Result<echo::AccountObservation, EchoError> {
        let r = self.native.account().observe()?;
        if !matches!(r.state, 1..=3) || r.transport != 0 || r.node_number != self.wire || r.generation == 0 ||
            (self.generation != 0 && r.generation != self.generation) { return Err(EchoError::Contract); }
        Ok(r)
    }
    fn activate(&mut self) -> Result<(), EchoError> {
        unsafe { self.native.activate() }?;
        self.generation = self.observe()?.generation;
        Ok(())
    }
}
fn selected(rows: &Published, local: &str) -> Option<Row> {
    rows.lock().ok()?.values().find(|r| r.native.station() == local).cloned()
}
struct Slot {
    attempt: u64,
    journal: Arc<Mutex<Journal>>,
    task: Option<(u32, JoinHandle<Result<(), EchoError>>)>,
}
struct State {
    accounts: Option<Arc<Accounts>>, selected: RowMap, basis: u64, last: u64,
    slot: Option<Slot>, shutdown: Option<JoinHandle<bool>>, drained: bool,
}
pub(super) struct Frontend {
    state: Mutex<State>,
    public: Published,
    closing: Arc<AtomicBool>,
    control: *const rptadv_peer_support::control::Descriptor,
    incoming: EntryPoint,
}
// The enclosing ProductRuntime retains the immutable control table to final drain.
unsafe impl Send for Frontend {}
unsafe impl Sync for Frontend {}
impl Frontend {
    pub(super) fn new(d: &crate::providers::ProductRuntimeDescriptors) -> Result<Self, crate::runtime::RuntimeError> {
        Ok(Self { state: Mutex::new(State { accounts: None, selected: RowMap::new(), basis: 0, last: 0,
            slot: None, shutdown: None, drained: false }), public: Default::default(), closing: Default::default(),
            control: ptr::from_ref(d.control).cast(), incoming: d.product.incoming.ok_or(crate::runtime::RuntimeError::InvalidDescriptor)? })
    }
    fn owners(&self, state: &mut State) -> Result<Arc<Accounts>, EchoError> {
        if let Some(accounts) = &state.accounts { return Ok(accounts.clone()); }
        let provider = crate::runtime::with_echo_resources(|r| unsafe { Provider::load(Path::new("librptadv_echolink.so.1"), r) })?;
        let control = Arc::new(unsafe { Client::open(self.control, "rpt_advanced/echolink", 512) }.map_err(|_| EchoError::Control)?);
        let public = self.public.clone();
        let incoming = self.incoming;
        let accounts = Arc::new(Accounts::new(provider, control, move |peer| {
            if selected(&public, peer.station()).is_some_and(|row| row.matches(&peer)) && peer.revalidate().is_ok() {
                unsafe { incoming_echo(incoming, peer) }
            }
        }));
        state.accounts = Some(accounts.clone());
        Ok(accounts)
    }
    pub(super) fn current(&self, peer: &Peer) -> bool {
        !self.closing.load(Ordering::Acquire) && selected(&self.public, peer.station()).is_some_and(|r| r.matches(peer))
    }
    pub(super) fn dial(&self, local: &str, node: u32) -> Option<Arc<Peer>> {
        let row = selected(&self.public, local)?;
        let peer = row.native.account().dial(node).ok()?;
        self.current(&peer).then_some(peer)
    }
    pub(super) fn observe(&self, local: &str) -> Result<Option<echo::AccountObservation>, EchoError> {
        let Some(row) = selected(&self.public, local) else { return Ok(None) };
        let observation = row.observe()?;
        Ok(selected(&self.public, local).filter(|live| Arc::ptr_eq(live.native.account(), row.native.account())
            && live.generation == row.generation).map(|_| observation))
    }
}
fn frontend<'a>(raw: *mut c_void) -> Option<&'a Frontend> {
    if raw.is_null() { return None; }
    unsafe { (&*ptr::addr_of!((*raw.cast::<StandaloneHostContext>()).managed_echo)).as_ref() }
}
unsafe extern "C" fn begin(raw: *mut c_void, basis: u64, attempt: u64) -> i32 {
    boundary(-1, || {
        let Some(f) = frontend(raw) else { return -1 };
        let Ok(mut s) = f.state.lock() else { return -1 };
        if f.closing.load(Ordering::Acquire) || s.slot.is_some() || s.basis != basis || attempt <= s.last { return -1; }
        let journal = Journal::new(std::mem::take(&mut s.selected), f.public.clone(), f.closing.clone(), attempt);
        s.slot = Some(Slot { attempt, journal: Arc::new(Mutex::new(journal)), task: None });
        s.last = attempt;
        0
    })
}
unsafe extern "C" fn row(raw: *mut c_void, attempt: u64, input: *const abi::rptadv_managed_echo_row_v1) -> i32 {
    boundary(-1, || {
        let Some(f) = frontend(raw) else { return -1 };
        if input.is_null() || !input.is_aligned() { return -1; }
        let [size,version] = unsafe { input.cast::<[u32;2]>().read() };
        if version != 1 || (size as usize) < size_of::<abi::rptadv_managed_echo_row_v1>() { return -1; }
        let i = unsafe { &*input };
        if i.station_uuid == [0;16] || i.account_uuid == [0;16] || i.credential_version == [0;16] ||
            i.local_label == 0 || i.directory_password.length == 0 || i.directory_password.length > 4096 ||
            i.directory_password.data.is_null() || !(1..=4).contains(&i.role) { return -1; }
        let p = i.settings.cast::<echo::AccountConfig>();
        if p.is_null() || !p.is_aligned() { return -1; }
        let [sz,v] = unsafe { p.cast::<[u32;2]>().read() };
        if (sz as usize) < size_of::<echo::AccountConfig>() || v != 1 { return -1; }
        let config = unsafe { &*p };
        if config.local_node.length > 32 || config.local_node.data.is_null() ||
            unsafe { std::slice::from_raw_parts(config.local_node.data, config.local_node.length) } != i.local_label.to_string().as_bytes() { return -1; }
        let Ok(mut s) = f.state.lock() else { return -1 };
        let Some(slot) = s.slot.as_ref().filter(|slot| slot.attempt == attempt && slot.task.is_none()) else { return -1 };
        let journal = slot.journal.clone();
        let Ok(accounts) = f.owners(&mut s) else { return -1 };
        let secret = echo::ByteSpan { data: i.directory_password.data, length: i.directory_password.length };
        let timing = JitterTiming { minimum_ms:i.minimum_ms,initial_ms:i.initial_ms,maximum_ms:i.maximum_ms };
        let Ok(native) = (unsafe { accounts.prepare_direct(config, secret, i.station_uuid, timing) }) else { return -1 };
        let entry = Row { native, station:i.station_uuid, account:i.account_uuid, version:i.credential_version,
            label:i.local_label, wire:config.node_number, generation:0 };
        let Ok(mut journal) = journal.lock() else { return -1 };
        if journal.insert(i.role, entry).is_err() { return -1; }
        0
    })
}
unsafe extern "C" fn step(raw: *mut c_void, attempt:u64, phase:u32) -> i32 {
    boundary(-1, || {
        let Some(f) = frontend(raw) else { return -1 };
        let Ok(mut s) = f.state.lock() else { return -1 };
        let accounts = s.accounts.clone();
        let Some(slot) = s.slot.as_mut().filter(|x| x.attempt == attempt) else { return -1 };
        if !(1..=4).contains(&phase) || f.closing.load(Ordering::Acquire) { return -1; }
        if let Some((_, job)) = &slot.task {
            if !job.is_finished() { return 1; }
            let (done, job) = slot.task.take().unwrap();
            let Ok(result) = job.join() else { return -1 };
            if done == phase {
                if result.is_err() { return -1; }
                if phase == 4 {
                    let journal = slot.journal.clone();
                    let Ok(j) = journal.lock() else { return -1 };
                    s.selected = j.selection();
                    if j.published && !j.reversed { s.basis = attempt; }
                    s.slot = None;
                }
                return 0;
            }
        }
        let journal = slot.journal.clone();
        let Ok(job) = std::thread::Builder::new().name("rpt-echo-adoption".into()).spawn(move || {
            if phase == 4 {
                if let Some(accounts) = accounts { accounts.drain_admissions()?; }
            }
            journal.lock().map_err(|_| EchoError::Control)?.phase(phase)
        }) else { return -1 };
        slot.task = Some((phase, job));
        1
    })
}
unsafe extern "C" fn shutdown(raw: *mut c_void) -> i32 {
    boundary(-1, || {
        let Some(f) = frontend(raw) else { return -1 };
        let Ok(mut s) = f.state.lock() else { return -1 };
        f.closing.store(true, Ordering::Release);
        if s.drained { return 0; }
        if let Some(job) = &s.shutdown {
            if !job.is_finished() { return 1; }
            if s.shutdown.take().unwrap().join().unwrap_or(false) {
                s.slot = None; s.selected.clear(); s.accounts = None; s.drained = true; return 0;
            }
            return -1;
        }
        if let Some(slot) = &mut s.slot {
            if slot.task.as_ref().is_some_and(|(_, j)| !j.is_finished()) { return 1; }
            if let Some((_,job)) = slot.task.take() { if job.join().is_err() { return -1; } }
        }
        let Ok(mut public) = f.public.lock() else { return -1 };
        public.clear();
        drop(public);
        let slot = s.slot.as_ref().map(|slot| slot.journal.clone());
        let selected = s.selected.clone();
        let accounts = s.accounts.clone();
        if slot.is_none() && selected.is_empty() && accounts.is_none() { s.drained = true; return 0; }
        let Ok(job) = std::thread::Builder::new().name("rpt-echo-stop".into()).spawn(move || {
            let mut ok = true;
            if let Some(j) = slot { ok &= j.lock().is_ok_and(|j| j.stop()); }
            for row in selected.values() { ok &= row.native.stop().is_ok(); }
            if let Some(accounts) = accounts { ok &= accounts.stop().is_ok(); }
            ok
        }) else { return -1 };
        s.shutdown = Some(job);
        1
    })
}
pub(super) static OPERATIONS: abi::rptadv_managed_echo_provider_v1 = abi::rptadv_managed_echo_provider_v1 {
    struct_size: size_of::<abi::rptadv_managed_echo_provider_v1>() as u32, abi_version:1,
    begin:Some(begin),row:Some(row),step:Some(step),shutdown:Some(shutdown),
};
