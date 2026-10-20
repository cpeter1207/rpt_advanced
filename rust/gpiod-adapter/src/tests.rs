use super::*;
use std::{
    cell::RefCell,
    ffi::{CStr, c_char, c_void},
    mem::size_of,
    ptr,
};

#[derive(Default)]
struct Driver {
    calls: Vec<&'static str>,
    fail: &'static str,
    path: String,
    line: u32,
    active_low: bool,
    direction: u32,
    initial: i32,
    input: i32,
    writes: Vec<i32>,
}
thread_local! { static DRIVER: RefCell<Driver> = RefCell::default(); }

fn event(name: &'static str) -> bool {
    DRIVER.with(|driver| {
        let mut driver = driver.borrow_mut();
        driver.calls.push(name);
        driver.fail != name
    })
}

fn allocation<T>(name: &'static str) -> *mut T {
    if event(name) {
        ptr::NonNull::<T>::dangling().as_ptr()
    } else {
        ptr::null_mut()
    }
}

// Replace only the external device boundary: all descriptor validation, ownership,
// startup ordering and observation translation below execute the real adapter.
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_chip_open(path: *const c_char) -> *mut abi::gpiod_chip {
    let path = unsafe { CStr::from_ptr(path) }.to_str().unwrap().to_owned();
    let selected = path == "/dev/rpt-site-gpio";
    DRIVER.with(|driver| driver.borrow_mut().path = path);
    if selected {
        allocation("chip_open")
    } else {
        ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_chip_close(_: *mut abi::gpiod_chip) {
    event("chip_close");
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_settings_new() -> *mut abi::gpiod_line_settings {
    allocation("settings_new")
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_settings_free(_: *mut abi::gpiod_line_settings) {
    event("settings_free");
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_settings_set_direction(
    _: *mut abi::gpiod_line_settings,
    direction: abi::gpiod_line_direction,
) -> i32 {
    DRIVER.with(|driver| driver.borrow_mut().direction = direction);
    if event("direction") { 0 } else { -1 }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_settings_set_active_low(
    _: *mut abi::gpiod_line_settings,
    active: bool,
) {
    DRIVER.with(|driver| driver.borrow_mut().active_low = active);
    event("polarity");
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_settings_set_output_value(
    _: *mut abi::gpiod_line_settings,
    value: abi::gpiod_line_value,
) -> i32 {
    DRIVER.with(|driver| driver.borrow_mut().initial = value);
    if event("initial") { 0 } else { -1 }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_config_new() -> *mut abi::gpiod_line_config {
    allocation("config_new")
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_config_free(_: *mut abi::gpiod_line_config) {
    event("config_free");
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_config_add_line_settings(
    _: *mut abi::gpiod_line_config,
    offsets: *const u32,
    count: usize,
    _: *mut abi::gpiod_line_settings,
) -> i32 {
    if count != 1 {
        return -1;
    }
    DRIVER.with(|driver| driver.borrow_mut().line = unsafe { *offsets });
    if event("add_line") { 0 } else { -1 }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_chip_request_lines(
    _: *mut abi::gpiod_chip,
    _: *mut abi::gpiod_request_config,
    _: *mut abi::gpiod_line_config,
) -> *mut abi::gpiod_line_request {
    allocation("request")
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_request_release(_: *mut abi::gpiod_line_request) {
    event("request_release");
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_request_get_value(
    _: *mut abi::gpiod_line_request,
    line: u32,
) -> abi::gpiod_line_value {
    if event("read") {
        DRIVER.with(|driver| {
            let driver = driver.borrow();
            if line == driver.line {
                driver.input
            } else {
                -1
            }
        })
    } else {
        -1
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn gpiod_line_request_set_value(
    _: *mut abi::gpiod_line_request,
    line: u32,
    value: abi::gpiod_line_value,
) -> i32 {
    if !event("write") {
        return -1;
    }
    DRIVER.with(|driver| {
        let mut driver = driver.borrow_mut();
        if line != driver.line {
            return -1;
        }
        driver.writes.push(value);
        0
    })
}

fn descriptor() -> &'static abi::rptadv_gpiod_descriptor_v1 {
    unsafe { &*rptadv_gpiod_adapter_descriptor_v1() }
}

fn endpoint() -> abi::rptadv_gpiod_endpoint_v1 {
    let device = b"/dev/rpt-site-gpio";
    abi::rptadv_gpiod_endpoint_v1 {
        struct_size: size_of::<abi::rptadv_gpiod_endpoint_v1>() as u32,
        abi_version: 1,
        device: device.as_ptr(),
        device_length: device.len(),
        line: 7,
        output: 1,
        active_low: 1,
        startup_active: 1,
        safe_active: 0,
    }
}

#[test]
fn site_gpiod_rejects_invalid_records_before_acquiring_a_line() {
    DRIVER.with(|driver| *driver.borrow_mut() = Driver::default());
    let api = descriptor();
    assert_eq!(api.abi_version, 1);
    assert_eq!(api.capability, *b"rptadv.gpiod\0\0\0\0");
    assert_eq!(
        api.struct_size as usize,
        size_of::<abi::rptadv_gpiod_descriptor_v1>()
    );
    let mut handle: *mut c_void = ptr::null_mut();
    unsafe {
        assert_eq!(api.open.unwrap()(ptr::null(), &mut handle), -1);
        assert_eq!(api.open.unwrap()(&endpoint(), ptr::null_mut()), -1);
        for change in 0..9 {
            let mut config = endpoint();
            match change {
                0 => config.struct_size = 0,
                1 => config.abi_version = 2,
                2 => config.device = ptr::null(),
                3 => config.device_length = 0,
                4 => config.output = 2,
                5 => config.active_low = 2,
                6 => config.startup_active = 2,
                7 => config.safe_active = 2,
                _ => config.device_length = usize::MAX,
            }
            assert_eq!(api.open.unwrap()(&config, &mut handle), -1);
            assert!(handle.is_null());
        }
        assert_eq!(api.read.unwrap()(ptr::null_mut(), ptr::null_mut()), -1);
        assert_eq!(api.write.unwrap()(ptr::null_mut(), ptr::null()), -1);
        assert_eq!(
            api.capabilities.unwrap()(ptr::null_mut(), ptr::null_mut()),
            -1
        );
        api.destroy.unwrap()(ptr::null_mut());
    }
    DRIVER.with(|driver| assert!(driver.borrow().calls.is_empty()));
}

#[test]
fn site_gpiod_missing_device_never_selects_another_chip() {
    DRIVER.with(|driver| *driver.borrow_mut() = Driver::default());
    let mut handle: *mut c_void = ptr::null_mut();
    let device = b"/dev/rpt-advanced-test-absent-gpio";
    let config = abi::rptadv_gpiod_endpoint_v1 {
        device: device.as_ptr(),
        device_length: device.len(),
        ..endpoint()
    };
    assert_eq!(
        unsafe { descriptor().open.unwrap()(&config, &mut handle) },
        1
    );
    assert!(handle.is_null());
    DRIVER.with(|driver| assert_eq!(driver.borrow().path, "/dev/rpt-advanced-test-absent-gpio"));
}

#[test]
fn site_gpiod_stable_line_and_polarity() {
    let api = descriptor();
    for active_low in [0, 1] {
        for output in [0, 1] {
            DRIVER.with(|driver| *driver.borrow_mut() = Driver::default());
            let config = abi::rptadv_gpiod_endpoint_v1 {
                output,
                active_low,
                ..endpoint()
            };
            let mut handle = ptr::null_mut();
            let mut caps = 0;
            let mut observed = abi::rptadv_gpiod_observation_v1 {
                struct_size: size_of::<abi::rptadv_gpiod_observation_v1>() as u32,
                abi_version: 1,
                quality: 0,
                active: 0,
            };
            let mut demand = abi::rptadv_gpiod_output_v1 {
                struct_size: size_of::<abi::rptadv_gpiod_output_v1>() as u32,
                abi_version: 1,
                active: 1,
            };
            unsafe {
                assert_eq!(api.open.unwrap()(&config, &mut handle), 0);
                assert!(!handle.is_null());
                assert_eq!(api.capabilities.unwrap()(handle, &mut caps), 0);
                assert_eq!(caps, if output == 0 { 1 } else { 2 });
                DRIVER.with(|driver| {
                    let driver = driver.borrow();
                    assert_eq!(driver.path, "/dev/rpt-site-gpio");
                    assert_eq!(driver.line, 7);
                    assert_eq!(driver.direction, if output == 0 { 2 } else { 3 });
                    assert_eq!(driver.active_low, active_low == 1);
                    if output != 0 {
                        assert_eq!(driver.initial, 1);
                        assert!(
                            driver.calls.iter().position(|call| *call == "initial")
                                < driver.calls.iter().position(|call| *call == "request")
                        );
                    }
                });
                for value in [0, 1, -1] {
                    DRIVER.with(|driver| driver.borrow_mut().input = value);
                    let result = api.read.unwrap()(handle, &mut observed);
                    if output == 0 && value >= 0 {
                        assert_eq!(
                            (result, observed.quality, observed.active),
                            (0, 1, value as u32)
                        );
                    } else {
                        assert_eq!(
                            (result, observed.quality),
                            if output == 0 { (1, 2) } else { (2, 0) }
                        );
                    }
                }
                assert_eq!(
                    api.write.unwrap()(handle, &demand),
                    if output == 0 { 2 } else { 0 }
                );
                demand.active = 2;
                assert_eq!(api.write.unwrap()(handle, &demand), -1);
                api.destroy.unwrap()(handle);
            }
            DRIVER.with(|driver| {
                let driver = driver.borrow();
                assert_eq!(driver.writes, if output == 0 { vec![] } else { vec![1, 0] });
                assert!(driver.calls.contains(&"request_release"));
                assert!(driver.calls.contains(&"chip_close"));
            });
        }
    }
}

#[test]
fn site_gpiod_failed_acquisition_releases_partial_ownership() {
    for (fail, expected) in [
        ("chip_open", vec![]),
        ("settings_new", vec!["chip_close"]),
        ("direction", vec!["settings_free", "chip_close"]),
        ("initial", vec!["settings_free", "chip_close"]),
        ("config_new", vec!["settings_free", "chip_close"]),
        (
            "add_line",
            vec!["config_free", "settings_free", "chip_close"],
        ),
        (
            "request",
            vec!["config_free", "settings_free", "chip_close"],
        ),
    ] {
        DRIVER.with(|driver| {
            *driver.borrow_mut() = Driver {
                fail,
                ..Driver::default()
            }
        });
        let mut handle = ptr::null_mut();
        assert_eq!(
            unsafe { descriptor().open.unwrap()(&endpoint(), &mut handle) },
            1,
            "{fail}"
        );
        assert!(handle.is_null());
        DRIVER.with(|driver| {
            let released: Vec<_> = driver
                .borrow()
                .calls
                .iter()
                .copied()
                .filter(|call| {
                    call.ends_with("_free")
                        || call.ends_with("_close")
                        || call.ends_with("_release")
                })
                .collect();
            assert_eq!(released, expected, "{fail}");
        });
    }
}

#[test]
fn site_gpiod_rejects_invalid_paths_without_device_access() {
    for device in [b"gpiochip0".as_slice(), b"/dev/x\0y", b"/dev/\xff"] {
        DRIVER.with(|driver| *driver.borrow_mut() = Driver::default());
        let config = abi::rptadv_gpiod_endpoint_v1 {
            device: device.as_ptr(),
            device_length: device.len(),
            ..endpoint()
        };
        let mut handle = ptr::null_mut();
        assert_eq!(
            unsafe { descriptor().open.unwrap()(&config, &mut handle) },
            -1
        );
        DRIVER.with(|driver| assert!(driver.borrow().calls.is_empty()));
    }
}

#[test]
fn site_gpiod_rejects_short_records_and_preserves_failed_write() {
    DRIVER.with(|driver| *driver.borrow_mut() = Driver::default());
    let api = descriptor();
    let mut handle = ptr::null_mut();
    unsafe {
        assert_eq!(api.open.unwrap()(&endpoint(), &mut handle), 0);
        assert_eq!(api.read.unwrap()(handle, ptr::null_mut()), -1);
        assert_eq!(api.write.unwrap()(handle, ptr::null()), -1);
        assert_eq!(api.capabilities.unwrap()(handle, ptr::null_mut()), -1);
        for (size, version) in [(0, 1), (1000, 2)] {
            let mut observed = abi::rptadv_gpiod_observation_v1 {
                struct_size: size,
                abi_version: version,
                quality: 0,
                active: 0,
            };
            let output = abi::rptadv_gpiod_output_v1 {
                struct_size: size,
                abi_version: version,
                active: 0,
            };
            assert_eq!(api.read.unwrap()(handle, &mut observed), -1);
            assert_eq!(api.write.unwrap()(handle, &output), -1);
        }
        let demand = abi::rptadv_gpiod_output_v1 {
            struct_size: size_of::<abi::rptadv_gpiod_output_v1>() as u32,
            abi_version: 1,
            active: 1,
        };
        DRIVER.with(|driver| driver.borrow_mut().fail = "write");
        assert_eq!(api.write.unwrap()(handle, &demand), 1);
        DRIVER.with(|driver| driver.borrow_mut().fail = "");
        assert_eq!(api.write.unwrap()(handle, &demand), 0);
        api.destroy.unwrap()(handle);
    }
    DRIVER.with(|driver| assert_eq!(driver.borrow().writes, [1, 0]));
}
