//! Safe control-plane owner for an independently replaceable C descriptor.
use crate::provider::{Config, abi::*};
use rpt_advanced_core::media::{Cancellation, MediaError, PreparedAudio};
#[cfg(file_adapter)]
use rpt_advanced_core::media::{FilePreparer, FileRequest};
#[cfg(speech_adapter)]
use rpt_advanced_core::media::{SpeechPreparer, SpeechRequest};
use std::{
    ffi::{CString, c_void},
    path::Path,
    ptr::{self, NonNull},
};

/// Owns one validated media adapter context and releases all foreign results.
pub struct MediaAdapter {
    descriptor: Descriptor,
    context: NonNull<c_void>,
}

// SAFETY: every selected descriptor must support concurrent preparation calls.
unsafe impl Send for MediaAdapter {}
// SAFETY: configuration is immutable; each call owns its own process and files.
unsafe impl Sync for MediaAdapter {}

impl MediaAdapter {
    /// Select the linked provider through its versioned descriptor.
    pub fn new(config: Config) -> Result<Self, MediaError> {
        // SAFETY: the built-in provider and its table are process-lifetime objects.
        unsafe {
            Self::from_descriptor(
                {
                    #[cfg(file_adapter)]
                    {
                        crate::provider::rptadv_file_adapter_descriptor()
                    }
                    #[cfg(speech_adapter)]
                    {
                        crate::provider::rptadv_speech_adapter_descriptor()
                    }
                },
                config,
            )
        }
    }

    /// Select an already loaded provider after controlled composition validation.
    ///
    /// # Safety
    /// The descriptor must be null or provide a readable ABI prefix; a non-null
    /// prefix claiming the complete table must back that allocation. It must also
    /// implement the documented ABI, including thread safety, truthful PCM views
    /// and ownership. Its code and host callbacks must stay loaded until this owner
    /// and all calls stop. Loading/unloading is host-owned.
    pub unsafe fn from_descriptor(
        descriptor: *const Descriptor,
        config: Config,
    ) -> Result<Self, MediaError> {
        // SAFETY: caller guarantees the versioned descriptor prefix is readable.
        let descriptor = unsafe { Descriptor::from_pointer(descriptor) }
            .map_err(|_| MediaError::IncompatibleAdapter)?;
        #[cfg(file_adapter)]
        let executable = path_string(&config.ffmpeg)?;
        #[cfg(speech_adapter)]
        let executable = path_string(&config.piper)?;
        let timeout_ms = u32::try_from(config.process_timeout.as_millis())
            .ok()
            .filter(|value| *value > 0)
            .ok_or(MediaError::InvalidRequest)?;
        let raw = RawConfig {
            struct_size: size_of::<RawConfig>() as u32,
            abi_version: ABI_VERSION,
            executable: executable.as_ptr(),
            timeout_ms,
            reaper_acquire: config.child_reaper.map(|reaper| reaper.acquire),
            reaper_release: config.child_reaper.map(|reaper| reaper.release),
        };
        let mut context = ptr::null_mut();
        // SAFETY: the table is validated; arguments remain live until it returns.
        status(unsafe { descriptor.create.unwrap()(&raw, &mut context) })?;
        Ok(Self {
            descriptor: *descriptor,
            context: NonNull::new(context).ok_or(MediaError::IncompatibleAdapter)?,
        })
    }

    fn receive(
        &self,
        control: &RawCancellation,
        call: impl FnOnce(*mut RawStream) -> i32,
    ) -> Result<PreparedAudio, MediaError> {
        let mut raw = RawStream {
            handle: ptr::null_mut(),
            sample_rate_hz: 0,
        };
        let result = call(&mut raw);
        let close = StreamOwner {
            handle: raw.handle,
            close: self.descriptor.close_stream.unwrap(),
        };
        status(result)?;
        if raw.handle.is_null() || raw.sample_rate_hz == 0 {
            return Err(MediaError::InvalidOutput);
        }
        let mut samples = Vec::new();
        let mut chunk = [0.0; 2048];
        loop {
            let mut count = 0;
            // SAFETY: stream, control, and bounded output remain live for the call.
            status(unsafe {
                self.descriptor.read_stream.unwrap()(
                    raw.handle,
                    control,
                    chunk.as_mut_ptr(),
                    chunk.len(),
                    &mut count,
                )
            })?;
            if count > chunk.len() {
                return Err(MediaError::InvalidOutput);
            }
            if count == 0 {
                break;
            }
            samples.try_reserve(count).map_err(|_| MediaError::Io)?;
            samples.extend_from_slice(&chunk[..count]);
        }
        drop(close);
        PreparedAudio::new(raw.sample_rate_hz, samples)
    }
}

impl Drop for MediaAdapter {
    fn drop(&mut self) {
        // SAFETY: ownership and borrowing ensure all calls have ended.
        unsafe {
            self.descriptor.destroy.unwrap()(self.context.as_ptr());
        }
    }
}

struct StreamOwner {
    handle: *mut c_void,
    close: unsafe extern "C" fn(*mut c_void),
}
impl Drop for StreamOwner {
    fn drop(&mut self) {
        // SAFETY: the result is returned to the same provider even on error.
        unsafe {
            (self.close)(self.handle);
        }
    }
}

fn path_string(path: &Path) -> Result<CString, MediaError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).map_err(|_| MediaError::InvalidRequest)
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_str().ok_or(MediaError::InvalidRequest)?)
            .map_err(|_| MediaError::InvalidRequest)
    }
}

extern "C" fn cancelled(context: *const c_void) -> u32 {
    // SAFETY: this callback is only installed with a borrowed Cancellation below.
    u32::from(unsafe { &*context.cast::<Cancellation>() }.is_cancelled())
}
fn cancellation(token: &Cancellation) -> RawCancellation {
    RawCancellation {
        context: ptr::from_ref(token).cast(),
        is_cancelled: cancelled,
    }
}

#[cfg(file_adapter)]
impl FilePreparer for MediaAdapter {
    fn prepare_file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        let path = path_string(request.path)?;
        let control = cancellation(request.cancellation);
        // SAFETY: all borrowed arguments outlive this synchronous call.
        self.receive(&control, |output| unsafe {
            self.descriptor.open_file.unwrap()(
                self.context.as_ptr(),
                path.as_ptr(),
                &control,
                output,
            )
        })
    }
}
#[cfg(speech_adapter)]
impl SpeechPreparer for MediaAdapter {
    fn prepare_speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        let text = CString::new(request.text).map_err(|_| MediaError::InvalidRequest)?;
        let model = path_string(request.model)?;
        let control = cancellation(request.cancellation);
        let raw = RawSpeechRequest {
            text: text.as_ptr(),
            model: model.as_ptr(),
            speed_percent: request.speed_percent,
            level_db: request.level_db,
        };
        // SAFETY: all borrowed arguments outlive this synchronous call.
        self.receive(&control, |output| unsafe {
            self.descriptor.open_speech.unwrap()(self.context.as_ptr(), &raw, &control, output)
        })
    }
}

fn status(code: i32) -> Result<(), MediaError> {
    Err(match code {
        0 => return Ok(()),
        -1 => MediaError::InvalidRequest,
        -2 => MediaError::Unavailable,
        -3 => MediaError::Io,
        -4 => MediaError::ProcessFailed,
        -5 => MediaError::TimedOut,
        -6 => MediaError::Cancelled,
        -7 => MediaError::InvalidOutput,
        _ => MediaError::IncompatibleAdapter,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_descriptor_is_rejected_before_full_table_access() {
        let truncated = [8_u32, ABI_VERSION];
        assert!(matches!(
            // SAFETY: the allocation contains exactly the readable ABI prefix.
            unsafe {
                MediaAdapter::from_descriptor(
                    truncated.as_ptr().cast(),
                    Config {
                        #[cfg(file_adapter)]
                        ffmpeg: "ffmpeg".into(),
                        #[cfg(speech_adapter)]
                        piper: "piper".into(),
                        process_timeout: std::time::Duration::from_secs(1),
                        child_reaper: None,
                    },
                )
            },
            Err(MediaError::IncompatibleAdapter)
        ));
    }
}
