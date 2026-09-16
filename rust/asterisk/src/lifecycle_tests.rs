use super::*;
use crate::bindings as ffi;
use std::{cell::Cell, ffi::c_void, mem::size_of, ptr};

thread_local! {
    static START_RESULT: Cell<i32> = const { Cell::new(0) };
    static STOP_RESULT: Cell<i32> = const { Cell::new(0) };
    static INCOMING_RESULT: Cell<i32> = const { Cell::new(0) };
}

#[unsafe(no_mangle)]
static mut ast_config_AST_CONFIG_DIR: *const std::ffi::c_char = ptr::null();

pub(crate) fn incoming_result(result: i32) {
    INCOMING_RESULT.set(result);
}

static DIGITS: Mutex<String> = Mutex::new(String::new());

unsafe extern "C" fn start(
    _: *const ffi::rptadv_host_services_v1,
    _: *const ffi::rptadv_control_descriptor_v1,
    _: *const ffi::rptadv_file_descriptor,
    _: *const ffi::rptadv_speech_descriptor,
    _: *const std::ffi::c_char,
    _: usize,
) -> i32 {
    START_RESULT.get()
}

unsafe extern "C" fn reload(_: *const std::ffi::c_char, _: usize) -> i32 {
    0
}

unsafe extern "C" fn stop() -> i32 {
    STOP_RESULT.get()
}

unsafe extern "C" fn authorize_incoming(
    local: *const std::ffi::c_char,
    local_length: usize,
    _: *const std::ffi::c_char,
    _: usize,
    _: *const std::ffi::c_char,
    _: usize,
) -> i32 {
    if local.is_null() {
        return -1;
    }
    let local = unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) };
    -i32::from(local == b"missing")
}

unsafe extern "C" fn incoming(
    _: *const std::ffi::c_char,
    _: usize,
    _: *const std::ffi::c_char,
    _: usize,
    _: *const std::ffi::c_char,
    _: usize,
    peer: *mut c_void,
) -> i32 {
    if peer.is_null() || INCOMING_RESULT.get() == 1 {
        1
    } else {
        unsafe { crate::services::destroy_raw(peer) };
        INCOMING_RESULT.get()
    }
}

unsafe extern "C" fn link_command(
    local: *const std::ffi::c_char,
    local_length: usize,
    _: *const std::ffi::c_char,
    _: usize,
    _: u32,
) -> i32 {
    let local = unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) };
    -i32::from(local == b"missing")
}

unsafe extern "C" fn link_status(
    local: *const std::ffi::c_char,
    local_length: usize,
    sink: ffi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    if local.is_null() || local_length == 0 {
        return -1;
    }
    let Some(sink) = sink else {
        return -1;
    };
    let local = unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) };
    match local {
        b"null-context" => {
            unsafe { sink(ptr::null_mut(), ptr::null(), 1) };
            return 0;
        }
        b"null-text" => {
            unsafe { sink(context, ptr::null(), 1) };
            return 0;
        }
        b"empty" => {
            unsafe { sink(context, ptr::null(), 0) };
            return 0;
        }
        b"invalid" => {
            unsafe { sink(context, c"\xff".as_ptr(), 1) };
            return 0;
        }
        b"missing" => return -1,
        _ => (),
    }
    let text = b"no active links\n";
    unsafe { sink(context, text.as_ptr().cast(), text.len()) };
    0
}

unsafe extern "C" fn digit(
    local: *const std::ffi::c_char,
    local_length: usize,
    digit: u8,
    completed: *mut u32,
) -> i32 {
    if local.is_null() || completed.is_null() {
        return -1;
    }
    let local = unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) };
    if local == b"missing" {
        return -1;
    }
    let mut digits = DIGITS.lock().unwrap_or_else(|error| error.into_inner());
    digits.push(digit as char);
    if digit != b'#' {
        unsafe { completed.write(0) };
        return 0;
    }
    let result = match digits.as_str() {
        "*806#" => 1,
        "*32000#" => {
            digits.clear();
            return -1;
        }
        _ => 0,
    };
    digits.clear();
    unsafe { completed.write(result) };
    0
}

static DESCRIPTOR: ffi::rptadv_product_descriptor_v1 = ffi::rptadv_product_descriptor_v1 {
    struct_size: size_of::<ffi::rptadv_product_descriptor_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.product\0\0",
    start: Some(start),
    reload: Some(reload),
    stop: Some(stop),
    authorize_incoming: Some(authorize_incoming),
    incoming: Some(incoming),
    link_command: Some(link_command),
    link_status: Some(link_status),
    digit: Some(digit),
};

struct Clear;
impl Drop for Clear {
    fn drop(&mut self) {
        *TEST_PRODUCT
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        DIGITS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        INCOMING_RESULT.set(0);
    }
}

pub(crate) fn with_running(operation: impl FnOnce()) {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let product = unsafe { Product::open(ptr::from_ref(&DESCRIPTOR)) }.unwrap();
    *TEST_PRODUCT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(product);
    let _clear = Clear;
    operation();
}

#[test]
fn product_descriptor_rejects_every_incompatible_prefix_and_missing_operation() {
    assert!(unsafe { Product::open(ptr::null()) }.is_none());
    for field in 0..11 {
        let mut descriptor = DESCRIPTOR;
        match field {
            0 => descriptor.struct_size -= 1,
            1 => descriptor.abi_version += 1,
            2 => descriptor.capability[0] = b'x',
            3 => descriptor.start = None,
            4 => descriptor.reload = None,
            5 => descriptor.stop = None,
            6 => descriptor.authorize_incoming = None,
            7 => descriptor.incoming = None,
            8 => descriptor.link_command = None,
            9 => descriptor.link_status = None,
            _ => descriptor.digit = None,
        }
        assert!(
            unsafe { Product::open(&descriptor) }.is_none(),
            "field {field}"
        );
    }
}

#[test]
fn lifecycle_retains_live_product_on_failed_stop_and_rolls_back_registration() {
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    let root = std::env::temp_dir().join(format!("rpt-adapter-lifecycle-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let directory = std::ffi::CString::new(root.to_str().unwrap()).unwrap();
    let descriptor = rptadv_asterisk_descriptor_v1();
    assert_eq!(unsafe { (*descriptor).abi_version }, 1);
    let load_selected = |product| unsafe {
        load(
            ptr::dangling_mut(),
            product,
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    };
    assert_eq!(unload(), 0);
    assert_eq!(load_selected(ptr::null()), 1);
    assert_eq!(load_selected(&DESCRIPTOR), 1);
    assert_eq!(super::reload(), -1);
    unsafe { ast_config_AST_CONFIG_DIR = c"\xff".as_ptr() };
    assert!(configuration().is_none());
    unsafe { ast_config_AST_CONFIG_DIR = directory.as_ptr() };
    assert!(configuration().is_none());
    std::fs::write(root.join("rpt_advanced.conf"), "[1000]\nnode_enabled=no\n").unwrap();
    START_RESULT.set(-1);
    assert_eq!(load_selected(&DESCRIPTOR), 1);
    START_RESULT.set(0);
    crate::module::tests::fail_registration(1);
    assert_eq!(load_selected(&DESCRIPTOR), 1);
    STOP_RESULT.set(-1);
    assert_eq!(load_selected(&DESCRIPTOR), 0);
    assert!(
        STATE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .registration
            .is_none()
    );
    assert_eq!(load_selected(&DESCRIPTOR), 1);
    assert_eq!(unload(), -1);
    STOP_RESULT.set(0);
    assert_eq!(unload(), 0);
    crate::module::tests::fail_registration(0);
    assert_eq!(load_selected(&DESCRIPTOR), 0);
    assert_eq!(super::reload(), 0);
    assert_eq!(unload(), 0);
    assert_eq!(super::reload(), -1);
    unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
    std::fs::remove_file(root.join("rpt_advanced.conf")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
