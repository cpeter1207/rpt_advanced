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
    release_audio: unsafe extern "C" fn(*mut c_void),
}
// SAFETY: each validated ABI permits concurrent independent requests. Context destruction
// requires exclusive ownership and cannot race a method borrowing this owner.
unsafe impl Send for NativeMediaPreparer {}
unsafe impl Sync for NativeMediaPreparer {}

impl NativeMediaPreparer {
    /// Compose independent file/speech capabilities and the host's paired child-reaper guard.
    /// Paths are literal local executable/directory paths; timeout applies to each child.
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
        temporary_directory: &Path,
        timeout_ms: u32,
        reaper: (unsafe extern "C" fn(), unsafe extern "C" fn()),
    ) -> Result<Self, MediaError> {
        if file.is_null() || speech.is_null() {
            return Err(MediaError::IncompatibleAdapter);
        }
        // SAFETY: readable prefixes are inspected before either complete table.
        if unsafe { ptr::addr_of!((*file).struct_size).read() } < size_of::<FileDescriptor>() as u32
            || unsafe { ptr::addr_of!((*file).abi_version).read() } != 1
            || unsafe { ptr::addr_of!((*speech).struct_size).read() }
                < size_of::<SpeechDescriptor>() as u32
            || unsafe { ptr::addr_of!((*speech).abi_version).read() } != 1
        {
            return Err(MediaError::IncompatibleAdapter);
        }
        // SAFETY: validated sizes back the complete tables.
        let file_api = unsafe { file.read() };
        let speech_api = unsafe { speech.read() };
        if file_api.capability.map(|v| v as u8) != *b"rptadv.file\0\0\0\0\0"
            || file_api.create.is_none()
            || file_api.destroy.is_none()
            || file_api.prepare_file.is_none()
            || file_api.release_audio.is_none()
            || speech_api.capability.map(|v| v as u8) != *b"rptadv.speech\0\0\0"
            || speech_api.create.is_none()
            || speech_api.destroy.is_none()
            || speech_api.prepare_speech.is_none()
            || speech_api.release_audio.is_none()
        {
            return Err(MediaError::IncompatibleAdapter);
        }
        let ffmpeg = path_string(ffmpeg)?;
        let piper = path_string(piper)?;
        let directory = path_string(temporary_directory)?;
        let mut config = ffi::rptadv_media_config {
            struct_size: size_of::<ffi::rptadv_media_config>() as u32,
            abi_version: 1,
            executable: ffmpeg.as_ptr(),
            temporary_directory: directory.as_ptr(),
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
            release_audio: file_api.release_audio.unwrap(),
        };
        config.executable = piper.as_ptr();
        handle = ptr::null_mut();
        // SAFETY: as above. The first owner rolls back if the second provider rejects creation.
        status(unsafe { speech_api.create.unwrap()(&config, &mut handle) })?;
        let speech = Context {
            handle: NonNull::new(handle).ok_or(MediaError::IncompatibleAdapter)?,
            destroy: speech_api.destroy.unwrap(),
            release_audio: speech_api.release_audio.unwrap(),
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
    fn receive(
        &self,
        cancellation: &Cancellation,
        call: impl FnOnce(*mut ffi::rptadv_media_audio) -> i32,
    ) -> Result<PreparedAudio, MediaError> {
        let mut output = ffi::rptadv_media_audio {
            handle: ptr::null_mut(),
            samples: ptr::null(),
            sample_count: 0,
            sample_rate_hz: 0,
        };
        let code = call(&mut output);
        struct Release<'a>(&'a Context, *mut c_void);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                if !self.1.is_null() {
                    // SAFETY: this guard exclusively owns the returned handle, even on malformed output.
                    unsafe {
                        (self.0.release_audio)(self.1);
                    }
                }
            }
        }
        let _release = Release(self, output.handle);
        status(code)?;
        if !valid_output(&output) {
            return Err(MediaError::InvalidOutput);
        }
        // SAFETY: successful validated descriptor output promises this immutable owned view.
        native(
            output.sample_rate_hz,
            unsafe { std::slice::from_raw_parts(output.samples, output.sample_count) },
            cancellation,
        )
    }
}

fn valid_output(output: &ffi::rptadv_media_audio) -> bool {
    !output.handle.is_null()
        && !output.samples.is_null()
        && output.sample_count != 0
        && output.sample_count <= isize::MAX as usize / size_of::<f32>()
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
        self.file.receive(request.cancellation, |output| {
            // SAFETY: all arguments are borrowed through this synchronous call.
            unsafe {
                self.file_api.prepare_file.unwrap()(
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
        self.speech.receive(request.cancellation, |output| {
            // SAFETY: strings, callback context and request outlive the synchronous operation.
            unsafe {
                self.speech_api.prepare_speech.unwrap()(
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
    unsafe { native_with_descriptor(rate, source, cancellation, ffi::rpcr2_descriptor()) }
}

// The provider retains the descriptor and callback code throughout this synchronous conversion.
unsafe fn native_with_descriptor(
    rate: u32,
    source: &[f32],
    cancellation: &Cancellation,
    pointer: *const ffi::rpcr2_descriptor,
) -> Result<PreparedAudio, MediaError> {
    if cancellation.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    if rate == 0 || source.is_empty() || source.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
        return Err(MediaError::InvalidOutput);
    }
    // Finite media has no clock drift. ABI 2 has no EOF flush: provide bounded zero context
    // (512 source frames per downsampling factor, beyond fastest-sinc support), use target 0,
    // then retain exactly ceil(source_frames * 48000 / source_rate) real output samples.
    // Padding is converter context, never an extra playable tail or a second resampler.
    let padding = u64::from(rate).div_ceil(48000) * 512;
    let count = (source.len() as u64)
        .checked_add(padding)
        .filter(|v| *v <= u64::from(u32::MAX))
        .ok_or(MediaError::InvalidOutput)?;
    let output_count = (source.len() as u64)
        .checked_mul(48000)
        .ok_or(MediaError::InvalidOutput)?
        .div_ceil(u64::from(rate));
    let mut input = Vec::new();
    input
        .try_reserve_exact(count as usize)
        .map_err(|_| MediaError::Io)?;
    input.extend_from_slice(source);
    input.resize(count as usize, 0.0);
    if pointer.is_null() {
        return Err(MediaError::IncompatibleAdapter);
    }
    if unsafe { ptr::addr_of!((*pointer).struct_size).read() }
        < size_of::<ffi::rpcr2_descriptor>() as u32
        || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != 2
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
    {
        return Err(MediaError::IncompatibleAdapter);
    }
    struct Ring(&'static ffi::rpcr2_descriptor, NonNull<ffi::rpcr2_ring>);
    impl Drop for Ring {
        fn drop(&mut self) {
            // SAFETY: stopped local ring has no other endpoint owner.
            unsafe {
                self.0.ring_destroy.unwrap()(self.1.as_ptr());
            }
        }
    }
    let config = ffi::rpcr2_config {
        struct_size: size_of::<ffi::rpcr2_config>() as u32,
        abi_version: 2,
        capacity_samples: count.max(512),
        input_rate_hz: rate,
        output_rate_hz: 48000,
        quality: 2,
    };
    let mut handle = ptr::null_mut();
    if unsafe { api.ring_create.unwrap()(&config, &mut handle) } != 0 {
        return Err(MediaError::IncompatibleAdapter);
    }
    let ring = Ring(
        api,
        NonNull::new(handle).ok_or(MediaError::IncompatibleAdapter)?,
    );
    let mut accepted = 0;
    if unsafe {
        api.ring_producer_push.unwrap()(ring.1.as_ptr(), input.as_ptr(), count, &mut accepted)
    } != 0
        || accepted != count
    {
        return Err(MediaError::InvalidOutput);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_count as usize)
        .map_err(|_| MediaError::Io)?;
    // Allow bounded converter startup, but never retain ring concealment in a prepared file.
    for _ in 0..output_count.saturating_add(padding * 48000 / u64::from(rate) + 512) {
        if cancellation.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let mut sample = 0.0;
        let mut real = false;
        if unsafe {
            api.ring_consumer_render_sample.unwrap()(ring.1.as_ptr(), &mut sample, 0, &mut real)
        } != 0
        {
            return Err(MediaError::InvalidOutput);
        }
        if real {
            output.push(sample);
        }
        if output.len() as u64 == output_count {
            return PreparedAudio::new(48000, output);
        }
    }
    Err(MediaError::InvalidOutput)
}

#[cfg(test)]
mod tests;
