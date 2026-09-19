//! C entry points, with panic containment and explicit adapter-side destruction.
use crate::{ChildReaper, Config, Preparation, abi::*};
use crate::{MediaError, result::PreparedAudio};
use std::{
    ffi::{CStr, c_char, c_void},
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
    prepare_file: Some(prepare_file),
    #[cfg(speech_adapter)]
    prepare_speech: Some(prepare_speech),
    release_audio: Some(release_audio),
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
                temporary_directory: path(raw.temporary_directory)?,
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

fn empty_audio() -> RawAudio {
    RawAudio {
        handle: ptr::null_mut(),
        samples: ptr::null(),
        sample_count: 0,
        sample_rate_hz: 0,
    }
}

unsafe fn prepare_arguments<'a>(
    context: *const c_void,
    cancellation: *const RawCancellation,
    output: *mut RawAudio,
) -> Result<(&'a Preparation, &'a RawCancellation), MediaError> {
    if output.is_null() {
        return Err(MediaError::InvalidRequest);
    }
    // SAFETY: all checked arguments must point to their specified live ABI objects.
    unsafe {
        *output = empty_audio();
    }
    let preparation =
        unsafe { context.cast::<Preparation>().as_ref() }.ok_or(MediaError::InvalidRequest)?;
    let cancellation = unsafe { cancellation.as_ref() }.ok_or(MediaError::InvalidRequest)?;
    Ok((preparation, cancellation))
}

unsafe fn publish(
    cancellation: &RawCancellation,
    output: *mut RawAudio,
    prepared: PreparedAudio,
) -> Result<(), MediaError> {
    let audio = Box::new(prepared);
    if cancelled(cancellation) {
        return Err(MediaError::Cancelled);
    }
    let view = RawAudio {
        samples: audio.samples().as_ptr(),
        sample_count: audio.samples().len(),
        sample_rate_hz: audio.sample_rate_hz(),
        handle: Box::into_raw(audio).cast(),
    };
    // SAFETY: output now owns view.handle; samples remain owned by that handle.
    unsafe {
        *output = view;
    }
    Ok(())
}

#[cfg(file_adapter)]
unsafe extern "C" fn prepare_file(
    context: *const c_void,
    source: *const c_char,
    cancellation: *const RawCancellation,
    output: *mut RawAudio,
) -> i32 {
    // SAFETY: the foreign caller provides valid arguments; prepare validates nulls.
    boundary(catch_unwind(AssertUnwindSafe(|| unsafe {
        let (preparation, cancellation) = prepare_arguments(context, cancellation, output)?;
        let audio = preparation.prepare_file(&path(source)?, &|| cancelled(cancellation))?;
        publish(cancellation, output, audio)
    })))
}

#[cfg(speech_adapter)]
unsafe extern "C" fn prepare_speech(
    context: *const c_void,
    request: *const RawSpeechRequest,
    cancellation: *const RawCancellation,
    output: *mut RawAudio,
) -> i32 {
    // SAFETY: request and strings are borrowed for this synchronous call only.
    boundary(catch_unwind(AssertUnwindSafe(|| unsafe {
        let (preparation, cancellation) = prepare_arguments(context, cancellation, output)?;
        let request = request.as_ref().ok_or(MediaError::InvalidRequest)?;
        let Ok(text) = string(request.text)?.to_str() else {
            return Err(MediaError::InvalidRequest);
        };
        let audio = preparation.prepare_speech(
            text,
            &path(request.model)?,
            request.speed_percent,
            request.level_db,
            &|| cancelled(cancellation),
        )?;
        publish(cancellation, output, audio)
    })))
}

unsafe extern "C" fn release_audio(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: the caller returns its prepared handle exactly once.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(handle.cast::<PreparedAudio>()));
        }));
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
