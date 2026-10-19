//! Linux GPIO capability. Physical line operations run only on serialized device owners.
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
    ffi::{CString, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
};

/// One libgpiod allocation, released on every unsuccessful acquisition path.
struct Handle<T> {
    pointer: NonNull<T>,
    release: unsafe extern "C" fn(*mut T),
}

impl<T> Handle<T> {
    /// Adopt a newly allocated external object and its matching destructor.
    fn new(pointer: *mut T, release: unsafe extern "C" fn(*mut T)) -> Result<Self, i32> {
        Ok(Self {
            pointer: NonNull::new(pointer).ok_or(1)?,
            release,
        })
    }

    fn pointer(&self) -> *mut T {
        self.pointer.as_ptr()
    }
}

impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        // SAFETY: each handle uniquely owns the matching libgpiod allocation.
        unsafe { (self.release)(self.pointer()) };
    }
}

/// The request owns the line FD independently of the temporary chip/configuration.
struct Line {
    request: Handle<abi::gpiod_line_request>,
    offset: u32,
    output: bool,
    safe_active: i32,
}

impl Line {
    /// Validate the endpoint before touching the device; acquire without a startup glitch.
    fn acquire(endpoint: &abi::rptadv_gpiod_endpoint_v1) -> Result<Self, i32> {
        if [
            endpoint.output,
            endpoint.active_low,
            endpoint.startup_active,
            endpoint.safe_active,
        ]
        .iter()
        .any(|value| *value > 1)
            || endpoint.device.is_null()
            || endpoint.device_length == 0
            || endpoint.device_length > isize::MAX as usize
        {
            return Err(-1);
        }
        // SAFETY: the caller lends a readable extent for this synchronous call.
        let bytes = unsafe { std::slice::from_raw_parts(endpoint.device, endpoint.device_length) };
        let path = std::str::from_utf8(bytes).map_err(|_| -1)?;
        if !std::path::Path::new(path).is_absolute() {
            return Err(-1);
        }
        let path = CString::new(path).map_err(|_| -1)?;
        // SAFETY: all external objects remain alive through request acquisition.
        unsafe {
            let chip = Handle::new(abi::gpiod_chip_open(path.as_ptr()), abi::gpiod_chip_close)?;
            let settings = Handle::new(
                abi::gpiod_line_settings_new(),
                abi::gpiod_line_settings_free,
            )?;
            let direction = if endpoint.output == 0 {
                abi::gpiod_line_direction_GPIOD_LINE_DIRECTION_INPUT
            } else {
                abi::gpiod_line_direction_GPIOD_LINE_DIRECTION_OUTPUT
            };
            device_result(abi::gpiod_line_settings_set_direction(
                settings.pointer(),
                direction,
            ))?;
            abi::gpiod_line_settings_set_active_low(settings.pointer(), endpoint.active_low != 0);
            if endpoint.output != 0 {
                device_result(abi::gpiod_line_settings_set_output_value(
                    settings.pointer(),
                    endpoint.startup_active as i32,
                ))?;
            }
            let config = Handle::new(abi::gpiod_line_config_new(), abi::gpiod_line_config_free)?;
            device_result(abi::gpiod_line_config_add_line_settings(
                config.pointer(),
                &endpoint.line,
                1,
                settings.pointer(),
            ))?;
            let request = Handle::new(
                abi::gpiod_chip_request_lines(chip.pointer(), ptr::null_mut(), config.pointer()),
                abi::gpiod_line_request_release,
            )?;
            Ok(Self {
                request,
                offset: endpoint.line,
                output: endpoint.output != 0,
                safe_active: endpoint.safe_active as i32,
            })
        }
    }

    /// Submit one logical value. The kernel performs the configured polarity conversion.
    fn set(&self, active: i32) -> Result<(), i32> {
        // SAFETY: the line request is live and the caller serializes all accesses.
        device_result(unsafe {
            abi::gpiod_line_request_set_value(self.request.pointer(), self.offset, active)
        })
    }
}

/// Contain Rust failures at the C boundary and preserve distinct device/validation results.
fn boundary(operation: impl FnOnce() -> Result<(), i32>) -> i32 {
    catch_unwind(AssertUnwindSafe(operation))
        .unwrap_or(Err(-1))
        .err()
        .unwrap_or(0)
}

fn device_result(result: i32) -> Result<(), i32> {
    if result == 0 { Ok(()) } else { Err(1) }
}

/// Validate the leading size before reading the version or borrowing a full record.
/// The caller supplies an aligned readable size word and the declared byte extent.
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

unsafe extern "C" fn open(
    endpoint: *const abi::rptadv_gpiod_endpoint_v1,
    context: *mut *mut c_void,
) -> i32 {
    boundary(|| {
        // SAFETY: non-null output pointers are writable for the call.
        let context = unsafe { context.as_mut() }.ok_or(-1)?;
        *context = ptr::null_mut();
        unsafe { record(endpoint)? };
        // SAFETY: record validated the full extent; the caller keeps it live.
        let line = Line::acquire(unsafe { &*endpoint })?;
        *context = Box::into_raw(Box::new(line)).cast();
        Ok(())
    })
}

unsafe extern "C" fn read(
    context: *mut c_void,
    observation: *mut abi::rptadv_gpiod_observation_v1,
) -> i32 {
    boundary(|| {
        // SAFETY: the caller serializes the live context and lends writable output.
        let line = unsafe { context.cast::<Line>().as_ref() }.ok_or(-1)?;
        unsafe { record(observation)? };
        let observation = unsafe { &mut *observation };
        observation.quality = 0;
        if line.output {
            return Err(2);
        }
        // SAFETY: the requested offset and request remain live.
        let value =
            unsafe { abi::gpiod_line_request_get_value(line.request.pointer(), line.offset) };
        if value < 0 {
            observation.quality = 2;
            return Err(1);
        }
        observation.active = value as u32;
        observation.quality = 1;
        Ok(())
    })
}

unsafe extern "C" fn write(
    context: *mut c_void,
    output: *const abi::rptadv_gpiod_output_v1,
) -> i32 {
    boundary(|| {
        // SAFETY: the caller serializes the live context and lends the input record.
        let line = unsafe { context.cast::<Line>().as_ref() }.ok_or(-1)?;
        unsafe { record(output)? };
        let active = unsafe { (*output).active };
        if active > 1 {
            return Err(-1);
        }
        if !line.output {
            return Err(2);
        }
        line.set(active as i32)
    })
}

unsafe extern "C" fn capabilities(context: *mut c_void, capabilities: *mut u32) -> i32 {
    boundary(|| {
        // SAFETY: the caller keeps the context live and provides writable output.
        let line = unsafe { context.cast::<Line>().as_ref() }.ok_or(-1)?;
        let capabilities = unsafe { capabilities.as_mut() }.ok_or(-1)?;
        *capabilities = if line.output {
            abi::RPTADV_GPIOD_OUTPUT
        } else {
            abi::RPTADV_GPIOD_INPUT
        };
        Ok(())
    })
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    boundary(|| {
        if !context.is_null() {
            // SAFETY: called once, only after this context's operations have quiesced.
            let line = unsafe { Box::from_raw(context.cast::<Line>()) };
            if line.output {
                let _ = line.set(line.safe_active);
            }
        }
        Ok(())
    });
}

static DESCRIPTOR: abi::rptadv_gpiod_descriptor_v1 = abi::rptadv_gpiod_descriptor_v1 {
    struct_size: size_of::<abi::rptadv_gpiod_descriptor_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.gpiod\0\0\0\0",
    open: Some(open),
    read: Some(read),
    write: Some(write),
    capabilities: Some(capabilities),
    destroy: Some(destroy),
};

/// Return the immutable Linux GPIO descriptor for this DSO's lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_gpiod_adapter_descriptor_v1() -> *const abi::rptadv_gpiod_descriptor_v1 {
    &DESCRIPTOR
}

#[cfg(test)]
mod tests;
