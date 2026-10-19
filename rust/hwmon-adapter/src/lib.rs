//! Non-audio measurement/PWM capability with exact device ownership.
#![deny(warnings, missing_docs)]

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    missing_docs
)]
mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}
use std::{
    collections::BTreeSet,
    ffi::{CStr, CString, OsStr, c_void},
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::fd::AsRawFd,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    ptr,
    sync::Mutex,
};

/// Only table initialization/retirement is serialized globally, never acquisition.
static USERS: Mutex<usize> = Mutex::new(0);

/// Physical PWM identities, claimed only during non-audio context open/close.
static OUTPUTS: Mutex<BTreeSet<(u64, u64)>> = Mutex::new(BTreeSet::new());

/// Reserve the opened attribute across path aliases before any startup write.
struct OutputClaim((u64, u64));
impl OutputClaim {
    fn acquire(file: &File) -> Result<Self, i32> {
        let metadata = file.metadata().map_err(|_| 1)?;
        let identity = (metadata.dev(), metadata.ino());
        if !OUTPUTS.lock().map_err(|_| 1)?.insert(identity) {
            return Err(1);
        }
        Ok(Self(identity))
    }
}
impl Drop for OutputClaim {
    fn drop(&mut self) {
        OUTPUTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Keep immutable libsensors tables alive across every device owner's reads.
struct Lease;
impl Lease {
    fn acquire() -> Result<Self, i32> {
        let mut users = USERS.lock().map_err(|_| 1)?;
        if *users == 0 {
            // SAFETY: immutable C strings and a synchronous library initialization.
            // A non-null empty stream disables host compute/ignore/set directives.
            let input = unsafe { abi::fopen(c"/dev/null".as_ptr(), c"r".as_ptr()) };
            if input.is_null() {
                return Err(1);
            }
            let status = unsafe { abi::sensors_init(input) };
            unsafe { abi::fclose(input) };
            if status != 0 {
                unsafe { abi::sensors_cleanup() };
                return Err(1);
            }
        }
        *users += 1;
        Ok(Self)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut users = USERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *users -= 1;
        if *users == 0 {
            // SAFETY: no endpoint can still read the tables after the last lease.
            unsafe { abi::sensors_cleanup() };
        }
    }
}

/// Copy only public chip metadata; the lease retains its borrowed prefix.
/// The directory FD prevents hwmon index reuse from rebinding this measurement.
struct Measurement {
    chip: abi::sensors_chip_name,
    number: i32,
    _path: CString,
    _directory: File,
    _lease: Lease,
}
impl Measurement {
    fn open(directory: File, channel: &str) -> Result<Self, i32> {
        let lease = Lease::acquire()?;
        let identity = directory.metadata().map_err(|_| 1)?;
        let mut chip_index = 0;
        loop {
            // SAFETY: the lease retains all library tables; iteration is read-only.
            let chip = unsafe { abi::sensors_get_detected_chips(ptr::null(), &mut chip_index) };
            if chip.is_null() {
                return Err(1);
            }
            let chip = unsafe { &*chip };
            let original_path = unsafe { CStr::from_ptr(chip.path) }.to_bytes();
            let same_device =
                std::fs::metadata(OsStr::from_bytes(original_path)).is_ok_and(|candidate| {
                    (candidate.dev(), candidate.ino()) == (identity.dev(), identity.ino())
                });
            if !same_device {
                continue;
            }
            let number = measurement_number(chip, channel)?;
            let path =
                CString::new(format!("/proc/self/fd/{}", directory.as_raw_fd())).map_err(|_| -1)?;
            let mut selected = *chip;
            selected.path = path.as_ptr().cast_mut();
            return Ok(Self {
                chip: selected,
                number,
                _path: path,
                _directory: directory,
                _lease: lease,
            });
        }
    }

    fn read(&self) -> Result<f64, i32> {
        let mut value = 0.0;
        // SAFETY: pinned metadata and library lease live through the serialized read.
        let result = unsafe { abi::sensors_get_value(&self.chip, self.number, &mut value) };
        if result == 0 && value.is_finite() {
            Ok(value)
        } else {
            Err(1)
        }
    }
}

/// Select an actual numeric measurement, not a writable threshold or alarm flag.
fn measurement_number(chip: &abi::sensors_chip_name, channel: &str) -> Result<i32, i32> {
    let mut feature_index = 0;
    loop {
        // SAFETY: chip and all table entries remain live under the caller's lease.
        let feature = unsafe { abi::sensors_get_features(chip, &mut feature_index) };
        if feature.is_null() {
            return Err(2);
        }
        let mut index = 0;
        loop {
            let subfeature = unsafe { abi::sensors_get_all_subfeatures(chip, feature, &mut index) };
            if subfeature.is_null() {
                break;
            }
            let subfeature = unsafe { &*subfeature };
            if unsafe { CStr::from_ptr(subfeature.name) }.to_bytes() != channel.as_bytes() {
                continue;
            }
            let supported = matches!(
                subfeature.type_,
                abi::sensors_subfeature_type_SENSORS_SUBFEATURE_IN_INPUT
                    | abi::sensors_subfeature_type_SENSORS_SUBFEATURE_CURR_INPUT
                    | abi::sensors_subfeature_type_SENSORS_SUBFEATURE_TEMP_INPUT
                    | abi::sensors_subfeature_type_SENSORS_SUBFEATURE_POWER_INPUT
                    | abi::sensors_subfeature_type_SENSORS_SUBFEATURE_POWER_AVERAGE
                    | abi::sensors_subfeature_type_SENSORS_SUBFEATURE_ENERGY_INPUT
                    | abi::sensors_subfeature_type_SENSORS_SUBFEATURE_FAN_INPUT
            );
            return if supported && subfeature.flags & abi::SENSORS_MODE_R != 0 {
                Ok(subfeature.number)
            } else {
                Err(2)
            };
        }
    }
}

/// Retain kernel PWM attributes; never change control mode or synthesize pulses.
struct Pwm {
    duty: File,
    mode: File,
    safe: f64,
    // Released after final safe submission and file closure, including failed opens.
    _claim: OutputClaim,
}
impl Pwm {
    fn open(directory: &File, channel: &str, startup: f64, safe: f64) -> Result<Self, i32> {
        if !channel
            .strip_prefix("pwm")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(2);
        }
        let pinned = format!("/proc/self/fd/{}", directory.as_raw_fd());
        let path = Path::new(&pinned);
        let unsupported = |error: std::io::Error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                2
            } else {
                1
            }
        };
        let mode = File::open(path.join(format!("{channel}_enable"))).map_err(unsupported)?;
        let duty = OpenOptions::new()
            .write(true)
            .open(path.join(channel))
            .map_err(unsupported)?;
        let claim = OutputClaim::acquire(&duty)?;
        let mut pwm = Self {
            duty,
            mode,
            safe,
            _claim: claim,
        };
        pwm.write(startup)?;
        Ok(pwm)
    }

    fn write(&mut self, percent: f64) -> Result<(), i32> {
        self.mode.rewind().map_err(|_| 1)?;
        let mut mode = String::new();
        Read::by_ref(&mut self.mode)
            .take(129)
            .read_to_string(&mut mode)
            .map_err(|_| 1)?;
        if mode.trim() != "1" {
            return Err(2);
        }
        let text = format!("{}\n", (percent * 255.0 / 100.0).round() as u16);
        self.duty.rewind().map_err(|_| 1)?;
        // One sysfs write is one complete control request, including its newline.
        if self.duty.write(text.as_bytes()).map_err(|_| 1)? == text.len() {
            Ok(())
        } else {
            Err(1)
        }
    }
}

/// Exactly one capability selected at open; no fallback or second device owner.
enum Context {
    Measurement(Measurement),
    Pwm(Pwm),
}
impl Context {
    fn open(endpoint: &abi::rptadv_hwmon_endpoint_v1) -> Result<Self, i32> {
        if endpoint.output > 1
            || !valid_percent(endpoint.startup_percent)
            || !valid_percent(endpoint.safe_percent)
        {
            return Err(-1);
        }
        // SAFETY: the caller lends both byte extents for this open operation.
        let device = unsafe { text(endpoint.device, endpoint.device_length)? };
        let channel = unsafe { text(endpoint.channel, endpoint.channel_length)? };
        if !Path::new(device).is_absolute()
            || !channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(-1);
        }
        let canonical = Path::new(device).canonicalize().map_err(|_| 1)?;
        let directory = File::open(&canonical).map_err(|_| 1)?;
        if !directory.metadata().map_err(|_| 1)?.is_dir() {
            return Err(-1);
        }
        if endpoint.output == 1 {
            Pwm::open(
                &directory,
                channel,
                endpoint.startup_percent,
                endpoint.safe_percent,
            )
            .map(Self::Pwm)
        } else {
            Measurement::open(directory, channel).map(Self::Measurement)
        }
    }
}

fn valid_percent(value: f64) -> bool {
    value.is_finite() && (0.0..=100.0).contains(&value)
}

/// Validate borrowed UTF-8 before making paths or passing data to external code.
unsafe fn text<'a>(pointer: *const u8, length: usize) -> Result<&'a str, i32> {
    if pointer.is_null() || length == 0 || length > isize::MAX as usize {
        return Err(-1);
    }
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
    if bytes.contains(&0) {
        return Err(-1);
    }
    std::str::from_utf8(bytes).map_err(|_| -1)
}

/// The caller lends an aligned size word and its declared live byte extent.
unsafe fn record<T>(pointer: *const T) -> Result<(), i32> {
    if pointer.is_null()
        || unsafe { pointer.cast::<u32>().read() } < size_of::<T>() as u32
        || unsafe { pointer.cast::<u32>().add(1).read() } != 1
    {
        Err(-1)
    } else {
        Ok(())
    }
}
fn boundary(operation: impl FnOnce() -> Result<(), i32>) -> i32 {
    catch_unwind(AssertUnwindSafe(operation))
        .unwrap_or(Err(-1))
        .err()
        .unwrap_or(0)
}
unsafe extern "C" fn open(
    endpoint: *const abi::rptadv_hwmon_endpoint_v1,
    context: *mut *mut c_void,
) -> i32 {
    boundary(|| {
        // SAFETY: caller lends writable output and a complete validated record.
        let context = unsafe { context.as_mut() }.ok_or(-1)?;
        *context = ptr::null_mut();
        unsafe { record(endpoint)? };
        let owned = Context::open(unsafe { &*endpoint })?;
        *context = Box::into_raw(Box::new(owned)).cast();
        Ok(())
    })
}
unsafe extern "C" fn read(
    context: *mut c_void,
    observation: *mut abi::rptadv_hwmon_observation_v1,
) -> i32 {
    boundary(|| {
        // SAFETY: caller serializes the live context and writable observation.
        let context = unsafe { context.cast::<Context>().as_ref() }.ok_or(-1)?;
        unsafe { record(observation)? };
        let observation = unsafe { &mut *observation };
        observation.quality = 0;
        let Context::Measurement(measurement) = context else {
            return Err(2);
        };
        observation.quality = 2;
        observation.value = measurement.read()?;
        observation.quality = 1;
        Ok(())
    })
}
unsafe extern "C" fn write(
    context: *mut c_void,
    output: *const abi::rptadv_hwmon_output_v1,
) -> i32 {
    boundary(|| {
        // SAFETY: caller serializes the live context and lends the input record.
        let context = unsafe { context.cast::<Context>().as_mut() }.ok_or(-1)?;
        unsafe { record(output)? };
        let percent = unsafe { (*output).percent };
        if !valid_percent(percent) {
            return Err(-1);
        }
        let Context::Pwm(pwm) = context else {
            return Err(2);
        };
        pwm.write(percent)
    })
}
unsafe extern "C" fn capabilities(context: *mut c_void, capabilities: *mut u32) -> i32 {
    boundary(|| {
        // SAFETY: caller keeps the context live and output writable.
        let context = unsafe { context.cast::<Context>().as_ref() }.ok_or(-1)?;
        let capabilities = unsafe { capabilities.as_mut() }.ok_or(-1)?;
        *capabilities = match context {
            Context::Measurement(_) => abi::RPTADV_HWMON_INPUT,
            Context::Pwm(_) => abi::RPTADV_HWMON_PWM,
        };
        Ok(())
    })
}
unsafe extern "C" fn destroy(context: *mut c_void) {
    boundary(|| {
        if !context.is_null() {
            // SAFETY: called once after this context's operations are quiescent.
            let mut context = unsafe { Box::from_raw(context.cast::<Context>()) };
            if let Context::Pwm(pwm) = context.as_mut() {
                let _ = pwm.write(pwm.safe);
            }
        }
        Ok(())
    });
}
static DESCRIPTOR: abi::rptadv_hwmon_descriptor_v1 = abi::rptadv_hwmon_descriptor_v1 {
    struct_size: size_of::<abi::rptadv_hwmon_descriptor_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.hwmon\0\0\0\0",
    open: Some(open),
    read: Some(read),
    write: Some(write),
    capabilities: Some(capabilities),
    destroy: Some(destroy),
};
/// Return the immutable sensor descriptor, retained until the DSO unloads.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_hwmon_adapter_descriptor_v1() -> *const abi::rptadv_hwmon_descriptor_v1 {
    &DESCRIPTOR
}
#[cfg(test)]
mod tests;
