use super::*;
use crate::bindings as ffi;
use crate::services::echo_tests::support::Fixture as EchoFixture;
use std::{cell::Cell, ffi::c_void, mem::size_of, ptr};

thread_local! {
    static START_RESULT: Cell<i32> = const { Cell::new(0) };
    static STOP_RESULT: Cell<i32> = const { Cell::new(0) };
    static INCOMING_RESULT: Cell<i32> = const { Cell::new(0) };
    static REENTRANT_UNLOAD: Cell<Option<i32>> = const { Cell::new(None) };
    static RELOAD_RESULT: Cell<i32> = const { Cell::new(0) };
    static POLLS: Cell<usize> = const { Cell::new(0) };
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
    _: *const ffi::rptadv_host_services_v11,
    _: *const ffi::rptadv_control_descriptor_v1,
    _: *const ffi::rptadv_file_descriptor,
    _: *const ffi::rptadv_speech_descriptor,
    _: *const std::ffi::c_char,
    _: usize,
) -> i32 {
    START_RESULT.get()
}

unsafe extern "C" fn reload_working() -> i32 {
    REENTRANT_UNLOAD.set(Some(super::unload()));
    1
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
    abi_version: 8,
    capability: *b"rptadv.prod8\0\0\0\0",
    reload_working: Some(reload_working),
    stop: Some(stop),
    authorize_incoming: Some(authorize_incoming),
    incoming: Some(incoming),
    link_command: Some(link_command),
    link_status: Some(link_status),
    digit: Some(digit),
    site_command: Some(site_command),
    reload_poll: Some(reload_ready),
};
unsafe extern "C" fn reload_ready() -> i32 {
    POLLS.set(POLLS.get() + 1);
    RELOAD_RESULT.get()
}

static MANAGEMENT: ffi::rptadv_product_management_v1 = ffi::rptadv_product_management_v1 {
    struct_size: size_of::<ffi::rptadv_product_management_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.mgmt1\0\0\0\0",
    start: Some(start),
};
struct Clear;
impl Drop for Clear {
    fn drop(&mut self) {
        let _selected = STATE.lock().unwrap_or_else(|e| e.into_inner()).take();
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
    with_running_asl(|_| operation());
}

fn with_running_asl(operation: impl FnOnce(&ffi::rptadv_asl_admission_v1)) {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let product = unsafe { Product::open(ptr::from_ref(&DESCRIPTOR)) }.unwrap();
    *TEST_PRODUCT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(product);
    let mut services = crate::services::HostServicesOwner::new();
    services.install_asl_fixture();
    let host = services.descriptor();
    let group = unsafe { &*host.asl_provider };
    unsafe extern "C" fn authorize(
        id: *const ffi::rptadv_asl_identity_v1,
        remote: ffi::rptadv_peer_bytes_v1,
        source: ffi::rptadv_peer_bytes_v1,
        out: *mut *mut c_void,
    ) -> i32 {
        if unsafe { (*id).wire } != 1000
            || unsafe { std::slice::from_raw_parts(remote.data, remote.length) } != b"2000"
            || unsafe { std::slice::from_raw_parts(source.data, source.length) } != b"192.0.2.1"
        {
            return -1;
        }
        unsafe { out.write(ptr::dangling_mut::<u8>().cast()) };
        0
    }
    unsafe extern "C" fn complete(_: *mut c_void, peer: *mut c_void) -> i32 {
        if peer.is_null() {
            0
        } else {
            unsafe { incoming(ptr::null(), peer) }
        }
    }
    let admission = ffi::rptadv_asl_admission_v1 {
        struct_size: size_of::<ffi::rptadv_asl_admission_v1>() as u32,
        abi_version: 1,
        authorize: Some(authorize),
        complete: Some(complete),
    };
    let mut row: ffi::rptadv_asl_account_v2 = unsafe { std::mem::zeroed() };
    row.struct_size = size_of::<ffi::rptadv_asl_account_v2>() as u32;
    row.abi_version = 2;
    row.id.station_uuid = [1; 16];
    row.id.account_uuid = [2; 16];
    row.id.generation = 1;
    row.id.wire = 1000;
    row.id.udp_port = 4569;
    let mut token = ptr::null_mut();
    assert_eq!(
        unsafe { group.prepare.unwrap()(host.context, 0, 1, &row, 1, &admission, &mut token) },
        0
    );
    for phase in [1, 2, 4] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let code = unsafe { group.step.unwrap()(host.context, token, phase) };
            if code != 1 {
                assert_eq!(code, 0);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    *STATE.lock().unwrap() = Some(State {
        product,
        services,
        registration: None,
    });
    let _clear = Clear;
    operation(&admission);
}

#[test]
fn product_descriptor_rejects_every_incompatible_prefix_and_missing_operation() {
    assert!(unsafe { Product::open(ptr::null()) }.is_none());
    for field in 0..12 {
        let mut descriptor = DESCRIPTOR;
        match field {
            0 => descriptor.struct_size -= 1,
            1 => descriptor.abi_version += 1,
            2 => descriptor.capability[0] = b'x',
            3 => descriptor.reload_working = None,
            4 => descriptor.stop = None,
            5 => descriptor.authorize_incoming = None,
            6 => descriptor.incoming = None,
            7 => descriptor.link_command = None,
            8 => descriptor.link_status = None,
            9 => descriptor.digit = None,
            10 => descriptor.site_command = None,
            _ => descriptor.reload_poll = None,
        }
        assert!(
            unsafe { Product::open(&descriptor) }.is_none(),
            "field {field}"
        );
    }
}

#[test]
fn product_reload_waits_for_pending_completion() {
    let product = unsafe { Product::open(ptr::from_ref(&DESCRIPTOR)) }.unwrap();
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    let _retained = TRANSITION.lock().unwrap();
    POLLS.set(0);
    RELOAD_RESULT.set(0);
    assert!(product.reload_working());
    assert_eq!(POLLS.get(), 1);
}

#[test]
fn adapter_declines_an_ini_only_configuration_before_registration() {
    let serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("rpt-asterisk-ini-only-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let ini = root.join("rpt_advanced.conf");
    std::fs::write(&ini, "[1000]\nnode_enabled=no\n").unwrap();
    let directory = std::ffi::CString::new(root.to_str().unwrap()).unwrap();
    START_RESULT.set(0);
    STOP_RESULT.set(0);
    crate::module::tests::fail_registration(0);

    // SAFETY: the fixture serializes the adapter and keeps both config storage
    // and the product descriptor alive through the public unload callback.
    unsafe { ast_config_AST_CONFIG_DIR = directory.as_ptr() };
    let adapter = unsafe { &*rptadv_asterisk_descriptor_v2() };
    let status = unsafe {
        adapter.load.unwrap()(
            ptr::dangling_mut(),
            &DESCRIPTOR,
            &MANAGEMENT,
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    };
    let registered = STATE
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|state| state.registration.is_some());
    let unloaded = adapter.unload.unwrap()();
    let empty = STATE.lock().unwrap().is_none();
    unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
    std::fs::remove_file(ini).unwrap();
    std::fs::remove_dir(root).unwrap();
    drop(serial);

    eprintln!(
        "INI_ONLY_CLOSED load={status} registered={registered} unload={unloaded} empty={empty}"
    );
    assert_eq!(
        (unloaded, empty),
        (0, true),
        "adapter must release the fixture"
    );
    assert_eq!(
        (status, registered),
        (1, false),
        "an INI-only directory must not start or register the Asterisk adapter"
    );
}

#[test]
fn lifecycle_retains_live_product_on_failed_stop_and_rolls_back_registration() {
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    RELOAD_RESULT.set(0);
    let root = std::env::temp_dir().join(format!("rpt-adapter-lifecycle-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let directory = std::ffi::CString::new(root.to_str().unwrap()).unwrap();
    let descriptor = rptadv_asterisk_descriptor_v2();
    assert_eq!(unsafe { (*descriptor).abi_version }, 2);
    let load_selected = |product| unsafe {
        load(
            ptr::dangling_mut(),
            product,
            &MANAGEMENT,
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
    assert!(configuration_path("rpt_advanced.service.json").is_none());
    unsafe { ast_config_AST_CONFIG_DIR = directory.as_ptr() };
    assert_eq!(load_selected(&DESCRIPTOR), 1);
    std::fs::write(root.join("rpt_advanced.service.json"), "{}").unwrap();
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
    RELOAD_RESULT.set(-1);
    assert_eq!(super::reload(), -1);
    assert!(
        STATE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .registration
            .is_some()
    );
    RELOAD_RESULT.set(0);
    assert_eq!(super::reload(), 0);
    assert_eq!(unload(), 0);
    assert_eq!(super::reload(), -1);
    unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
    std::fs::remove_file(root.join("rpt_advanced.service.json")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn real_echo_account_failures_preserve_the_running_lifecycle_owner() {
    use std::os::unix::fs::symlink;
    const CHILD: &str = "RPTADV_TEST_ASTERISK_ECHO_FIXTURE";
    if let Some(mode) = std::env::var_os(CHILD) {
        let directory = PathBuf::from(std::env::var_os("RPTADV_TEST_ASTERISK_ECHO_DIR").unwrap());
        std::fs::write(directory.join("rpt_advanced.service.json"), "{}").unwrap();
        let config_directory = std::ffi::CString::new(directory.to_str().unwrap()).unwrap();
        unsafe { ast_config_AST_CONFIG_DIR = config_directory.as_ptr() };
        let control = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let adapter = unsafe { &*rptadv_asterisk_descriptor_v2() };
        assert_eq!(
            unsafe {
                adapter.load.unwrap()(
                    ptr::dangling_mut(),
                    &DESCRIPTOR,
                    &MANAGEMENT,
                    control.cast(),
                    ptr::null(),
                    ptr::null(),
                )
            },
            0
        );
        let host = *STATE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .services
            .descriptor();
        crate::services::echo_tests::accounts::check(
            &host,
            if mode == "rollback-stop" { 99 } else { 7 },
        );
        let library =
            unsafe { libloading::Library::new(directory.join("librptadv_echolink.so.1")) }.unwrap();
        let fault =
            unsafe { library.get::<unsafe extern "C" fn(usize)>(b"fixture_record_fault\0") }
                .unwrap();
        unsafe { fault(80) };
        let refused =
            unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_stop_rejections\0") }
                .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while unsafe { refused() } == 0 {
            assert_ne!(unload(), 0);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_ne!(unload(), 0);
        assert!(STATE.lock().unwrap().is_some());
        if mode == "rollback-stop" {
            return;
        }
        unsafe { fault(0) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while unload() != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(STATE.lock().unwrap().is_none());
        unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
        return;
    }
    for mode in ["retirement", "rollback-stop"] {
        let fixture = EchoFixture::new("managed");
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

#[test]
fn managed_entry_rejects_null_and_short_groups_before_start() {
    let module = std::ptr::NonNull::<u8>::dangling().as_ptr().cast();
    #[repr(C, align(8))]
    struct Prefix(u32);
    let prefix = Prefix(4);
    for table in [std::ptr::null(), std::ptr::from_ref(&prefix).cast()] {
        // SAFETY: the checked entry requires a readable length word only when non-null.
        // The missing product and control groups cannot be used by this failed admission.
        let status = unsafe {
            super::load(
                module,
                std::ptr::null(),
                table,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_ne!(status, 0);
    }
}

#[test]
fn managed_reload_holds_module_until_receipt_and_uncertain_boot_drains() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("rpt-managed-adapter-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("rpt_advanced.service.json");
    std::fs::write(&path, b"{}").unwrap();
    let directory = std::ffi::CString::new(root.to_str().unwrap()).unwrap();
    // SAFETY: the serialized fixture retains this CString through final unload.
    unsafe { ast_config_AST_CONFIG_DIR = directory.as_ptr() };
    let load = || {
        // SAFETY: both complete fixture tables and opaque module outlive all calls.
        // The fixture product ignores providers and returns its selected startup disposition.
        unsafe {
            super::load(
                ptr::dangling_mut(),
                &DESCRIPTOR,
                &MANAGEMENT,
                ptr::null(),
                ptr::null(),
                ptr::null(),
            )
        }
    };
    crate::module::tests::fail_registration(0);
    START_RESULT.set(-1);
    assert_eq!(load(), 1);
    assert!(STATE.lock().unwrap().is_none());
    START_RESULT.set(-2);
    STOP_RESULT.set(-1);
    assert_eq!(load(), 0);
    assert!(
        STATE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .registration
            .is_none()
    );
    assert_eq!(load(), 1);
    assert_eq!(super::unload(), -1);
    STOP_RESULT.set(0);
    assert_eq!(super::unload(), 0);
    START_RESULT.set(0);
    assert_eq!(load(), 0);
    {
        let state = STATE.lock().unwrap();
        let host = state.as_ref().unwrap().services.descriptor();
        assert!(host.struct_size as usize >= size_of::<ffi::rptadv_host_services_v11>());
        // SAFETY: the retained frontend owns both the checked table and its context.
        let group = unsafe { host.phone_provider.as_ref() }
            .expect("managed Asterisk must offer its private-call owner before account selection");
        assert_eq!(group.abi_version, 2);
        assert!(group.struct_size as usize >= size_of::<ffi::rptadv_phone_provider_v2>());
        let mut clock = ffi::rptadv_media_clock_reader_v1 {
            struct_size: size_of::<ffi::rptadv_media_clock_reader_v1>() as u32,
            abi_version: 1,
            clock_kind: 0,
            reserved: 0,
            read_ns: None,
        };
        // SAFETY: the group stays retained and the caller provides the full output record.
        assert_eq!(
            unsafe { group.media_clock_reader.unwrap()(host.context, &mut clock) },
            0
        );
        assert_eq!(clock.clock_kind, 1);
        let read = clock.read_ns.expect("native monotonic clock");
        // SAFETY: both reads complete while the owning frontend code is still loaded.
        let (first, second) = unsafe { (read(), read()) };
        assert!(first != 0 && second >= first);
    }
    assert_eq!(super::reload(), 0);
    assert_eq!(REENTRANT_UNLOAD.get(), Some(-1));
    assert!(
        STATE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .registration
            .is_some()
    );
    assert_eq!(super::unload(), 0);
    // SAFETY: Asterisk no longer holds the test configuration directory.
    unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[path = "../../private-phone-host/tests/support/frontend_account.rs"]
mod test_phone_group;
#[test]
fn frontend_phone_group_authenticates_and_withdraws_the_actual_sip_socket() {
    let mut host = crate::services::HostServicesOwner::new();
    host.install_private(&DESCRIPTOR);
    let table = host.descriptor();
    unsafe { test_phone_group::register(table.context, table.phone_provider.cast()) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while !host.private_stopped() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn managed_asterisk_prepares_and_cancels_an_asl_account_before_opening_a_listener() {
    thread_local! { static STAGE: Cell<(bool,bool)> = const {Cell::new((false,false))}; }
    unsafe extern "C" fn authorize(
        _: *const ffi::rptadv_asl_identity_v1,
        _: ffi::rptadv_peer_bytes_v1,
        _: ffi::rptadv_peer_bytes_v1,
        _: *mut *mut c_void,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn complete(_: *mut c_void, _: *mut c_void) -> i32 {
        1
    }
    unsafe extern "C" fn start(
        host: *const ffi::rptadv_host_services_v11,
        _: *const ffi::rptadv_control_descriptor_v1,
        _: *const ffi::rptadv_file_descriptor,
        _: *const ffi::rptadv_speech_descriptor,
        _: *const std::ffi::c_char,
        _: usize,
    ) -> i32 {
        let host = unsafe { &*host };
        if let Some(provider) = unsafe { host.asl_provider.as_ref() } {
            let mut row: ffi::rptadv_asl_account_v2 = unsafe { std::mem::zeroed() };
            row.struct_size = size_of::<ffi::rptadv_asl_account_v2>() as u32;
            row.abi_version = 2;
            row.id.station_uuid = [1; 16];
            row.id.account_uuid = [2; 16];
            row.id.generation = 1;
            row.id.wire = 1000;
            row.id.udp_port = 4570;
            row.jitter_min_ms = 20;
            row.jitter_initial_ms = 40;
            row.jitter_max_ms = 100;
            let admission = ffi::rptadv_asl_admission_v1 {
                struct_size: size_of::<ffi::rptadv_asl_admission_v1>() as u32,
                abi_version: 1,
                authorize: Some(authorize),
                complete: Some(complete),
            };
            let mut token = ptr::null_mut();
            let prepared = unsafe {
                provider.prepare.unwrap()(host.context, 0, 1, &row, 1, &admission, &mut token)
            } == 0
                && !token.is_null();
            let mut cancelled = false;
            if prepared {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let result = unsafe { provider.step.unwrap()(host.context, token, 4) };
                    if result != 1 {
                        cancelled = result == 0;
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
            STAGE.set((prepared, cancelled));
        }
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root =
        std::env::temp_dir().join(format!("rpt-asterisk-asl-dormant-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("rpt_advanced.service.json");
    std::fs::write(&path, b"{}").unwrap();
    let directory = std::ffi::CString::new(root.to_str().unwrap()).unwrap();
    unsafe { ast_config_AST_CONFIG_DIR = directory.as_ptr() };
    crate::module::tests::fail_registration(0);
    STOP_RESULT.set(0);
    let management = ffi::rptadv_product_management_v1 {
        struct_size: size_of::<ffi::rptadv_product_management_v1>() as u32,
        abi_version: 1,
        capability: *b"rptadv.mgmt1\0\0\0\0",
        start: Some(start),
    };
    let loaded = unsafe {
        super::load(
            ptr::dangling_mut(),
            &DESCRIPTOR,
            &management,
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    };
    let stopped = super::unload();
    let empty = STATE.lock().unwrap().is_none();
    unsafe { ast_config_AST_CONFIG_DIR = ptr::null() };
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
    assert_eq!((loaded, stopped, empty), (0, 0, true));
    let (prepared, cancelled) = STAGE.get();
    eprintln!("ASL_PBX_DORMANT prepared={prepared} cancelled={cancelled} unloaded={empty}");
    assert_eq!(
        (prepared, cancelled),
        (true, true),
        "managed Asterisk must own the dormant ASL account transaction through cancellation"
    );
}

#[test]
fn one_module_unload_drains_the_active_asl_withdrawal() {
    thread_local! {
        static SAW_PENDING: Cell<bool> = const { Cell::new(false) };
        static WITHDRAWAL: Cell<(*mut c_void, usize)> = const { Cell::new((ptr::null_mut(), 0)) };
    }
    // The fixture Product's stop callback consumes the actual ASL owner's phase receipts.
    unsafe extern "C" fn stop_withdrawing() -> i32 {
        let Ok(selected) = STATE.try_lock() else {
            return -1;
        };
        let Some(state) = selected.as_ref() else {
            return -1;
        };
        let host = state.services.descriptor();
        // SAFETY: the installed owner retains this selection until phase 4 completes.
        let group = unsafe { &*host.asl_provider };
        loop {
            let (token, index) = WITHDRAWAL.get();
            let Some(&phase) = [1, 2, 4].get(index) else {
                return 0;
            };
            let receipt = unsafe { group.step.unwrap()(host.context, token, phase) };
            if receipt != 0 {
                if receipt == 1 {
                    SAW_PENDING.set(true);
                }
                return -1;
            }
            WITHDRAWAL.set((if phase == 4 { ptr::null_mut() } else { token }, index + 1));
        }
    }
    static WITHDRAWING_PRODUCT: ffi::rptadv_product_descriptor_v1 =
        ffi::rptadv_product_descriptor_v1 {
            stop: Some(stop_withdrawing),
            ..DESCRIPTOR
        };
    with_running_asl(|admission| {
        {
            let mut selected = STATE.lock().unwrap();
            let state = selected.as_mut().unwrap();
            assert!(state.services.asl_incoming("1000").is_some());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !state.services.close_asl_ingress() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            let host = state.services.descriptor();
            let group = unsafe { &*host.asl_provider };
            let mut token = ptr::null_mut();
            // SAFETY: the actual published basis is 1. Empty rows withdraw it; the
            // fixture's static callbacks and host remain live through final release.
            let code = unsafe {
                group.prepare.unwrap()(host.context, 1, 2, ptr::null(), 0, admission, &mut token)
            };
            assert_eq!(code, 0);
            assert!(!token.is_null());
            SAW_PENDING.set(false);
            WITHDRAWAL.set((token, 0));
            state.product = unsafe { Product::open(&WITHDRAWING_PRODUCT) }.unwrap();
        }

        let first = super::unload();
        let saw_pending_in_first = SAW_PENDING.get();
        let retained = STATE.lock().unwrap().is_some();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while STATE.lock().unwrap().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "ASL withdrawal failed to drain"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
            let _ = super::unload();
        }
        assert!(
            saw_pending_in_first,
            "Product stop saw the pending ASL phase"
        );
        assert_eq!(WITHDRAWAL.get(), (ptr::null_mut(), 3));
        eprintln!("ASL_PBX_UNLOAD first={first} retained={retained} withdrew=true empty=true");
        assert_eq!(
            (first, retained),
            (0, false),
            "one ordinary module unload must drain the active ASL withdrawal"
        );
    });
}
