//! C entry points, with panic containment and explicit adapter-side destruction.
use crate::{ChildReaper, Config, Preparation, abi::*};
use crate::{MediaError, process::PcmStream};
use std::{
    ffi::{CStr, c_char, c_void},
    mem::MaybeUninit,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    ptr,
    time::Duration,
};

fn cancelled(cancellation: &RawCancellation) -> bool {
    (cancellation.is_cancelled)(cancellation.context) != 0
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor {
    struct_size: size_of::<Descriptor>() as u32,
    abi_version: ABI_VERSION,
    capability: CAPABILITY,
    create: Some(create),
    destroy: Some(destroy),
    #[cfg(file_adapter)]
    open_file: Some(open_file),
    #[cfg(speech_adapter)]
    open_speech: Some(open_speech),
    step_stream: Some(step_stream),
    close_stream: Some(close_stream),
};

/// Return the process-lifetime media descriptor. It is never freed or modified.
#[unsafe(no_mangle)]
#[cfg(file_adapter)]
pub extern "C" fn rptadv_file_adapter_descriptor() -> *const Descriptor {
    &DESCRIPTOR
}

/// Return the process-lifetime speech descriptor. It is never freed or modified.
#[unsafe(no_mangle)]
#[cfg(speech_adapter)]
pub extern "C" fn rptadv_speech_adapter_descriptor() -> *const Descriptor {
    &DESCRIPTOR
}

fn boundary(result: std::thread::Result<Result<(), MediaError>>) -> i32 {
    const STATUS: [i32; 8] = [-1, -2, -3, -4, -5, -6, -7, -3];
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => STATUS[error as usize],
        Err(_) => -3,
    }
}

unsafe fn string<'a>(value: *const c_char) -> Result<&'a CStr, MediaError> {
    if value.is_null() {
        return Err(MediaError::InvalidRequest);
    }
    // SAFETY: each ABI caller must provide live, terminated strings for the call.
    Ok(unsafe { CStr::from_ptr(value) })
}

unsafe fn path(value: *const c_char) -> Result<PathBuf, MediaError> {
    // SAFETY: the raw path has the same lifetime requirements as string().
    let string = unsafe { string(value)? };
    if string.is_empty() {
        return Err(MediaError::InvalidRequest);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(std::ffi::OsStr::from_bytes(string.to_bytes()).into())
    }
    #[cfg(not(unix))]
    {
        Ok(string
            .to_str()
            .map_err(|_| MediaError::InvalidRequest)?
            .into())
    }
}

unsafe extern "C" fn create(config: *const RawConfig, output: *mut *mut c_void) -> i32 {
    boundary(catch_unwind(AssertUnwindSafe(|| {
        if output.is_null() {
            return Err(MediaError::InvalidRequest);
        }
        // SAFETY: output is writable by the ABI contract.
        unsafe {
            *output = ptr::null_mut();
        }
        // SAFETY: the ABI requires a readable config prefix; this validates it first.
        let raw = unsafe { RawConfig::from_pointer(config) }?;
        if raw.timeout_ms == 0 {
            return Err(MediaError::InvalidRequest);
        }
        let child_reaper = match (raw.reaper_acquire, raw.reaper_release) {
            (None, None) => None,
            (Some(acquire), Some(release)) => Some(ChildReaper { acquire, release }),
            _ => return Err(MediaError::InvalidRequest),
        };
        // SAFETY: strings are borrowed only for this creation call and copied.
        let config = unsafe {
            Config {
                #[cfg(file_adapter)]
                ffmpeg: path(raw.executable)?,
                #[cfg(speech_adapter)]
                piper: path(raw.executable)?,
                process_timeout: Duration::from_millis(raw.timeout_ms.into()),
                child_reaper,
            }
        };
        let preparation = Box::new(Preparation::new(config));
        // SAFETY: output is writable and receives the sole ownership handle.
        unsafe {
            *output = Box::into_raw(preparation).cast();
        }
        Ok(())
    })))
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: caller returns this handle once after all preparation calls stop.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(context.cast::<Preparation>()));
        }));
    }
}

unsafe fn open_arguments<'a>(
    context: *const c_void,
    cancellation: *const RawCancellation,
    output: *mut *mut c_void,
) -> Result<(&'a Preparation, &'a RawCancellation), MediaError> {
    if output.is_null() {
        return Err(MediaError::InvalidRequest);
    }
    // SAFETY: all checked arguments must point to their specified live ABI objects.
    unsafe {
        *output = ptr::null_mut();
    }
    let preparation =
        unsafe { context.cast::<Preparation>().as_ref() }.ok_or(MediaError::InvalidRequest)?;
    let cancellation = unsafe { cancellation.as_ref() }.ok_or(MediaError::InvalidRequest)?;
    Ok((preparation, cancellation))
}

unsafe fn publish(output: *mut *mut c_void, stream: PcmStream) {
    // SAFETY: output receives the sole ownership handle. After successful setup
    // the caller owns retirement, including when cancellation races publication.
    unsafe {
        output.write(Box::into_raw(Box::new(stream)).cast());
    }
}

#[cfg(file_adapter)]
unsafe extern "C" fn open_file(
    context: *const c_void,
    source: *const c_char,
    cancellation: *const RawCancellation,
    output: *mut *mut c_void,
) -> i32 {
    // SAFETY: the foreign caller provides valid arguments; prepare validates nulls.
    boundary(catch_unwind(AssertUnwindSafe(|| unsafe {
        let (preparation, cancellation) = open_arguments(context, cancellation, output)?;
        let stream = preparation.open_file(&path(source)?, &|| cancelled(cancellation))?;
        publish(output, stream);
        Ok(())
    })))
}

#[cfg(speech_adapter)]
unsafe extern "C" fn open_speech(
    context: *const c_void,
    request: *const RawSpeechRequest,
    cancellation: *const RawCancellation,
    output: *mut *mut c_void,
) -> i32 {
    // SAFETY: request and strings are borrowed for this synchronous call only.
    boundary(catch_unwind(AssertUnwindSafe(|| unsafe {
        let (preparation, cancellation) = open_arguments(context, cancellation, output)?;
        let request = request.as_ref().ok_or(MediaError::InvalidRequest)?;
        let Ok(text) = string(request.text)?.to_str() else {
            return Err(MediaError::InvalidRequest);
        };
        let stream = preparation.open_speech(
            text,
            &path(request.model)?,
            request.speed_percent,
            request.level_db,
            &|| cancelled(cancellation),
        )?;
        publish(output, stream);
        Ok(())
    })))
}

unsafe extern "C" fn step_stream(
    handle: *mut c_void,
    cancellation: *const RawCancellation,
    output: *mut f32,
    capacity: usize,
    result: *mut RawStep,
) -> i32 {
    boundary(catch_unwind(AssertUnwindSafe(|| unsafe {
        if result.is_null() {
            return Err(MediaError::InvalidRequest);
        }
        result.write(RawStep::default());
        if output.is_null() || capacity == 0 || capacity > isize::MAX as usize / size_of::<f32>() {
            return Err(MediaError::InvalidRequest);
        }
        let cancellation = cancellation.as_ref().ok_or(MediaError::InvalidRequest)?;
        if cancelled(cancellation) {
            return Err(MediaError::Cancelled);
        }
        let stream = handle
            .cast::<PcmStream>()
            .as_mut()
            .ok_or(MediaError::InvalidRequest)?;
        // The C caller's output storage need not be initialized.
        let samples = std::slice::from_raw_parts_mut(output.cast::<MaybeUninit<f32>>(), capacity);
        result.write(stream.step(samples, &|| cancelled(cancellation))?);
        Ok(())
    })))
}

unsafe extern "C" fn close_stream(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: the caller closes its stream exactly once.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(handle.cast::<PcmStream>()));
        }));
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
