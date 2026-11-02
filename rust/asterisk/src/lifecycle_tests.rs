use super::*;
use crate::bindings as ffi;
use crate::services::echo_tests::support::Fixture as EchoFixture;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    mem::size_of,
    ptr,
};

thread_local! {
    static START_RESULT: Cell<i32> = const { Cell::new(0) };
    static STOP_RESULT: Cell<i32> = const { Cell::new(0) };
    static INCOMING_RESULT: Cell<i32> = const { Cell::new(0) };
    static RELOAD_DOCUMENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
static mut ast_config_AST_CONFIG_DIR: *const std::ffi::c_char = ptr::null();

pub(crate) fn incoming_result(result: i32) {
    INCOMING_RESULT.set(result);
}

static DIGITS: Mutex<String> = Mutex::new(String::new());
pub(crate) static SITE_REQUESTS: Mutex<Vec<(String, u32, String, f64)>> = Mutex::new(Vec::new());

unsafe extern "C" fn site_command(
    local: *const std::ffi::c_char,
    length: usize,
    request: *const ffi::rptadv_site_command_v1,
) -> i32 {
    let local = unsafe { std::slice::from_raw_parts(local.cast::<u8>(), length) };
    let request = unsafe { &*request };
    assert_eq!(
        request.struct_size as usize,
        size_of::<ffi::rptadv_site_command_v1>()
    );
    assert_eq!(request.abi_version, 1);
    let target =
        unsafe { std::slice::from_raw_parts(request.target.cast::<u8>(), request.target_length) };
    SITE_REQUESTS.lock().unwrap().push((
        std::str::from_utf8(local).unwrap().into(),
        request.operation,
        std::str::from_utf8(target).unwrap().into(),
        request.value,
    ));
    match local {
        b"missing" => -2,
        b"unsafe" => -3,
        b"invalid" => -4,
        b"unavailable" => -1,
        _ => 0,
    }
}

unsafe extern "C" fn start(
    _: *const ffi::rptadv_host_services_v9,
    _: *const ffi::rptadv_control_descriptor_v1,
    _: *const ffi::rptadv_file_descriptor,
    _: *const ffi::rptadv_speech_descriptor,
    _: *const std::ffi::c_char,
    _: usize,
) -> i32 {
    START_RESULT.get()
}

unsafe extern "C" fn reload(configuration: *const std::ffi::c_char, length: usize) -> i32 {
    let text = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(
            configuration.cast::<u8>(),
            length,
        ))
    }
    .unwrap();
    RELOAD_DOCUMENTS.with_borrow_mut(|documents| documents.push(text.to_owned()));
    -i32::from(text == "rejected-candidate")
}

unsafe extern "C" fn stop() -> i32 {
    STOP_RESULT.get()
}

unsafe extern "C" fn authorize_incoming(
    request: *const ffi::rptadv_incoming_peer_v1,
    _: *mut c_void,
) -> i32 {
    if request.is_null() || unsafe { (*request).local.data.is_null() } {
        return -1;
    }
    let local =
        unsafe { std::slice::from_raw_parts((*request).local.data, (*request).local.length) };
    -i32::from(local == b"missing")
}

unsafe extern "C" fn incoming(_: *const ffi::rptadv_incoming_peer_v1, peer: *mut c_void) -> i32 {
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
    abi_version: 7,
    capability: *b"rptadv.prod7\0\0\0\0",
    start: Some(start),
    reload: Some(reload),
    stop: Some(stop),
    authorize_incoming: Some(authorize_incoming),
    incoming: Some(incoming),
    link_command: Some(link_command),
    link_status: Some(link_status),
    digit: Some(digit),
    inspect_configuration: Some(inspect_configuration),
    inspect_secrets: Some(inspect_secrets),
    site_command: Some(site_command),
};

unsafe extern "C" fn inspect_configuration(
    text: *const std::ffi::c_char,
    length: usize,
    sink: ffi::rptadv_configuration_sink,
    context: *mut std::ffi::c_void,
    _: ffi::rptadv_text_sink_v1,
    _: *mut std::ffi::c_void,
) -> i32 {
    let text = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) };
    if let Some(number) = std::str::from_utf8(text)
        .unwrap()
        .strip_prefix("fixture-echo:")
    {
        use rptadv_peer_support::echolink::abi as echo;
        let number = number.parse::<u32>().unwrap();
        let config = echo::AccountConfig {
            enabled: u32::from(number != 0),
            node_number: number,
            local_node: echo::ByteSpan {
                data: b"524950".as_ptr(),
                length: 6,
            },
            ..Default::default()
        };
        let mut node: ffi::rptadv_node_host_configuration = unsafe { std::mem::zeroed() };
        node.struct_size = size_of::<ffi::rptadv_node_host_configuration>() as u32;
        node.echolink = ptr::from_ref(&config).cast();
        return unsafe { sink.unwrap()(context, &node) };
    }
    i32::from(text == b"inspection-failure")
}

unsafe extern "C" fn inspect_secrets(
    _: *const std::ffi::c_char,
    _: usize,
    _: ffi::rptadv_secret_sink,
    _: *mut std::ffi::c_void,
    _: ffi::rptadv_text_sink_v1,
    _: *mut std::ffi::c_void,
) -> i32 {
    0
}

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
        SITE_REQUESTS.lock().unwrap().clear();
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
    for field in 0..14 {
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
            10 => descriptor.digit = None,
            11 => descriptor.inspect_configuration = None,
            12 => descriptor.inspect_secrets = None,
            _ => descriptor.site_command = None,
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
    RELOAD_DOCUMENTS.with_borrow_mut(Vec::clear);
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
    std::fs::write(root.join("rpt_advanced.conf"), "inspection-failure").unwrap();
    assert_eq!(load_selected(&DESCRIPTOR), 1);
    assert!(STATE.lock().unwrap().is_none());
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
    std::fs::rename(root.join("rpt_advanced.conf"), root.join("saved.conf")).unwrap();
    assert_eq!(super::reload(), -1);
    assert!(STATE.lock().unwrap().is_some());
    std::fs::rename(root.join("saved.conf"), root.join("rpt_advanced.conf")).unwrap();
    std::fs::write(root.join("rpt_advanced.conf"), "accepted-candidate").unwrap();
    assert_eq!(super::reload(), 0);
    std::fs::write(root.join("rpt_advanced.conf"), "rejected-candidate").unwrap();
    assert_eq!(super::reload(), -1);
    RELOAD_DOCUMENTS.with_borrow(|documents| {
        assert_eq!(
            documents,
            &[
                "[1000]\nnode_enabled=no\n",
                "accepted-candidate",
                "rejected-candidate",
                "accepted-candidate"
            ]
        )
    });
    assert_eq!(unload(), 0);
    assert_eq!(super::reload(), -1);
    unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
    std::fs::remove_file(root.join("rpt_advanced.conf")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn real_echo_account_failures_preserve_the_running_lifecycle_owner() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    const CHILD: &str = "RPTADV_TEST_ASTERISK_ECHO_FIXTURE";
    if let Some(mode) = std::env::var_os(CHILD) {
        let directory =
            std::path::PathBuf::from(std::env::var_os("RPTADV_TEST_ASTERISK_ECHO_DIR").unwrap());
        let path = directory.join("rpt_advanced.conf");
        let secrets = directory.join("echolink-secrets.conf");
        std::fs::write(&secrets, "[524950]\ndirectory_password=fixture-value\n").unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config_directory = std::ffi::CString::new(directory.to_str().unwrap()).unwrap();
        unsafe { ast_config_AST_CONFIG_DIR = config_directory.as_ptr() };
        let control = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let load_selected = || unsafe {
            load(
                ptr::dangling_mut(),
                &DESCRIPTOR,
                control.cast(),
                ptr::null(),
                ptr::null(),
            )
        };
        if mode == "rollback-stop" {
            std::fs::write(&path, "fixture-echo:99").unwrap();
            crate::module::tests::fail_registration(1);
            assert_eq!(
                load_selected(),
                0,
                "failed stop must retain loaded callback code"
            );
            assert!(STATE.lock().unwrap().is_some());
            assert_eq!(unload(), -1);
            // This intentionally failed owner remains retained until this
            // isolated child exits; it cannot be safely abandoned/unloaded.
            return;
        }
        std::fs::write(&path, "fixture-echo:42").unwrap();
        assert_eq!(load_selected(), 1);
        assert!(STATE.lock().unwrap().is_none());
        std::fs::write(&path, "fixture-echo:7").unwrap();
        assert_eq!(load_selected(), 0);
        std::fs::write(&path, "fixture-echo:0").unwrap();
        assert_eq!(super::reload(), 0);
        let library =
            unsafe { libloading::Library::new(directory.join("librptadv_echolink.so.1")) }.unwrap();
        let fail =
            unsafe { library.get::<unsafe extern "C" fn(u32)>(b"fixture_disabled_stop_failure\0") }
                .unwrap();
        unsafe { fail(1) };
        std::fs::write(&path, "fixture-empty").unwrap();
        assert_eq!(
            super::reload(),
            0,
            "retirement failure does not undo the committed reload"
        );
        assert_eq!(unload(), -1);
        unsafe { fail(0) };
        assert_eq!(unload(), 0);
        unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
        return;
    }
    for mode in ["retirement", "rollback-stop"] {
        let fixture = EchoFixture::new("network");
        let directory = fixture.0.parent().unwrap();
        symlink(&fixture.0, directory.join("librptadv_echolink.so.1")).unwrap();
        let mut search = vec![directory.to_owned()];
        search.extend(std::env::split_paths(
            &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
        ));
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lifecycle::tests::real_echo_account_failures_preserve_the_running_lifecycle_owner",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, mode)
            .env("RPTADV_TEST_ASTERISK_ECHO_DIR", directory)
            .env("LD_LIBRARY_PATH", std::env::join_paths(search).unwrap())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
