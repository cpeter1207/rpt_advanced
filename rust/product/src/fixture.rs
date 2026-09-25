//! Test-only deterministic allocation seams.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::VecDeque,
    ffi::{c_char, c_void},
    mem::size_of,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicPtr, AtomicUsize, Ordering},
    },
};

thread_local! { static FAIL_BYTES: Cell<usize> = const { Cell::new(0) }; }
struct Allocator;

fn reject(bytes: usize) -> bool {
    FAIL_BYTES
        .try_with(|slot| {
            if slot.get() == bytes {
                slot.set(0);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}

// SAFETY: layouts and live pointers are forwarded unchanged to System. Returning null
// for one selected allocation is explicitly permitted by GlobalAlloc.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if reject(layout.size()) {
            ptr::null_mut()
        } else {
            unsafe { System.alloc(layout) }
        }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        if reject(bytes) {
            ptr::null_mut()
        } else {
            unsafe { System.realloc(pointer, layout, bytes) }
        }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

pub static LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub static RADIO_DROPS: AtomicUsize = AtomicUsize::new(0);
pub static RADIO_OPENS: AtomicUsize = AtomicUsize::new(0);
pub static PEER_DROPS: AtomicUsize = AtomicUsize::new(0);
pub static PEER_OPENS: AtomicUsize = AtomicUsize::new(0);
pub static RADIO_OPEN_RESULT: AtomicUsize = AtomicUsize::new(0);
pub static CONTROL_STOP_RESULT: AtomicUsize = AtomicUsize::new(0);
pub static CONTROL_SUBMIT_MODE: AtomicUsize = AtomicUsize::new(0);
pub static CONTROL_REVISION: AtomicPtr<std::sync::atomic::AtomicU64> =
    AtomicPtr::new(ptr::null_mut());
pub static LOCAL_TIME_RESULT: AtomicUsize = AtomicUsize::new(0);
pub static NOTICE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static PEER_DIAL_RESULT: AtomicUsize = AtomicUsize::new(0);
pub static PEER_DIAL_DELAY_MS: AtomicUsize = AtomicUsize::new(0);
/// Fail only the next preparation handshake, never an active reader's control text.
pub static PEER_PREPARE_TEXT_RESULT: AtomicUsize = AtomicUsize::new(0);
/// Wait for real reader termination before refresh to exercise the publication race.
pub static PEER_WAIT_FOR_END: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn wait_for_peer_end(reader: &crate::link::session::PeerReader) {
    if PEER_WAIT_FOR_END.swap(0, Ordering::AcqRel) != 0 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !reader.ended() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            reader.ended(),
            "peer must terminate through its real read failure"
        );
    }
}
pub static PEER_DIGITS: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
pub static RADIO_READY: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn local_time(
    _: *mut c_void,
    _: i64,
    result: *mut crate::abi::rptadv_local_time_v1,
) -> i32 {
    if LOCAL_TIME_RESULT
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return -1;
    }
    let Some(result) = (unsafe { result.as_mut() }) else {
        return -1;
    };
    result.valid = 1;
    result.year = 2030;
    result.month = 1;
    result.day = 1;
    result.weekday = 2;
    result.hour = 9;
    result.minute = 7;
    result.second = 3;
    0
}
unsafe extern "C" fn notice(_: *mut c_void, _: *const c_char, _: usize, _: u32) {
    NOTICE_COUNT.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn reaper() {}
unsafe extern "C" fn lookup(
    _: *mut c_void,
    _: u32,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    remote: *const c_char,
    remote_length: usize,
    _: *const c_char,
    _: usize,
    output: *mut c_char,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    if remote.is_null() || output.is_null() || written.is_null() {
        return -1;
    }
    let remote = unsafe { std::slice::from_raw_parts(remote.cast::<u8>(), remote_length) };
    let mut destination = b"radio@fixture/".to_vec();
    destination.extend_from_slice(remote);
    if destination.len() > capacity {
        return -1;
    }
    unsafe {
        ptr::copy_nonoverlapping(destination.as_ptr(), output.cast(), destination.len());
        written.write(destination.len());
    }
    0
}
unsafe extern "C" fn radio_open(
    _: *mut c_void,
    name: *const c_char,
    length: usize,
    _: usize,
    output: *mut *mut c_void,
) -> i32 {
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    if RADIO_OPEN_RESULT.swap(0, Ordering::AcqRel) != 0 {
        *output = ptr::null_mut();
        return -1;
    }
    RADIO_OPENS.fetch_add(1, Ordering::Relaxed);
    *output = Box::into_raw(Box::new(FakeRadio {
        name: String::from_utf8(
            unsafe { std::slice::from_raw_parts(name.cast(), length) }.to_vec(),
        )
        .unwrap(),
        stop: Default::default(),
        thread: None,
    }))
    .cast();
    0
}
unsafe extern "C" fn peer_ready(_: *mut c_void, _: *mut c_void) -> i32 {
    std::thread::sleep(std::time::Duration::from_millis(1));
    i32::from(
        !PEER_DIGITS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty(),
    )
}
struct FakeRadio {
    name: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
pub(crate) unsafe fn radio_name(handle: *mut c_void) -> String {
    unsafe { &*handle.cast::<FakeRadio>() }.name.clone()
}
unsafe extern "C" fn peer_bind_radio(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> i32 {
    0
}
pub static RADIO_ACTIVATE_RESULT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
unsafe extern "C" fn radio_activate(
    host_context: *mut c_void,
    handle: *mut c_void,
    receive: crate::abi::rptadv_radio_receive_v2,
    receive_context: *mut c_void,
    transmit: crate::abi::rptadv_radio_transmit_v2,
    transmit_context: *mut c_void,
) -> i32 {
    if RADIO_ACTIVATE_RESULT.swap(0, Ordering::AcqRel) != 0 {
        return -1;
    }
    let (Some(receive), Some(transmit)) = (receive, transmit) else {
        return -1;
    };
    let radio = unsafe { &mut *handle.cast::<FakeRadio>() };
    let stop = radio.stop.clone();
    let contexts = (receive_context as usize, transmit_context as usize);
    let always = !host_context.is_null();
    radio.thread = Some(std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            if always || RADIO_READY.load(Ordering::Acquire) != 0 {
                let mut samples = [0.0; 8];
                let mut keyed = 0;
                unsafe {
                    receive(contexts.0 as *mut c_void, 0, samples.as_mut_ptr(), 8);
                    transmit(
                        contexts.1 as *mut c_void,
                        samples.as_mut_ptr(),
                        8,
                        &mut keyed,
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }));
    0
}
unsafe extern "C" fn radio_destroy(_: *mut c_void, handle: *mut c_void) {
    if !handle.is_null() {
        let mut radio = unsafe { Box::from_raw(handle.cast::<FakeRadio>()) };
        radio.stop.store(true, Ordering::Release);
        if let Some(thread) = radio.thread.take() {
            thread.join().unwrap();
        }
        RADIO_DROPS.fetch_add(1, Ordering::Relaxed);
    }
}
unsafe extern "C" fn peer_dial(
    _: *mut c_void,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    _: usize,
    current: crate::abi::rptadv_current_v1,
    context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    if PEER_DIAL_RESULT.swap(0, Ordering::AcqRel) != 0 {
        *output = ptr::null_mut();
        return -1;
    }
    let delay = PEER_DIAL_DELAY_MS.swap(0, Ordering::AcqRel);
    if delay != 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay as u64));
    }
    if current.is_none_or(|current| unsafe { current(context) } == 0) {
        return -1;
    }
    *output = peer();
    0
}
unsafe extern "C" fn peer_rate(_: *mut c_void, _: *const c_void) -> u32 {
    8000
}
unsafe extern "C" fn peer_read(
    _: *mut c_void,
    _: *mut c_void,
    event: crate::abi::rptadv_peer_event_v1,
    context: *mut c_void,
) -> i32 {
    if let Some(event) = event {
        if let Some(digit) = PEER_DIGITS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
        {
            unsafe { event(context, 2, ptr::from_ref(&digit).cast(), 1) };
        } else {
            let samples = [0.25_f32; 4];
            unsafe { event(context, 3, samples.as_ptr().cast(), samples.len()) };
        }
    }
    0
}
unsafe extern "C" fn send_text(
    _: *mut c_void,
    _: *mut c_void,
    text: *const c_char,
    length: usize,
) -> i32 {
    // SAFETY: PeerIo supplies a borrowed CStr's initialized bytes for this call.
    let text = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) };
    if text == b"!NEWKEY1!" {
        -(PEER_PREPARE_TEXT_RESULT.swap(0, Ordering::AcqRel) as i32)
    } else {
        0
    }
}
unsafe extern "C" fn send_digit(_: *mut c_void, _: *mut c_void, _: u8) -> i32 {
    0
}
unsafe extern "C" fn write(_: *mut c_void, _: *mut c_void, _: *const f32, _: usize) -> i32 {
    0
}
unsafe extern "C" fn peer_destroy(_: *mut c_void, handle: *mut c_void) {
    if !handle.is_null() {
        unsafe { drop(Box::from_raw(handle.cast::<u8>())) };
        PEER_DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

static mut HOST: crate::abi::rptadv_host_services_v3 = crate::abi::rptadv_host_services_v3 {
    struct_size: size_of::<crate::abi::rptadv_host_services_v3>() as u32,
    abi_version: 3,
    capability: *b"rptadv.hst3\0",
    context: ptr::null_mut(),
    local_time: Some(local_time),
    command_notice: Some(notice),
    reaper_acquire: Some(reaper),
    reaper_release: Some(reaper),
    directory_lookup: Some(lookup),
    radio_open: Some(radio_open),
    radio_activate: Some(radio_activate),
    radio_destroy: Some(radio_destroy),
    peer_dial: Some(peer_dial),
    peer_bind_radio: Some(peer_bind_radio),
    peer_rate: Some(peer_rate),
    peer_ready: Some(peer_ready),
    peer_read: Some(peer_read),
    peer_send_text: Some(send_text),
    peer_send_digit: Some(send_digit),
    peer_write: Some(write),
    peer_destroy: Some(peer_destroy),
};

pub fn host_descriptor() -> *const crate::abi::rptadv_host_services_v3 {
    &raw const HOST
}

pub fn peer() -> *mut c_void {
    PEER_OPENS.fetch_add(1, Ordering::Relaxed);
    Box::into_raw(Box::new(0_u8)).cast()
}

pub unsafe fn release_peer(handle: *mut c_void) {
    unsafe { peer_destroy(ptr::null_mut(), handle) };
}

unsafe extern "C" fn control_open(_: *const c_char, _: usize) -> *mut c_void {
    ptr::dangling_mut::<u8>().cast()
}
unsafe extern "C" fn control_submit(
    _: *mut c_void,
    task: crate::abi::rptadv_control_task_v1,
) -> i32 {
    match CONTROL_SUBMIT_MODE.swap(0, Ordering::AcqRel) {
        1 => return -1,
        2 => {
            unsafe { task.release.unwrap()(task.context) };
            return 0;
        }
        3 => {
            let revision = CONTROL_REVISION.swap(ptr::null_mut(), Ordering::AcqRel);
            if !revision.is_null() {
                unsafe { &*revision }.fetch_add(1, Ordering::AcqRel);
            }
        }
        _ => {}
    }
    unsafe {
        task.run.unwrap()(task.context);
        task.release.unwrap()(task.context);
    }
    0
}
unsafe extern "C" fn control_stop(_: *mut c_void) -> i32 {
    CONTROL_STOP_RESULT.swap(0, Ordering::AcqRel) as i32
}
unsafe extern "C" fn control_close(_: *mut c_void) -> i32 {
    0
}
static mut CONTROL: crate::abi::rptadv_control_descriptor_v1 =
    crate::abi::rptadv_control_descriptor_v1 {
        abi_version: 1,
        struct_size: size_of::<crate::abi::rptadv_control_descriptor_v1>(),
        capability: c"rptadv.control".as_ptr(),
        open: Some(control_open),
        submit: Some(control_submit),
        stop_and_drain: Some(control_stop),
        close: Some(control_close),
    };

pub fn control_descriptor() -> *const crate::abi::rptadv_control_descriptor_v1 {
    &raw const CONTROL
}

pub fn fail_allocation<T>(bytes: usize, operation: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            FAIL_BYTES.set(0);
        }
    }
    assert_ne!(bytes, 0);
    assert_eq!(FAIL_BYTES.replace(bytes), 0);
    let _clear = Clear;
    let result = operation();
    assert_eq!(FAIL_BYTES.get(), 0, "selected allocation was not reached");
    result
}

#[test]
fn peer_prepare_failure_is_not_consumed_by_worker_text() {
    let _serial = LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner());
    PEER_PREPARE_TEXT_RESULT.store(1, Ordering::Release);
    for (text, expected) in [
        (c"!IAXKEY! 1 1 0 0", 0),
        (c"!NEWKEY1!", -1),
        (c"!NEWKEY1!", 0),
    ] {
        assert_eq!(
            unsafe {
                send_text(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    text.as_ptr(),
                    text.to_bytes().len(),
                )
            },
            expected
        );
    }
}
