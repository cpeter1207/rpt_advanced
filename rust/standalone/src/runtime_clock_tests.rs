//! C2 lifetime probes on the real runtime/provider owners, not future C3/C4 consumers.
use super::*;
use crate::providers::{clock_tests, tests::replace_descriptor};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

type Reader = unsafe extern "C" fn() -> u64;
static LIVE: AtomicBool = AtomicBool::new(false);
static USERS: AtomicUsize = AtomicUsize::new(0);
static READS: AtomicUsize = AtomicUsize::new(0);
static UNLOADS: AtomicUsize = AtomicUsize::new(0);
static VIOLATION: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<Probes>> = Mutex::new(None);

struct Probe {
    stop: mpsc::Sender<()>,
    join: std::thread::JoinHandle<()>,
}
struct Probes {
    host: usize,
    sdk: Reader,
    rollback: bool,
    fail_stop: bool,
    users: Vec<Probe>,
}
extern "C" fn on_read() {
    if !LIVE.load(Ordering::Acquire) {
        VIOLATION.store(true, Ordering::Release);
    }
    READS.fetch_add(1, Ordering::Relaxed);
}
extern "C" fn on_unload() {
    if USERS.load(Ordering::Acquire) != 0 {
        VIOLATION.store(true, Ordering::Release);
    }
    LIVE.store(false, Ordering::Release);
    UNLOADS.fetch_add(1, Ordering::Relaxed);
}
fn spawn_reader(reader: Reader, name: &str) -> Probe {
    let (stop, receiver) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    USERS.fetch_add(1, Ordering::AcqRel);
    let join = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            assert_eq!(unsafe { reader() }, 4242);
            ready.send(()).unwrap();
            receiver.recv().unwrap();
            assert_eq!(unsafe { reader() }, 4242);
            USERS.fetch_sub(1, Ordering::AcqRel);
        })
        .unwrap();
    started.recv().unwrap();
    Probe { stop, join }
}
fn drain(state: &mut Probes) {
    for user in state.users.drain(..) {
        user.stop.send(()).unwrap();
        user.join.join().unwrap();
    }
}
unsafe fn obtain(host: *const abi::rptadv_host_services_v10) -> Reader {
    let host = unsafe { &*host };
    let mut out = abi::rptadv_media_clock_reader_v1 {
        struct_size: size_of::<abi::rptadv_media_clock_reader_v1>() as u32,
        abi_version: 1,
        clock_kind: 0,
        reserved: 0,
        read_ns: None,
    };
    assert_eq!(
        unsafe { host.media_clock_reader.unwrap()(host.context, &mut out) },
        0
    );
    assert_eq!((out.clock_kind, out.reserved), (1, 0));
    out.read_ns.unwrap()
}
unsafe extern "C" fn start(
    host: *const abi::rptadv_host_services_v10,
    _: *const abi::rptadv_control_descriptor_v1,
    _: *const abi::rptadv_file_descriptor,
    _: *const abi::rptadv_speech_descriptor,
    _: *const std::ffi::c_char,
    _: usize,
) -> i32 {
    let reader = unsafe { obtain(host) };
    let mut locked = STATE.lock().unwrap();
    let state = locked.as_mut().unwrap();
    state.host = host as usize;
    assert!(std::ptr::fn_addr_eq(reader, state.sdk));
    // Bounded stand-ins use the two actual retrieval seams. C3/C4 are not installed.
    state
        .users
        .push(spawn_reader(reader, "c2-product-source-probe"));
    state
        .users
        .push(spawn_reader(reader, "c2-product-io-probe"));
    state.users.push(spawn_reader(state.sdk, "c2-sdk-tx-probe"));
    if state.rollback {
        drain(state); // Failed start must return only after its admitted users finish.
        return -1;
    }
    0
}
unsafe extern "C" fn reload(_: *const std::ffi::c_char, _: usize) -> i32 {
    let mut locked = STATE.lock().unwrap();
    let state = locked.as_mut().unwrap();
    let reader = unsafe { obtain(state.host as *const abi::rptadv_host_services_v10) };
    assert!(std::ptr::fn_addr_eq(reader, state.sdk));
    state
        .users
        .push(spawn_reader(reader, "c2-added-device-probe"));
    0
}
unsafe extern "C" fn stop() -> i32 {
    let mut locked = STATE.lock().unwrap();
    let state = locked.as_mut().unwrap();
    if state.fail_stop {
        state.fail_stop = false;
        return -1;
    }
    drain(state);
    0
}
unsafe extern "C" fn poll() -> i32 {
    0
}

#[test]
fn stage_c_clock_host_prefix_and_lifetime() {
    for (case, rollback, fail_stop) in [
        ("rollback", true, false),
        ("stop", false, false),
        ("retry", false, true),
    ] {
        LIVE.store(true, Ordering::Release);
        USERS.store(0, Ordering::Release);
        READS.store(0, Ordering::Release);
        UNLOADS.store(0, Ordering::Release);
        VIOLATION.store(false, Ordering::Release);
        let path =
            std::env::temp_dir().join(format!("rptadv-c2-clock-{}-{case}", std::process::id()));
        let mut providers = crate::providers::ProviderSet::load().unwrap();
        clock_tests::attach_lifetime_fixture(&mut providers, &path, on_read, on_unload);
        let raw = unsafe { providers.descriptor(clock_tests::AUDIO).unwrap().as_ptr() };
        // SDK manifest receives this original pointer, not a new owned descriptor.
        let sdk = unsafe {
            (*raw.cast::<abi::rptadv_audio_adapter_descriptor>())
                .monotonic_now_ns
                .unwrap()
        };
        let mut product = *crate::providers::ProductInspection::open().unwrap().api();
        product.start = Some(start);
        product.reload = Some(reload);
        product.reload_poll = Some(poll);
        product.stop = Some(stop);
        replace_descriptor(&mut providers, "librptadv_product.so.3", product);
        *STATE.lock().unwrap() = Some(Probes {
            host: 0,
            sdk,
            rollback,
            fail_stop,
            users: Vec::new(),
        });
        let active = "[1000]\nnode_enabled=no\n";
        let result = ProductRuntime::start_with_providers(
            std::path::Path::new("rpt_advanced.conf"),
            active,
            crate::resolve_radio_nodes(active).unwrap(),
            crate::secrets::SecretsFile::parse("").unwrap(),
            providers,
        );
        if rollback {
            assert_eq!(result.err(), Some(RuntimeError::Start));
            assert_eq!(READS.load(Ordering::Acquire), 6);
        } else {
            let mut runtime = result.unwrap();
            assert_eq!(USERS.load(Ordering::Acquire), 3);
            assert_eq!(UNLOADS.load(Ordering::Acquire), 0);
            let candidate = "[1000]\nnode_enabled=no\n[1001]\nradio_channel=added\n[radio 1001]\n";
            runtime
                .reload_with_radios(candidate, crate::resolve_radio_nodes(candidate).unwrap())
                .unwrap();
            assert_eq!(USERS.load(Ordering::Acquire), 4);
            let host = runtime.host.as_ref().unwrap().descriptor();
            let mut handle = std::ptr::null_mut();
            assert_eq!(
                unsafe {
                    host.radio_open.unwrap()(
                        host.context,
                        b"added".as_ptr().cast(),
                        5,
                        128,
                        &mut handle,
                    )
                },
                0
            );
            unsafe { host.radio_destroy.unwrap()(host.context, handle) };
            if fail_stop {
                assert_eq!(runtime.stop(), Err(RuntimeError::Stop));
                assert!(runtime.host.is_some() && runtime.providers.is_some());
                assert_eq!(USERS.load(Ordering::Acquire), 4);
                assert_eq!(UNLOADS.load(Ordering::Acquire), 0);
                assert_eq!(unsafe { sdk() }, 4242);
            }
            runtime.stop().unwrap();
            assert!(runtime.host.is_none() && runtime.providers.is_none());
            assert_eq!(READS.load(Ordering::Acquire), if fail_stop { 9 } else { 8 });
        }
        assert_eq!(USERS.load(Ordering::Acquire), 0);
        assert_eq!(UNLOADS.load(Ordering::Acquire), 1);
        assert!(!VIOLATION.load(Ordering::Acquire));
        assert!(!LIVE.load(Ordering::Acquire));
        STATE.lock().unwrap().take();
        // Only this test's compiler source/DSO, after all code leases end.
        std::fs::remove_dir_all(path).unwrap();
    }
}

// Test-only adoption after the same ProductRuntime::start used by the constructor.
impl ProductRuntime {
    pub(crate) fn c5_retain_providers(&mut self, providers: Box<crate::providers::ProviderSet>) {
        assert!(self.providers.is_none());
        self.providers = Some(providers);
    }
}

// Inspect actual outer owners without replacing the production stop callback.
impl ProductRuntime {
    pub(crate) fn c5_retained_reader(&self) -> Option<unsafe extern "C" fn() -> u64> {
        self.composition.as_ref()?;
        let providers = self.providers.as_ref()?;
        let host = self.host.as_ref()?.descriptor();
        let mut out: abi::rptadv_media_clock_reader_v1 = unsafe { std::mem::zeroed() };
        out.struct_size = size_of::<abi::rptadv_media_clock_reader_v1>() as u32;
        out.abi_version = 1;
        if unsafe { host.media_clock_reader?(host.context, &mut out) } != 0 {
            return None;
        }
        let reader = providers.media_clock_reader().ok()??;
        (out.clock_kind == 1
            && out.reserved == 0
            && out
                .read_ns
                .is_some_and(|host_reader| std::ptr::fn_addr_eq(host_reader, reader)))
        .then_some(reader)
    }
}
