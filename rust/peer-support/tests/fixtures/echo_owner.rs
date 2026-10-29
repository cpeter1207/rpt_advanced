//! Isolated ABI fixture: reject early context destruction while an account survives.
#[path = "../../src/echolink/abi.rs"]
pub mod abi;
use std::{
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

static CONTEXTS: AtomicUsize = AtomicUsize::new(0);
static ACCOUNTS: AtomicUsize = AtomicUsize::new(0);
static PEERS: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn fixture_peer() -> *mut abi::Qso {
    PEERS.fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(1_u8)).cast()
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_outbound_peer() -> *mut abi::Qso {
    let peer = fixture_peer();
    unsafe { peer.cast::<u8>().write(2) };
    peer
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_peer_destroy(peer: *mut abi::Qso) {
    assert_ne!(PEERS.fetch_sub(1, Ordering::SeqCst), 0);
    unsafe { drop(Box::from_raw(peer.cast::<u8>())) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_expire(peer: *mut abi::Qso) {
    unsafe { peer.cast::<u8>().write(0) };
}
unsafe extern "C" fn validate(peer: *const abi::Qso, output: *mut abi::PeerInfo) -> abi::Status {
    if unsafe { *peer.cast::<u8>() } == 0 {
        return abi::Status::Closed;
    }
    unsafe { info(peer, output) }
}
unsafe extern "C" fn info(peer: *const abi::Qso, output: *mut abi::PeerInfo) -> abi::Status {
    let bytes = |value: &'static [u8]| abi::ByteSpan {
        data: value.as_ptr(),
        length: value.len(),
    };
    unsafe {
        output.write(abi::PeerInfo {
            struct_size: size_of::<abi::PeerInfo>() as u32,
            abi_version: 1,
            node_number: u32::MAX,
            capabilities: 14,
            account_generation: 4,
            qso_generation: 9,
            callsign: bytes(b"W1XYZ-R"),
            local_node: bytes(b"524950"),
            source: if *peer.cast::<u8>() == 2 {
                bytes(b"")
            } else {
                bytes(b"127.0.0.2:49152")
            },
        })
    };
    abi::Status::Ok
}
unsafe extern "C" fn activate(account: *mut abi::Account, callback: Option<abi::EventFn>, context: *mut c_void) -> abi::Status {
    if let Some(callback) = callback {
        unsafe { callback(context, &abi::Event { struct_size: size_of::<abi::Event>() as u32, kind: 0, qso: ptr::null_mut(), generation: 4, status: abi::Status::Ok, taken: ptr::null_mut() }) };
        if unsafe { *account.cast::<u32>() } == 7 {
            let peer = fixture_peer();
            let mut taken = 0;
            unsafe { callback(context, &abi::Event { struct_size: size_of::<abi::Event>() as u32, kind: 1, qso: peer, generation: 9, status: abi::Status::Pending, taken: &mut taken }) };
            if taken != 1 { unsafe { fixture_peer_destroy(peer) }; }
        }
    }
    abi::Status::Ok
}
unsafe extern "C" fn reconfigure(_: *mut abi::Account, config: *const abi::AccountConfig, _: *const abi::AccountSecrets) -> abi::Status {
    if unsafe { (*config).node_number } == 42 { abi::Status::Full } else { abi::Status::Ok }
}
unsafe extern "C" fn apply(target: *mut abi::Account, prepared: *mut abi::Account) -> abi::Status {
    if unsafe { *prepared.cast::<u32>() } == 42 { return abi::Status::Full; }
    unsafe { ptr::swap(target.cast::<u32>(), prepared.cast::<u32>()) };
    abi::Status::Ok
}
unsafe extern "C" fn dial(
    _: *mut abi::Account,
    node: u32,
    output: *mut *mut abi::Qso,
) -> abi::Status {
    unsafe { output.write(ptr::null_mut()) };
    if node != u32::MAX && node != 42 {
        return abi::Status::Failed;
    }
    unsafe { output.write(fixture_outbound_peer()) };
    abi::Status::Ok
}
struct Registration {
    step: abi::StepFn,
    context: *mut c_void,
    stopped: std::sync::atomic::AtomicBool,
}
unsafe extern "C" fn attach(
    _: *mut abi::Context,
    _: *mut abi::Qso,
    step: Option<abi::StepFn>,
    context: *mut c_void,
    out: *mut *mut abi::Registration,
) -> abi::Status {
    let step = step.unwrap();
    unsafe { step(context) };
    unsafe {
        out.write(
            Box::into_raw(Box::new(Registration {
                step,
                context,
                stopped: std::sync::atomic::AtomicBool::new(false),
            }))
            .cast(),
        )
    };
    abi::Status::Ok
}
unsafe extern "C" fn deadline(_: *mut abi::Qso, after: u64) -> abi::Status {
    assert!(after == 123456 || after == u64::MAX);
    abi::Status::Ok
}
unsafe extern "C" fn read(
    _: *mut abi::Qso,
    completion: u32,
    sink: *const abi::PcmSink,
    text: Option<abi::TextFn>,
    context: *mut c_void,
    out: *mut abi::ReadResult,
) -> abi::Status {
    let mut result = abi::ReadResult::default();
    if let Some(sink) = unsafe { sink.as_ref() } {
        assert!(completion <= 2);
        result.state = 1;
        if completion == 0 {
            assert_eq!(
                unsafe {
                    sink.audio.unwrap()(sink.context, 800, [0.25, 0.5, -0.25].as_ptr(), 3, 1)
                },
                2
            );
            assert_eq!(unsafe { sink.gap.unwrap()(sink.context, 803, 160) }, 80);
            result.progress = 2;
            result.activity = 1;
            result.finish_pending = 1;
        } else {
            result.progress = 3;
            result.activity = 2;
        }
    }
    if let Some(text) = text {
        unsafe { text(context, span(b"W1ABC"), span(b"hello")) };
    }
    unsafe { out.write(result) };
    abi::Status::Ok
}
#[repr(C)]
struct EncodeResult {
    size: u32,
    status: u32,
    consumed: usize,
    bytes: usize,
    start: u64,
    source: u32,
    packet: u32,
    required: usize,
}
unsafe extern "C" fn encode(
    _: *mut abi::Qso,
    pcm: *const f32,
    count: usize,
    _: *mut u8,
    capacity: usize,
    flush: u32,
    result: *mut c_void,
) -> abi::Status {
    assert_eq!(
        unsafe { std::slice::from_raw_parts(pcm, count) },
        [0.25, -0.25]
    );
    assert_eq!((capacity, flush), (4, 7));
    let result = unsafe { &mut *result.cast::<EncodeResult>() };
    assert_eq!(result.size, size_of::<EncodeResult>() as u32);
    result.status = 2;
    result.consumed = 0;
    result.bytes = 0;
    result.required = 12;
    abi::Status::Ok
}
unsafe extern "C" fn reset(_: *mut abi::Qso) -> abi::Status {
    abi::Status::Ok
}
unsafe extern "C" fn send(
    _: *mut abi::Qso,
    bytes: abi::ByteSpan,
    frames: u32,
    timestamp: u32,
) -> abi::Status {
    assert_eq!((frames, timestamp), (160, 4567));
    match unsafe { std::slice::from_raw_parts(bytes.data, bytes.length) } {
        b"ok" => abi::Status::Ok,
        b"full" => abi::Status::Full,
        b"pending" => abi::Status::Pending,
        _ => abi::Status::Failed,
    }
}
unsafe extern "C" fn elapsed(_: *const abi::Qso, out: *mut u64) -> abi::Status {
    unsafe { out.write(0x1_0000_0007) };
    abi::Status::Ok
}
fn span(bytes: &[u8]) -> abi::ByteSpan {
    abi::ByteSpan {
        data: bytes.as_ptr(),
        length: bytes.len(),
    }
}
unsafe extern "C" fn text(
    _: *mut abi::Qso,
    sender: abi::ByteSpan,
    body: abi::ByteSpan,
) -> abi::Status {
    assert_eq!(
        unsafe { std::slice::from_raw_parts(sender.data, sender.length) },
        b"W1ABC"
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts(body.data, body.length) },
        b"hello"
    );
    abi::Status::Ok
}
unsafe extern "C" fn registration_stop(registration: *mut abi::Registration) {
    unsafe { &*registration.cast::<Registration>() }
        .stopped
        .store(true, Ordering::Release);
}
unsafe extern "C" fn registration_wake(registration: *mut abi::Registration) {
    let registration = unsafe { &*registration.cast::<Registration>() };
    if !registration.stopped.load(Ordering::Acquire) {
        unsafe { (registration.step)(registration.context) };
    }
}
unsafe extern "C" fn registration_detach(registration: *mut abi::Registration) {
    unsafe { drop(Box::from_raw(registration.cast::<Registration>())) };
}

unsafe extern "C" fn create(
    _: *const abi::RuntimeConfig,
    out: *mut *mut abi::Context,
) -> abi::Status {
    assert_eq!(CONTEXTS.fetch_add(1, Ordering::SeqCst), 0);
    assert_eq!(ACCOUNTS.load(Ordering::SeqCst), 0);
    unsafe { out.write(Box::into_raw(Box::new(1_u8)).cast()) };
    abi::Status::Ok
}
unsafe extern "C" fn destroy_context(context: *mut abi::Context) {
    assert_eq!(ACCOUNTS.load(Ordering::SeqCst), 0);
    CONTEXTS.fetch_sub(1, Ordering::SeqCst);
    unsafe { drop(Box::from_raw(context.cast::<u8>())) };
}
unsafe extern "C" fn prepare(
    _: *mut abi::Context,
    config: *const abi::AccountConfig,
    _: *const abi::AccountSecrets,
    out: *mut *mut abi::Account,
) -> abi::Status {
    ACCOUNTS.fetch_add(1, Ordering::SeqCst);
    unsafe { out.write(Box::into_raw(Box::new((*config).node_number)).cast()) };
    abi::Status::Ok
}
unsafe extern "C" fn stop(account: *mut abi::Account) -> abi::Status {
    if CONTEXTS.load(Ordering::SeqCst) == 1 && unsafe { *account.cast::<u32>() } != 99 {
        abi::Status::Ok
    } else {
        abi::Status::Failed
    }
}
unsafe extern "C" fn destroy(account: *mut abi::Account) {
    ACCOUNTS.fetch_sub(1, Ordering::SeqCst);
    unsafe { drop(Box::from_raw(account.cast::<u32>())) };
}

#[unsafe(no_mangle)]
pub extern "C" fn fixture_contexts() -> usize {
    CONTEXTS.load(Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub extern "C" fn fixture_accounts() -> usize {
    ACCOUNTS.load(Ordering::SeqCst)
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_peers() -> usize {
    PEERS.load(Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub extern "C" fn rptadv_echolink_descriptor_v1() -> *const abi::Descriptor {
    static TABLE: abi::Descriptor = abi::Descriptor {
        struct_size: size_of::<abi::Descriptor>() as u32,
        abi_version: 1,
        capability: *b"rptadv.echolink.v1\0\0\0\0\0\0",
        capabilities: if cfg!(network) {
            15
        } else if cfg!(partial) {
            3
        } else {
            1
        },
        context_create: Some(create),
        context_destroy: Some(destroy_context),
        account_prepare: Some(prepare),
        account_stop: Some(stop),
        account_destroy: Some(destroy),
        account_activate: if cfg!(network) { Some(activate) } else { None },
        account_reconfigure: if cfg!(network) {
            Some(reconfigure)
        } else {
            None
        },
        dial: if cfg!(network) { Some(dial) } else { None },
        validate_pending: if cfg!(network) { Some(validate) } else { None },
        peer_info: if cfg!(network) { Some(info) } else { None },
        peer_attach: if cfg!(network) { Some(attach) } else { None },
        peer_stop: if cfg!(network) {
            Some(registration_stop)
        } else {
            None
        },
        peer_detach: if cfg!(network) {
            Some(registration_detach)
        } else {
            None
        },
        peer_wake: if cfg!(network) {
            Some(registration_wake)
        } else {
            None
        },
        peer_set_deadline: if cfg!(network) { Some(deadline) } else { None },
        peer_destroy: if cfg!(network) {
            Some(fixture_peer_destroy)
        } else {
            None
        },
        peer_read: if cfg!(network) { Some(read) } else { None },
        peer_prepare_audio: if cfg!(network) { Some(encode) } else { None },
        peer_reset_encoder: if cfg!(network) { Some(reset) } else { None },
        peer_send_prepared: if cfg!(network) { Some(send) } else { None },
        peer_elapsed_ms: if cfg!(network) { Some(elapsed) } else { None },
        peer_send_text: if cfg!(network) { Some(text) } else { None },
        account_apply_prepared: if cfg!(network) { Some(apply) } else { None },
    };
    ptr::from_ref(&TABLE)
}
