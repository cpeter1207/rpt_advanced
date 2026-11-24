//! Native private transports with a deterministic station callback boundary.
//! The shared frontend owns the account, UDP socket, authenticated dialog, codec and executor.
use super::{Counters, fixture, quiesce};
use crate::abi;
use rptadv_private_phone_host::{calls::{PrivateCall, Registration}, frontend::{self, Frontend},
    selection::{NativePaths, PhoneHost}};
use std::{ffi::c_void, path::PathBuf, ptr, sync::Arc};

#[repr(C)]
pub(super) struct Owners {
    // Existing reviewed station callbacks interpret the context's initial field as Counters.
    pub(super) station: Counters,
    phone: PhoneHost,
    trace: Trace,
    reads: std::sync::Mutex<Option<ReadIdentity>>,
    pub(super) read_count: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReadIdentity {
    pub(super) peer: usize,
    pub(super) source: std::net::SocketAddr,
    pub(super) generation: (u64,u64),
    pub(super) clock_epoch: u64,
}
#[derive(Default)]
struct Trace {
    reads: [std::sync::atomic::AtomicUsize; 6],
    resets: [std::sync::atomic::AtomicUsize; 2],
    prepares: [std::sync::atomic::AtomicUsize; 3],
    sends: [std::sync::atomic::AtomicUsize; 4],
    deadlines: [std::sync::atomic::AtomicUsize; 4],
    retirement: [std::sync::atomic::AtomicUsize; 3],
}

fn mark(counter: &std::sync::atomic::AtomicUsize) {
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn counts<const N: usize>(counters: &[std::sync::atomic::AtomicUsize; N]) -> [usize; N] {
    std::array::from_fn(|i| counters[i].load(std::sync::atomic::Ordering::Relaxed))
}

struct Adapter;
// SAFETY: each wrapped peer is one uniquely owned actual provider call. The test retains
// the host and the opaque registrations until the normal management stop succeeds.
unsafe impl Frontend for Adapter {
    unsafe fn host<'a>(ctx: *mut c_void) -> Option<&'a PhoneHost> {
        Some(&unsafe {ctx.cast::<Owners>().as_ref()}?.phone)
    }
    unsafe fn call<'a>(peer: *mut c_void) -> Option<&'a PrivateCall> {
        unsafe {peer.cast::<PrivateCall>().as_ref()}
    }
    fn wrap(call: PrivateCall) -> *mut c_void { Box::into_raw(Box::new(call)).cast() }
    unsafe fn discard(peer: *mut c_void) {
        if !peer.is_null() { unsafe {drop(Box::from_raw(peer.cast::<PrivateCall>()))}; }
    }
}

const ORIGINAL: rptadv_private_phone_host::abi::rptadv_phone_provider_v1 = frontend::operations::<Adapter>();
static PHONE: rptadv_private_phone_host::abi::rptadv_phone_provider_v1 =
    rptadv_private_phone_host::abi::rptadv_phone_provider_v1 { send_prepared_timed: Some(timed), ..ORIGINAL };

impl Owners {
    pub(super) fn new() -> Box<Self> {
        let paths = |name| PathBuf::from(std::env::var_os(name).expect("qualified SDK path"));
        // The same product callbacks called by standalone and Asterisk inspect and adopt
        // this live opaque handle. Only account configuration in the actual store grants ingress.
        let product = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
        let authorize = product.authorize_incoming.unwrap();
        let admit = product.incoming.unwrap();
        let phone = PhoneHost::new(NativePaths {
            sip: paths("RPTADV_SIP_TEST_PROVIDER"), jitter: paths("RPTADV_JITTER_TEST_PROVIDER"),
            iax: paths("RPTADV_IAX_TEST_PROVIDER"), g711: paths("RPTADV_G711_TEST_PROVIDER"),
        }, Arc::new(move |call| frontend::incoming::<Adapter>(call,
            |request, peer| unsafe {authorize(request.cast(),peer)},
            |request, peer| unsafe {admit(request.cast(),peer)})));
        Box::new(Self { station: Counters::default(), trace: Default::default(), reads:Default::default(), read_count:Default::default(), phone })
    }

    pub(super) fn table(&mut self, binding: &abi::rptadv_station_binding_v1) -> abi::rptadv_host_services_v10 {
        let mut host = unsafe { *fixture::host_descriptor() };
        host.context = ptr::from_mut(self).cast();
        host.station_binding = binding;
        host.radio_quiesce = Some(quiesce);
        host.radio_update = Some(reject_unexpected_radio_update);
        host.phone_provider = ptr::from_ref(&PHONE).cast();
        host.peer_info = Some(info);
        host.peer_rate = Some(rate);
        host.peer_ready = Some(ready);
        host.peer_destroy = Some(destroy);
        host.peer_step_attach = Some(attach);
        host.peer_step_stop = Some(stop);
        host.peer_step_detach = Some(detach);
        host.peer_step_wake = Some(wake);
        host.peer_prepare_audio = Some(prepare);
        host.peer_reset_encoder = Some(reset);
        host.peer_send_prepared = Some(untimed);
        host.peer_elapsed_ms = Some(elapsed);
        host.peer_set_deadline = Some(deadline);
        host.peer_read_source = Some(read);
        host.peer_send_digit = Some(digit);
        host
    }
    pub(super) fn read_identity(&self) -> ReadIdentity {
        self.reads.lock().unwrap().expect("a genuine private source read")
    }
    pub(super) fn retirement(&self) -> [usize; 3] { counts(&self.trace.retirement) }
    pub(super) fn trace(&self) {
        eprintln!("IAX native read[idle,progress,held,other,closed,error]={:?} reset[ok,error]={:?} prepare[bytes,buffered,error]={:?} send[accepted,blocked,expired,error]={:?} deadline[now,timed,parked,error]={:?}",
            counts(&self.trace.reads), counts(&self.trace.resets), counts(&self.trace.prepares),
            counts(&self.trace.sends), counts(&self.trace.deadlines));
    }
    pub(super) fn joined(&self) -> bool { self.phone.stop() == Ok(true) }
}

unsafe extern "C" fn reject_unexpected_radio_update(_: *mut c_void, _: *mut c_void, _: u32) -> i32 { -1 }
unsafe extern "C" fn info(_: *mut c_void, peer: *const c_void, out: *mut abi::rptadv_peer_info_v1) -> i32 {
    unsafe { Adapter::call(peer.cast_mut()).map_or(-1, |call| frontend::info(call, out.cast())) }
}
unsafe extern "C" fn rate(_: *mut c_void, peer: *const c_void) -> u32 {
    unsafe { Adapter::call(peer.cast_mut()) }.map_or(0, PrivateCall::rate)
}
unsafe extern "C" fn ready(_: *mut c_void, peer: *mut c_void) -> i32 {
    unsafe { Adapter::call(peer) }.map_or(-1, |call| i32::from(call.established()))
}
unsafe extern "C" fn destroy(ctx: *mut c_void, peer: *mut c_void) {
    unsafe {Adapter::discard(peer)};
    if !peer.is_null() {
        if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} { mark(&owner.trace.retirement[2]); }
    }
}
unsafe extern "C" fn attach(_: *mut c_void, peer: *mut c_void, step: abi::rptadv_peer_step_v1,
    ctx: *mut c_void, out: *mut *mut c_void) -> i32 {
    let Some(out) = (unsafe {out.as_mut()}) else {return -1};
    *out = ptr::null_mut();
    let (Some(call),Some(step)) = (unsafe {Adapter::call(peer)},step) else {return -1};
    let Ok(registration) = (unsafe {call.attach(step,ctx)}) else {return -1};
    *out = Box::into_raw(Box::new(registration)).cast();
    0
}
unsafe extern "C" fn stop(ctx: *mut c_void, token: *mut c_void) {
    if let Some(registration) = unsafe {token.cast::<Registration>().as_ref()} {
        registration.request_stop();
        if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} { mark(&owner.trace.retirement[0]); }
    }
}
unsafe extern "C" fn wake(_: *mut c_void, token: *mut c_void) {
    if let Some(registration) = unsafe {token.cast::<Registration>().as_ref()} { registration.wake(); }
}
unsafe extern "C" fn detach(ctx: *mut c_void, token: *mut c_void) {
    if !token.is_null() {
        unsafe {drop(Box::from_raw(token.cast::<Registration>()))};
        if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} { mark(&owner.trace.retirement[1]); }
    }
}
unsafe extern "C" fn prepare(ctx: *mut c_void, peer: *mut c_void, samples: *const f32, count: usize,
    bytes: *mut u8, space: usize, flush: u32, out: *mut abi::rptadv_codec_encode_result_v1) -> i32 {
    let status = unsafe {Adapter::call(peer).map_or(-1, |c| frontend::prepare_audio(c,samples,count,bytes,space,flush,out.cast()))};
    if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} {
        let class = if !(0..=2).contains(&status) {2} else if unsafe {out.as_ref()}.is_some_and(|r| r.encoded_bytes != 0) {0} else {1};
        mark(&owner.trace.prepares[class]);
    }
    status
}
unsafe extern "C" fn reset(ctx: *mut c_void, peer: *mut c_void) -> i32 {
    let status = unsafe {Adapter::call(peer)}.and_then(|c| c.reset_encoder().ok()).map_or(-1,|()|0);
    if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} { mark(&owner.trace.resets[usize::from(status != 0)]); }
    status
}
unsafe extern "C" fn timed(ctx: *mut c_void, peer: *mut c_void, data: *const u8, count: usize,
    frames: u32, timestamp: u32, deadline: u32) -> i32 {
    let status = unsafe { ORIGINAL.send_prepared_timed.unwrap()(ctx,peer,data,count,frames,timestamp,deadline) };
    if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} {
        mark(&owner.trace.sends[match status {0=>0,1=>1,2=>2,_=>3}]);
        if !(0..=2).contains(&status) {
            let shape = match (count, frames) {
                (160, 160) => "complete G.711 packet",
                (n, f) if n == f as usize && (1..160).contains(&n) => "short G.711 packet",
                _ => "other packet shape",
            };
            eprintln!("private PHONE timed send rejected: {shape}");
        }
    }
    status
}
// Private egress must use the grouped, source-timed sender in PHONE.
unsafe extern "C" fn untimed(_: *mut c_void, _: *mut c_void, _: *const u8, _: usize, _: u32, _: u32) -> i32 { -1 }
unsafe extern "C" fn elapsed(_: *mut c_void, peer: *mut c_void) -> u32 {
    unsafe {Adapter::call(peer)}.and_then(|c| c.elapsed_ms().ok()).unwrap_or(u32::MAX)
}
unsafe extern "C" fn deadline(ctx: *mut c_void, peer: *mut c_void, after: u64) -> i32 {
    let status = unsafe {Adapter::call(peer)}.and_then(|c|c.set_deadline(after).ok()).map_or(-1,|()|0);
    if let Some(owner) = unsafe {ctx.cast::<Owners>().as_ref()} {
        let class = if status != 0 {3} else {match after {0=>0,u64::MAX=>2,_=>1}};
        mark(&owner.trace.deadlines[class]);
    }
    status
}
unsafe extern "C" fn digit(_: *mut c_void, peer: *mut c_void, symbol: u8) -> i32 {
    unsafe {Adapter::call(peer)}.and_then(|c|c.send_digit(symbol).ok()).unwrap_or(-1)
}
unsafe extern "C" fn read(ctx: *mut c_void, peer: *mut c_void, completion: u32,
    sink: *const abi::rptadv_peer_source_sink_v1, _: abi::rptadv_peer_source_text_v1,
    _: *mut c_void, out: *mut abi::rptadv_peer_source_result_v1) -> i32 {
    let (Some(owners),Some(call))=(unsafe{ctx.cast::<Owners>().as_ref()},unsafe{Adapter::call(peer)}) else{return -1};
    if let Some(input)=unsafe{sink.as_ref()} {
        if input.struct_size as usize >= std::mem::size_of::<abi::rptadv_peer_source_sink_v1>() {
            owners.read_count.fetch_add(1, std::sync::atomic::Ordering::Release);
            *owners.reads.lock().unwrap_or_else(|e|e.into_inner())=Some(ReadIdentity {
                peer:peer as usize, source:call.source(), generation:call.generations(), clock_epoch:input.host_clock_epoch,
            });
        }
    }
    let status = unsafe { frontend::read(call,completion,sink.cast(),out.cast()) };
    let class = match (status, unsafe {out.as_ref()}) {
        (0, Some(r)) if r.state == 3 => 4,
        (0, Some(r)) => match r.progress {0=>0,1=>1,2=>2,_=>3},
        _ => 5,
    };
    mark(&owners.trace.reads[class]);
    status
}
