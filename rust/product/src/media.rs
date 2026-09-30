//! Offline descriptor-backed media preparation and native-rate conversion.

use crate::abi as ffi;
use rpt_advanced_core::media::{
    Cancellation, FileRequest, MediaError, PreparedAudio, SpeechRequest,
};
use std::{
    ffi::{CStr, CString, c_void},
    mem::size_of,
    path::Path,
    ptr::{self, NonNull},
};

/// Independent file and speech owners, composed with the released native-rate converter.
pub struct NativeMediaPreparer {
    file_api: FileDescriptor,
    speech_api: SpeechDescriptor,
    file: Context,
    speech: Context,
}
/// File capability descriptor selected by the composition loader.
pub type FileDescriptor = ffi::rptadv_file_descriptor;
/// Speech capability descriptor selected independently by the composition loader.
pub type SpeechDescriptor = ffi::rptadv_speech_descriptor;

struct Context {
    handle: NonNull<c_void>,
    destroy: unsafe extern "C" fn(*mut c_void),
    read_stream: unsafe extern "C" fn(
        *mut c_void,
        *const ffi::rptadv_media_cancellation,
        *mut f32,
        usize,
        *mut usize,
    ) -> i32,
    close_stream: unsafe extern "C" fn(*mut c_void),
}
// SAFETY: each validated ABI permits concurrent independent requests. Context destruction
// requires exclusive ownership and cannot race a method borrowing this owner.
unsafe impl Send for NativeMediaPreparer {}
unsafe impl Sync for NativeMediaPreparer {}

impl NativeMediaPreparer {
    /// Compose independent file/speech capabilities and the host's paired child-reaper guard.
    /// Executable paths are literal local paths; timeout applies to each child.
    ///
    /// # Safety
    /// Each non-null pointer must provide a readable size/version prefix and truthfully back
    /// its claimed descriptor size. The loader retains both DSOs and reaper callback code
    /// until this owner and all preparations stop. No operation belongs on an audio worker.
    pub unsafe fn from_descriptors(
        file: *const FileDescriptor,
        speech: *const SpeechDescriptor,
        ffmpeg: &Path,
        piper: &Path,
        timeout_ms: u32,
        reaper: (unsafe extern "C" fn(), unsafe extern "C" fn()),
    ) -> Result<Self, MediaError> {
        if file.is_null() || speech.is_null() {
            return Err(MediaError::IncompatibleAdapter);
        }
        // SAFETY: readable prefixes are inspected before either complete table.
        if unsafe { ptr::addr_of!((*file).struct_size).read() } < size_of::<FileDescriptor>() as u32
            || unsafe { ptr::addr_of!((*file).abi_version).read() } != ffi::RPTADV_MEDIA_ABI_VERSION
            || unsafe { ptr::addr_of!((*speech).struct_size).read() }
                < size_of::<SpeechDescriptor>() as u32
            || unsafe { ptr::addr_of!((*speech).abi_version).read() }
                != ffi::RPTADV_MEDIA_ABI_VERSION
        {
            return Err(MediaError::IncompatibleAdapter);
        }
        // SAFETY: validated sizes back the complete tables.
        let file_api = unsafe { file.read() };
        let speech_api = unsafe { speech.read() };
        if file_api.capability.map(|v| v as u8) != *b"rptadv.file\0\0\0\0\0"
            || file_api.create.is_none()
            || file_api.destroy.is_none()
            || file_api.open_file.is_none()
            || file_api.read_stream.is_none()
            || file_api.close_stream.is_none()
            || speech_api.capability.map(|v| v as u8) != *b"rptadv.speech\0\0\0"
            || speech_api.create.is_none()
            || speech_api.destroy.is_none()
            || speech_api.open_speech.is_none()
            || speech_api.read_stream.is_none()
            || speech_api.close_stream.is_none()
        {
            return Err(MediaError::IncompatibleAdapter);
        }
        let ffmpeg = path_string(ffmpeg)?;
        let piper = path_string(piper)?;
        let mut config = ffi::rptadv_media_config {
            struct_size: size_of::<ffi::rptadv_media_config>() as u32,
            abi_version: ffi::RPTADV_MEDIA_ABI_VERSION,
            executable: ffmpeg.as_ptr(),
            timeout_ms,
            reaper_acquire: Some(reaper.0),
            reaper_release: Some(reaper.1),
        };
        let mut handle = ptr::null_mut();
        // SAFETY: strings/config remain borrowed through synchronous creation.
        status(unsafe { file_api.create.unwrap()(&config, &mut handle) })?;
        let file = Context {
            handle: NonNull::new(handle).ok_or(MediaError::IncompatibleAdapter)?,
            destroy: file_api.destroy.unwrap(),
            read_stream: file_api.read_stream.unwrap(),
            close_stream: file_api.close_stream.unwrap(),
        };
        config.executable = piper.as_ptr();
        handle = ptr::null_mut();
        // SAFETY: as above. The first owner rolls back if the second provider rejects creation.
        status(unsafe { speech_api.create.unwrap()(&config, &mut handle) })?;
        let speech = Context {
            handle: NonNull::new(handle).ok_or(MediaError::IncompatibleAdapter)?,
            destroy: speech_api.destroy.unwrap(),
            read_stream: speech_api.read_stream.unwrap(),
            close_stream: speech_api.close_stream.unwrap(),
        };
        Ok(Self {
            file_api,
            speech_api,
            file,
            speech,
        })
    }
}
impl Context {
    fn collect_stream(
        &self,
        cancellation: &Cancellation,
        open: impl FnOnce(*mut ffi::rptadv_media_stream) -> i32,
    ) -> Result<PreparedAudio, MediaError> {
        let mut stream = ffi::rptadv_media_stream {
            handle: ptr::null_mut(),
            sample_rate_hz: 0,
        };
        let code = open(&mut stream);
        struct Close<'a>(&'a Context, *mut c_void);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                if !self.1.is_null() {
                    // SAFETY: this guard owns the opened stream and closes it exactly once.
                    unsafe { (self.0.close_stream)(self.1) };
                }
            }
        }
        let close = Close(self, stream.handle);
        status(code)?;
        if stream.handle.is_null() || stream.sample_rate_hz == 0 {
            return Err(MediaError::InvalidOutput);
        }
        let control = raw_cancellation(cancellation);
        let mut samples = Vec::new();
        let mut chunk = [0.0_f32; 2048];
        loop {
            let mut count = 0;
            // SAFETY: stream remains open; chunk and cancellation token remain live for the call.
            status(unsafe {
                (self.read_stream)(
                    stream.handle,
                    &control,
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
        native(stream.sample_rate_hz, &samples, cancellation)
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: exclusive owner drops only after all borrowed preparations return.
        unsafe {
            (self.destroy)(self.handle.as_ptr());
        }
    }
}
impl rpt_advanced_core::runtime::NativeFilePreparer for NativeMediaPreparer {
    fn file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        let path = path_string(request.path)?;
        let cancellation = raw_cancellation(request.cancellation);
        self.file.collect_stream(request.cancellation, |output| {
            // SAFETY: all arguments are borrowed through this synchronous open call.
            unsafe {
                self.file_api.open_file.unwrap()(
                    self.file.handle.as_ptr(),
                    path.as_ptr(),
                    &cancellation,
                    output,
                )
            }
        })
    }
}
impl rpt_advanced_core::runtime::NativeSpeechPreparer for NativeMediaPreparer {
    fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        let text = CString::new(request.text).map_err(|_| MediaError::InvalidRequest)?;
        let model = path_string(request.model)?;
        let raw = ffi::rptadv_media_speech_request {
            text: text.as_ptr(),
            model: model.as_ptr(),
            speed_percent: request.speed_percent,
            level_db: request.level_db,
        };
        let cancellation = raw_cancellation(request.cancellation);
        self.speech.collect_stream(request.cancellation, |output| {
            // SAFETY: strings, callback context and request outlive the synchronous operation.
            unsafe {
                self.speech_api.open_speech.unwrap()(
                    self.speech.handle.as_ptr(),
                    &raw,
                    &cancellation,
                    output,
                )
            }
        })
    }
}
fn path_string(path: &Path) -> Result<CString, MediaError> {
    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| MediaError::InvalidRequest)
}
fn raw_cancellation(token: &Cancellation) -> ffi::rptadv_media_cancellation {
    unsafe extern "C" fn cancelled(context: *const c_void) -> u32 {
        // SAFETY: only raw_cancellation supplies this pointer; token outlives the call.
        u32::from(unsafe { &*context.cast::<Cancellation>() }.is_cancelled())
    }
    ffi::rptadv_media_cancellation {
        context: ptr::from_ref(token).cast(),
        is_cancelled: Some(cancelled),
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

fn native(
    rate: u32,
    source: &[f32],
    cancellation: &Cancellation,
) -> Result<PreparedAudio, MediaError> {
    // SAFETY: the linked provider retains its immutable descriptor and callback code.
    unsafe { native_with_descriptor(rate, source, cancellation, ffi::rpcr3_descriptor()) }
}

// The provider retains the descriptor and callback code throughout this synchronous conversion.
unsafe fn native_with_descriptor(
    rate: u32,
    source: &[f32],
    cancellation: &Cancellation,
    pointer: *const ffi::rpcr3_descriptor,
) -> Result<PreparedAudio, MediaError> {
    if cancellation.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    if rate == 0 || source.is_empty() || source.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
        return Err(MediaError::InvalidOutput);
    }
    // Finite media has no clock drift or PLC. Preload the real source, then
    // supply only the zero context required by the provider's intrinsic delay.
    // Trim that prefix and retain exactly ceil(source_frames * 48000 / source_rate).
    let count = source.len() as u64;
    if count > u64::from(u32::MAX) {
        return Err(MediaError::InvalidOutput);
    }
    let output_count = (source.len() as u64)
        .checked_mul(48000)
        .ok_or(MediaError::InvalidOutput)?
        .div_ceil(u64::from(rate));
    if pointer.is_null() {
        return Err(MediaError::IncompatibleAdapter);
    }
    if unsafe { ptr::addr_of!((*pointer).struct_size).read() }
        < size_of::<ffi::rpcr3_descriptor>() as u32
        || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != ffi::RPCR3_ABI_VERSION
    {
        return Err(MediaError::IncompatibleAdapter);
    }
    let api = unsafe { &*pointer };
    if api.capability_name.is_null()
        || unsafe { CStr::from_ptr(api.capability_name) } != c"rptadv.rate-adjusting-pcm-ring.f32"
        || api.ring_create.is_none()
        || api.ring_destroy.is_none()
        || api.ring_producer_push.is_none()
        || api.ring_consumer_render_sample.is_none()
        || api.ring_output_delay.is_none()
    {
        return Err(MediaError::IncompatibleAdapter);
    }
    struct Ring(&'static ffi::rpcr3_descriptor, NonNull<ffi::rpcr3_ring>);
    impl Drop for Ring {
        fn drop(&mut self) {
            // SAFETY: stopped local ring has no other endpoint owner.
            unsafe {
                self.0.ring_destroy.unwrap()(self.1.as_ptr());
            }
        }
    }
    let config = ffi::rpcr3_config {
        struct_size: size_of::<ffi::rpcr3_config>() as u32,
        abi_version: ffi::RPCR3_ABI_VERSION,
        capacity_samples: count.max(512),
        input_rate_hz: rate,
        output_rate_hz: 48000,
        reserve_samples: 0,
        target_samples: 0,
        max_producer_samples: count.max(512),
        max_output_samples: 1,
        plc_mode: 0,
    };
    let mut handle = ptr::null_mut();
    if unsafe { api.ring_create.unwrap()(&config, &mut handle) } != 0 {
        return Err(MediaError::IncompatibleAdapter);
    }
    let ring = Ring(
        api,
        NonNull::new(handle).ok_or(MediaError::IncompatibleAdapter)?,
    );
    let mut delay = 0;
    // SAFETY: this local ring is live and delay is writable count storage.
    if unsafe { api.ring_output_delay.unwrap()(ring.1.as_ptr(), &mut delay) } != 0 {
        return Err(MediaError::InvalidOutput);
    }
    let mut padding = delay
        .checked_add(1)
        .and_then(|frames| frames.checked_mul(u64::from(rate)))
        .ok_or(MediaError::InvalidOutput)?
        .div_ceil(48000);
    let render_bound = output_count
        .checked_add(delay)
        .and_then(|frames| frames.checked_add(512))
        .ok_or(MediaError::InvalidOutput)?;
    let mut accepted = 0;
    if unsafe {
        api.ring_producer_push.unwrap()(ring.1.as_ptr(), source.as_ptr(), count, &mut accepted)
    } != 0
        || accepted != count
    {
        return Err(MediaError::InvalidOutput);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_count as usize)
        .map_err(|_| MediaError::Io)?;
    // Context is not playable PCM. Refill it as space becomes available so
    // finite conversion needs no second converter or padded source allocation.
    let zeros = [0.0; 512];
    for _ in 0..render_bound {
        if cancellation.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        if padding != 0 {
            let offered = padding.min(zeros.len() as u64);
            let mut accepted = 0;
            // SAFETY: the fixed zero block and output count remain live for this call.
            if unsafe {
                api.ring_producer_push.unwrap()(
                    ring.1.as_ptr(),
                    zeros.as_ptr(),
                    offered,
                    &mut accepted,
                )
            } != 0
                || accepted > offered
            {
                return Err(MediaError::InvalidOutput);
            }
            padding -= accepted;
        }
        let mut sample = 0.0;
        let mut real = false;
        if unsafe {
            api.ring_consumer_render_sample.unwrap()(ring.1.as_ptr(), &mut sample, &mut real)
        } != 0
        {
            return Err(MediaError::InvalidOutput);
        }
        if real {
            if delay != 0 {
                delay -= 1;
            } else {
                output.push(sample);
            }
        }
        if output.len() as u64 == output_count {
            return PreparedAudio::new(48000, output);
        }
    }
    Err(MediaError::InvalidOutput)
}

#[cfg(test)]
mod tests;
