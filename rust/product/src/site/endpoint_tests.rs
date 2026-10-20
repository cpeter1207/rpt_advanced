//! Boundary doubles expose ABI arguments and failures without claiming physical qualification.
use super::super::Provider;
use super::*;
use rpt_advanced_core::config::SiteBackend;
use std::{ffi::c_void, mem::size_of, ptr, sync::Mutex};

#[derive(Default)]
struct Probe {
    opened: Vec<(u32, u32, f64, f64)>,
    written: Vec<f64>,
    closed: usize,
    closed_on_control: bool,
    failure: bool,
    invalid: bool,
}
static SERIAL: Mutex<()> = Mutex::new(());
static PROBE: Mutex<Probe> = Mutex::new(Probe {
    opened: Vec::new(),
    written: Vec::new(),
    closed: 0,
    closed_on_control: false,
    failure: false,
    invalid: false,
});
#[path = "acquisition_tests.rs"]
mod acquisition;
#[path = "worker_tests.rs"]
mod workers;
unsafe extern "C" fn gpio_open(
    e: *const abi::rptadv_gpiod_endpoint_v1,
    c: *mut *mut c_void,
) -> i32 {
    let e = unsafe { &*e };
    if e.struct_size as usize != size_of::<abi::rptadv_gpiod_endpoint_v1>()
        || e.abi_version != 1
        || !matches!(e.line, 7 | 8)
    {
        return -1;
    }
    PROBE.lock().unwrap().opened.push((
        e.output,
        e.active_low,
        f64::from(e.startup_active),
        f64::from(e.safe_active),
    ));
    unsafe { *c = ptr::dangling_mut::<u8>().cast() };
    0
}
unsafe extern "C" fn hwmon_open(
    e: *const abi::rptadv_hwmon_endpoint_v1,
    c: *mut *mut c_void,
) -> i32 {
    let e = unsafe { &*e };
    if e.struct_size as usize != size_of::<abi::rptadv_hwmon_endpoint_v1>() || e.abi_version != 1 {
        return -1;
    }
    PROBE
        .lock()
        .unwrap()
        .opened
        .push((e.output, 0, e.startup_percent, e.safe_percent));
    unsafe { *c = ptr::dangling_mut::<u8>().cast() };
    0
}
unsafe extern "C" fn gpio_read(_: *mut c_void, o: *mut abi::rptadv_gpiod_observation_v1) -> i32 {
    workers::GPIO_ENTERED.store(true, std::sync::atomic::Ordering::Release);
    while workers::BLOCK_GPIO.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let o = unsafe { &mut *o };
    let p = PROBE.lock().unwrap();
    if p.failure {
        return 1;
    }
    o.quality = 1;
    o.active = if p.invalid { 2 } else { 1 };
    0
}
unsafe extern "C" fn hwmon_read(_: *mut c_void, o: *mut abi::rptadv_hwmon_observation_v1) -> i32 {
    let o = unsafe { &mut *o };
    let p = PROBE.lock().unwrap();
    if p.failure {
        return 1;
    }
    o.quality = 1;
    o.value = if p.invalid { f64::NAN } else { 12.5 };
    0
}
unsafe extern "C" fn gpio_write(_: *mut c_void, o: *const abi::rptadv_gpiod_output_v1) -> i32 {
    let o = unsafe { &*o };
    if o.struct_size as usize != size_of::<abi::rptadv_gpiod_output_v1>()
        || o.abi_version != 1
        || o.active > 1
    {
        return -1;
    }
    let mut p = PROBE.lock().unwrap();
    if p.failure {
        return 1;
    }
    p.written.push(f64::from(o.active));
    0
}
unsafe extern "C" fn hwmon_write(_: *mut c_void, o: *const abi::rptadv_hwmon_output_v1) -> i32 {
    let o = unsafe { &*o };
    if o.struct_size as usize != size_of::<abi::rptadv_hwmon_output_v1>() || o.abi_version != 1 {
        return -1;
    }
    let mut p = PROBE.lock().unwrap();
    if p.failure {
        return 1;
    }
    p.written.push(o.percent);
    0
}
unsafe extern "C" fn capabilities(_: *mut c_void, c: *mut u32) -> i32 {
    let p = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        *c = if p.invalid {
            0
        } else if p.opened.last().unwrap().0 == 1 {
            2
        } else {
            1
        }
    };
    0
}
unsafe extern "C" fn destroy(_: *mut c_void) {
    let mut probe = PROBE.lock().unwrap();
    probe.closed += 1;
    probe.closed_on_control |= acquisition::IN_CONTROL.load(std::sync::atomic::Ordering::Acquire);
}
fn backends() -> Arc<SiteBackends> {
    Arc::new(SiteBackends {
        gpiod: Some(Provider {
            table: abi::rptadv_gpiod_descriptor_v1 {
                struct_size: size_of::<abi::rptadv_gpiod_descriptor_v1>() as u32,
                abi_version: 1,
                capability: *b"rptadv.gpiod\0\0\0\0",
                open: Some(gpio_open),
                read: Some(gpio_read),
                write: Some(gpio_write),
                capabilities: Some(capabilities),
                destroy: Some(destroy),
            },
            _library: libloading::os::unix::Library::this().into(),
        }),
        hwmon: Some(Provider {
            table: abi::rptadv_hwmon_descriptor_v1 {
                struct_size: size_of::<abi::rptadv_hwmon_descriptor_v1>() as u32,
                abi_version: 1,
                capability: *b"rptadv.hwmon\0\0\0\0",
                open: Some(hwmon_open),
                read: Some(hwmon_read),
                write: Some(hwmon_write),
                capabilities: Some(capabilities),
                destroy: Some(destroy),
            },
            _library: libloading::os::unix::Library::this().into(),
        }),
    })
}
fn settings(backend: SiteBackend) -> SiteEndpoint {
    SiteEndpoint {
        backend,
        device: "/selected-device".into(),
        channel: "7".into(),
    }
}

#[test]
fn site_endpoint_reads_only_valid_source_values_and_normalizes_no_second_time() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (backend, expected) in [
        (SiteBackend::Gpiod, Sample::Digital(true)),
        (SiteBackend::Hwmon, Sample::Number(12.5)),
    ] {
        *PROBE.lock().unwrap() = Probe::default();
        let mut endpoint = Endpoint::open(
            backends(),
            &settings(backend),
            Direction::Input { active_high: false },
        )
        .unwrap();
        assert_eq!(endpoint.read(), Ok(expected));
        PROBE.lock().unwrap().invalid = true;
        assert_eq!(endpoint.read(), Err(ReadingQuality::Fault));
        PROBE.lock().unwrap().failure = true;
        assert_eq!(endpoint.read(), Err(ReadingQuality::Fault));
        assert_eq!(endpoint.write(1.0), Err(ReadingQuality::Unavailable));
        drop(endpoint);
        assert_eq!(PROBE.lock().unwrap().closed, 1);
    }
}

#[test]
fn site_endpoint_output_units_and_safe_failure_preserve_ownership() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for backend in [SiteBackend::Gpiod, SiteBackend::Hwmon] {
        *PROBE.lock().unwrap() = Probe::default();
        let mut endpoint = Endpoint::open(
            backends(),
            &settings(backend),
            Direction::Output {
                active_high: false,
                startup_on: true,
                safe_on: false,
            },
        )
        .unwrap();
        let pwm = backend == SiteBackend::Hwmon;
        assert_eq!(endpoint.pwm(), pwm);
        let opened = PROBE.lock().unwrap().opened.clone();
        assert_eq!(
            opened,
            if pwm {
                vec![(1, 0, 0.0, 100.0)]
            } else {
                vec![(1, 1, 1.0, 0.0)]
            }
        );
        assert_eq!(endpoint.read(), Err(ReadingQuality::Unavailable));
        for invalid in [f64::NAN, -0.1, 1.1] {
            assert_eq!(endpoint.write(invalid), Err(ReadingQuality::Fault));
        }
        assert_eq!(
            endpoint.write(0.25),
            if pwm {
                Ok(())
            } else {
                Err(ReadingQuality::Unavailable)
            }
        );
        endpoint.write(1.0).unwrap();
        PROBE.lock().unwrap().failure = true;
        assert_eq!(endpoint.safe(), Err(ReadingQuality::Fault));
        assert_eq!(PROBE.lock().unwrap().closed, 0);
        PROBE.lock().unwrap().failure = false;
        endpoint.safe().unwrap();
        let written = PROBE.lock().unwrap().written.clone();
        assert_eq!(
            written,
            if pwm {
                vec![75.0, 0.0, 100.0]
            } else {
                vec![1.0, 0.0]
            }
        );
        drop(endpoint);
        assert_eq!(PROBE.lock().unwrap().closed, 1);
    }
}

#[test]
fn site_endpoint_requires_the_selected_capability_without_fallback() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let direction = Direction::Input { active_high: true };
    let missing = Arc::new(SiteBackends {
        gpiod: None,
        hwmon: None,
    });
    for backend in [
        SiteBackend::Gpiod,
        SiteBackend::Hwmon,
        SiteBackend::Cm119,
        SiteBackend::Parallel,
    ] {
        assert!(matches!(
            Endpoint::open(Arc::clone(&missing), &settings(backend), direction),
            Err(ReadingQuality::Unavailable)
        ));
    }
    for backend in [SiteBackend::Gpiod, SiteBackend::Hwmon] {
        *PROBE.lock().unwrap() = Probe {
            invalid: true,
            ..Probe::default()
        };
        assert!(matches!(
            Endpoint::open(backends(), &settings(backend), direction),
            Err(ReadingQuality::Unavailable)
        ));
        assert_eq!(PROBE.lock().unwrap().closed, 1);
    }
}
