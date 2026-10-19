//! Sensor-library boundary doubles with real filesystem identity/lifetime checks.
use super::*;
use std::{
    ffi::{CStr, CString},
    fs,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
static CHIPS: Mutex<Vec<Chip>> = Mutex::new(Vec::new());
static INITS: AtomicUsize = AtomicUsize::new(0);
static CLEANUPS: AtomicUsize = AtomicUsize::new(0);
static FAIL_INIT: AtomicBool = AtomicBool::new(false);
static BLOCK: (Mutex<(bool, bool)>, Condvar) = (Mutex::new((false, false)), Condvar::new());
const CHANNELS: [&str; 6] = [
    "temp1_input",
    "in0_input",
    "curr1_input",
    "power1_input",
    "energy1_input",
    "fan1_input",
];
const TYPES: [u32; 6] = [0x200, 0, 0x500, 0x303, 0x400, 0x100];

struct Chip {
    api: abi::sensors_chip_name,
    features: Vec<abi::sensors_feature>,
    subfeatures: Vec<abi::sensors_subfeature>,
    _path: CString,
    _names: Vec<CString>,
}
// SAFETY: fixture metadata is immutable while handles exist. The test lock
// serializes replacement; pointers refer only to owned CString heap buffers.
unsafe impl Send for Chip {}

impl Chip {
    fn new(path: &Path) -> Self {
        let path = CString::new(path.to_str().unwrap()).unwrap();
        let names: Vec<_> = CHANNELS
            .iter()
            .map(|name| CString::new(*name).unwrap())
            .collect();
        let features = names
            .iter()
            .enumerate()
            .map(|(index, name)| abi::sensors_feature {
                name: name.as_ptr().cast_mut(),
                number: index as i32,
                type_: TYPES[index] >> 8,
                first_subfeature: index as i32,
                padding1: 0,
            })
            .collect();
        let subfeatures = names
            .iter()
            .enumerate()
            .map(|(index, name)| abi::sensors_subfeature {
                name: name.as_ptr().cast_mut(),
                number: index as i32,
                type_: TYPES[index],
                mapping: index as i32,
                flags: 1,
            })
            .collect();
        Self {
            api: abi::sensors_chip_name {
                prefix: c"fixture".as_ptr().cast_mut(),
                bus: abi::sensors_bus_id { type_: 4, nr: 0 },
                addr: 0,
                path: path.as_ptr().cast_mut(),
            },
            features,
            subfeatures,
            _path: path,
            _names: names,
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn sensors_init(input: *mut abi::SensorFile) -> i32 {
    INITS.fetch_add(1, Ordering::SeqCst);
    if input.is_null() || FAIL_INIT.load(Ordering::SeqCst) {
        -1
    } else {
        0
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn sensors_cleanup() {
    CLEANUPS.fetch_add(1, Ordering::SeqCst);
}
#[unsafe(no_mangle)]
unsafe extern "C" fn sensors_get_detected_chips(
    _: *const abi::sensors_chip_name,
    index: *mut i32,
) -> *const abi::sensors_chip_name {
    let index = unsafe { &mut *index };
    let chips = CHIPS.lock().unwrap();
    let result = chips
        .get(*index as usize)
        .map_or(ptr::null(), |chip| &chip.api);
    *index += 1;
    result
}
#[unsafe(no_mangle)]
unsafe extern "C" fn sensors_get_features(
    _: *const abi::sensors_chip_name,
    index: *mut i32,
) -> *const abi::sensors_feature {
    let index = unsafe { &mut *index };
    let chips = CHIPS.lock().unwrap();
    let result = chips[0]
        .features
        .get(*index as usize)
        .map_or(ptr::null(), |feature| feature);
    *index += 1;
    result
}
#[unsafe(no_mangle)]
unsafe extern "C" fn sensors_get_all_subfeatures(
    _: *const abi::sensors_chip_name,
    feature: *const abi::sensors_feature,
    index: *mut i32,
) -> *const abi::sensors_subfeature {
    let index = unsafe { &mut *index };
    if *index != 0 {
        return ptr::null();
    }
    *index += 1;
    let number = unsafe { (*feature).number };
    &CHIPS.lock().unwrap()[0].subfeatures[number as usize]
}
#[unsafe(no_mangle)]
unsafe extern "C" fn sensors_get_value(
    chip: *const abi::sensors_chip_name,
    number: i32,
    output: *mut f64,
) -> i32 {
    let path = Path::new(unsafe { CStr::from_ptr((*chip).path) }.to_str().unwrap());
    if path.join("block").exists() {
        let (lock, wake) = &BLOCK;
        let mut state = lock.lock().unwrap();
        state.0 = true;
        wake.notify_all();
        while !state.1 {
            state = wake.wait(state).unwrap();
        }
    }
    let value = fs::read_to_string(path.join(CHANNELS[number as usize]))
        .ok()
        .and_then(|text| text.trim().parse().ok());
    match value {
        Some(value) => {
            unsafe { *output = value };
            0
        }
        None => -1,
    }
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "rptadv-hwmon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        *CHIPS.lock().unwrap() = vec![Chip::new(&root.join("a")), Chip::new(&root.join("b"))];
        INITS.store(0, Ordering::SeqCst);
        CLEANUPS.store(0, Ordering::SeqCst);
        FAIL_INIT.store(false, Ordering::SeqCst);
        Self(root)
    }
    fn put(&self, name: &str, value: &str) {
        fs::write(self.0.join(name), value).unwrap();
    }
    fn open(
        &self,
        device: &str,
        channel: &str,
        output: u32,
        start: f64,
        safe: f64,
    ) -> (i32, Opened) {
        let path = self.0.join(device);
        let path = path.to_str().unwrap();
        let endpoint = abi::rptadv_hwmon_endpoint_v1 {
            struct_size: size_of::<abi::rptadv_hwmon_endpoint_v1>() as u32,
            abi_version: 1,
            device: path.as_ptr(),
            device_length: path.len(),
            channel: channel.as_ptr(),
            channel_length: channel.len(),
            output,
            startup_percent: start,
            safe_percent: safe,
        };
        let mut handle = ptr::null_mut();
        let result = unsafe { (DESCRIPTOR.open.unwrap())(&endpoint, &mut handle) };
        (result, Opened(handle))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Opened(*mut c_void);
impl Drop for Opened {
    fn drop(&mut self) {
        unsafe { (DESCRIPTOR.destroy.unwrap())(self.0) };
    }
}
fn observed(handle: *mut c_void) -> (i32, u32, f64) {
    let mut value = abi::rptadv_hwmon_observation_v1 {
        struct_size: size_of::<abi::rptadv_hwmon_observation_v1>() as u32,
        abi_version: 1,
        quality: 0,
        value: 123.0,
    };
    let result = unsafe { (DESCRIPTOR.read.unwrap())(handle, &mut value) };
    (result, value.quality, value.value)
}
fn demand(handle: *mut c_void, percent: f64) -> i32 {
    let output = abi::rptadv_hwmon_output_v1 {
        struct_size: size_of::<abi::rptadv_hwmon_output_v1>() as u32,
        abi_version: 1,
        percent,
    };
    unsafe { (DESCRIPTOR.write.unwrap())(handle, &output) }
}

#[test]
fn site_hwmon_source_units_and_shared_library_lifetime() {
    let _serial = serial();
    let fixture = Fixture::new();
    let mut handles = Vec::new();
    for (channel, expected) in CHANNELS
        .into_iter()
        .zip([25.0, 13.8, 1.5, 20.0, 1000.0, 2400.0])
    {
        fixture.put(&format!("a/{channel}"), &expected.to_string());
        let (result, handle) = fixture.open("a", channel, 0, 0.0, 0.0);
        assert_eq!(result, 0);
        assert_eq!(observed(handle.0), (0, 1, expected));
        assert_eq!(demand(handle.0, 50.0), 2);
        handles.push(handle);
    }
    assert_eq!(INITS.load(Ordering::SeqCst), 1);
    handles.pop();
    assert_eq!(CLEANUPS.load(Ordering::SeqCst), 0);
    drop(handles);
    assert_eq!(CLEANUPS.load(Ordering::SeqCst), 1);
}

#[test]
fn site_hwmon_library_class_symlink_resolves_to_the_pinned_device() {
    let _serial = serial();
    let fixture = Fixture::new();
    fixture.put("a/temp1_input", "25");
    let alias = fixture.0.join("class-hwmon0");
    std::os::unix::fs::symlink(fixture.0.join("a"), &alias).unwrap();
    *CHIPS.lock().unwrap() = vec![Chip::new(&fixture.0.join("b")), Chip::new(&alias)];
    let (result, handle) = fixture.open("a", "temp1_input", 0, 0.0, 0.0);
    assert_eq!(result, 0);
    assert_eq!(observed(handle.0), (0, 1, 25.0));
    drop(handle);
}

#[test]
fn site_hwmon_directory_reuse_never_switches_an_open_measurement() {
    let _serial = serial();
    let fixture = Fixture::new();
    fixture.put("a/temp1_input", "25");
    let (result, handle) = fixture.open("a", "temp1_input", 0, 0.0, 0.0);
    assert_eq!(result, 0);
    fs::rename(fixture.0.join("a"), fixture.0.join("old")).unwrap();
    fs::create_dir(fixture.0.join("a")).unwrap();
    fixture.put("a/temp1_input", "99");
    assert_eq!(observed(handle.0), (0, 1, 25.0));
    for value in ["NaN", "inf", "invalid"] {
        fixture.put("old/temp1_input", value);
        assert_eq!(observed(handle.0), (1, 2, 123.0));
    }
    fs::remove_file(fixture.0.join("old/temp1_input")).unwrap();
    assert_eq!(observed(handle.0), (1, 2, 123.0));
    drop(handle);
}

#[test]
fn site_hwmon_rejects_unknown_channels_and_failed_initialization() {
    let _serial = serial();
    let fixture = Fixture::new();
    for (device, channel, expected) in [
        ("absent", "temp1_input", 1),
        ("a", "missing", 2),
        ("a", "../temp1_input", -1),
    ] {
        let (result, handle) = fixture.open(device, channel, 0, 0.0, 0.0);
        assert_eq!(result, expected);
        assert!(handle.0.is_null());
    }
    FAIL_INIT.store(true, Ordering::SeqCst);
    let (result, handle) = fixture.open("a", "temp1_input", 0, 0.0, 0.0);
    assert_eq!(result, 1);
    assert!(handle.0.is_null());
    FAIL_INIT.store(false, Ordering::SeqCst);
    fixture.put("a/temp1_input", "12");
    let (result, handle) = fixture.open("a", "temp1_input", 0, 0.0, 0.0);
    assert_eq!(result, 0);
    assert_eq!(observed(handle.0), (0, 1, 12.0));
    drop(handle);
    assert_eq!(
        INITS.load(Ordering::SeqCst),
        CLEANUPS.load(Ordering::SeqCst)
    );
}

#[test]
fn site_pwm_requires_real_manual_capability_and_preserves_safe_duty() {
    let _serial = serial();
    let fixture = Fixture::new();
    fixture.put("a/pwm1", "13\n");
    fixture.put("a/pwm1_enable", "2\n");
    assert_eq!(fixture.open("a", "pwm1", 1, 25.0, 100.0).0, 2);
    assert_eq!(
        fs::read_to_string(fixture.0.join("a/pwm1")).unwrap(),
        "13\n"
    );
    fixture.put("a/pwm1_enable", "1\n");
    let (result, handle) = fixture.open("a", "pwm1", 1, 25.0, 100.0);
    assert_eq!(result, 0);
    assert!(
        fs::read_to_string(fixture.0.join("a/pwm1"))
            .unwrap()
            .starts_with("64\n")
    );
    assert_eq!(observed(handle.0), (2, 0, 123.0));
    for value in [f64::NAN, -1.0, 101.0, f64::INFINITY] {
        assert_eq!(demand(handle.0, value), -1);
    }
    assert_eq!(demand(handle.0, 50.0), 0);
    assert!(
        fs::read_to_string(fixture.0.join("a/pwm1"))
            .unwrap()
            .starts_with("128\n")
    );
    fixture.put("a/pwm1_enable", "2\n");
    assert_eq!(demand(handle.0, 0.0), 2);
    fixture.put("a/pwm1_enable", "1\n");
    drop(handle);
    assert!(
        fs::read_to_string(fixture.0.join("a/pwm1"))
            .unwrap()
            .starts_with("255\n")
    );
    assert_eq!(INITS.load(Ordering::SeqCst), 0);
}

#[test]
fn site_hwmon_slow_device_does_not_serialize_another_device() {
    let _serial = serial();
    let fixture = Fixture::new();
    fixture.put("a/temp1_input", "10");
    fixture.put("b/temp1_input", "20");
    fixture.put("a/block", "");
    *BLOCK.0.lock().unwrap() = (false, false);
    let (result, first) = fixture.open("a", "temp1_input", 0, 0.0, 0.0);
    assert_eq!(result, 0);
    let (result, second) = fixture.open("b", "temp1_input", 0, 0.0, 0.0);
    assert_eq!(result, 0);
    let first_address = first.0 as usize;
    let reader = std::thread::spawn(move || observed(first_address as *mut c_void));
    let (state, timeout) = BLOCK
        .1
        .wait_timeout_while(BLOCK.0.lock().unwrap(), Duration::from_secs(2), |state| {
            !state.0
        })
        .unwrap();
    let entered = state.0 && !timeout.timed_out();
    drop(state);
    let second_address = second.0 as usize;
    let (sent, received) = mpsc::channel();
    let other = std::thread::spawn(move || {
        sent.send(observed(second_address as *mut c_void)).unwrap();
    });
    let result = received.recv_timeout(Duration::from_secs(2));
    BLOCK.0.lock().unwrap().1 = true;
    BLOCK.1.notify_all();
    let first_result = reader.join().unwrap();
    other.join().unwrap();
    assert!(entered);
    assert_eq!(result.unwrap(), (0, 1, 20.0));
    assert_eq!(first_result, (0, 1, 10.0));
    drop((first, second));
}
