//! Validated host-services clients; native handles never cross into core policy.

use crate::{Error, abi};
use rpt_advanced_core::schedule::{CivilTime, Weekday};
use std::{
    ffi::{CStr, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
};

/// Borrowed peer event delivered synchronously by the host owner.
pub enum PeerInput<'a> {
    /// Complete text payload.
    Text(&'a [u8]),
    /// Completed conventional DTMF digit.
    Digit(char),
    /// Decoded normalized F32 PCM.
    Audio(&'a [f32]),
}

/// Validated process-lifetime host capability.
#[derive(Clone, Copy)]
pub struct HostServices(&'static abi::rptadv_host_services_v2);
// SAFETY: validation requires a process-lifetime immutable table. Opaque objects remain
// uniquely owned; the host documents independent operation on their owner threads.
unsafe impl Send for HostServices {}
// SAFETY: the immutable table and context may be copied; object operations remain serialized.
unsafe impl Sync for HostServices {}

fn complete(api: &abi::rptadv_host_services_v2) -> bool {
    [
        api.local_time.is_some(),
        api.command_notice.is_some(),
        api.reaper_acquire.is_some(),
        api.reaper_release.is_some(),
        api.directory_lookup.is_some(),
        api.radio_open.is_some(),
        api.radio_activate.is_some(),
        api.radio_destroy.is_some(),
        api.peer_dial.is_some(),
        api.peer_rate.is_some(),
        api.peer_ready.is_some(),
        api.peer_read.is_some(),
        api.peer_send_text.is_some(),
        api.peer_send_digit.is_some(),
        api.peer_write.is_some(),
        api.peer_destroy.is_some(),
    ]
    .into_iter()
    .all(|present| present)
}

impl HostServices {
    /// Validate the complete table before any callback or context is retained.
    ///
    /// # Safety
    /// `pointer` exposes a truthful readable size/version prefix and a process-lifetime
    /// immutable complete table. Its code/context remain live through product stop; context
    /// calls are thread-safe, while each returned handle permits exactly one serial owner.
    /// Callbacks are synchronous, do not retain borrowed buffers, provide aligned bounded
    /// slices, follow documented ownership/status values, and never unwind.
    pub unsafe fn open(pointer: *const abi::rptadv_host_services_v2) -> Result<Self, Error> {
        if pointer.is_null()
            || unsafe { ptr::addr_of!((*pointer).struct_size).read() }
                < size_of::<abi::rptadv_host_services_v2>() as u32
            || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != 2
        {
            return Err(Error::Admission);
        }
        let api = unsafe { &*pointer };
        if api.capability != *b"rptadv.hst2\0" || !complete(api) {
            return Err(Error::Admission);
        }
        Ok(Self(api))
    }
    /// Convert one Unix second using the host process's configured local timezone.
    pub fn local_time(&self, unix_seconds: i64) -> Option<(CivilTime, u8)> {
        let mut raw = abi::rptadv_local_time_v1 {
            struct_size: size_of::<abi::rptadv_local_time_v1>() as u32,
            valid: 0,
            year: 0,
            month: 0,
            day: 0,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        if unsafe { self.0.local_time.unwrap()(self.0.context, unix_seconds, &mut raw) } != 0
            || raw.valid == 0
            || raw.weekday > 6
            || raw.second >= 60
        {
            return None;
        }
        let weekday = [
            Weekday::Sunday,
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
        ][raw.weekday as usize];
        CivilTime::new(raw.year, raw.month, raw.day, weekday, raw.hour, raw.minute)
            .ok()
            .map(|civil| (civil, raw.second))
    }
    /// Emit the existing link-command completion notice through the host logger.
    pub fn command_notice(&self, local: &str, completed: bool) {
        unsafe {
            self.0.command_notice.unwrap()(
                self.0.context,
                local.as_ptr().cast(),
                local.len(),
                u32::from(completed),
            );
        }
    }
    /// Child-reaper acquisition callback for process-backed media providers.
    pub fn reaper_acquire(&self) -> unsafe extern "C" fn() {
        self.0.reaper_acquire.unwrap()
    }
    /// Child-reaper release callback for process-backed media providers.
    pub fn reaper_release(&self) -> unsafe extern "C" fn() {
        self.0.reaper_release.unwrap()
    }
    /// Resolve one requested ASL destination through the host's public directory facilities.
    pub fn lookup(
        &self,
        method: u32,
        static_file: &str,
        external_file: &str,
        remote: &str,
        source: Option<&str>,
    ) -> Result<String, Error> {
        let mut output = [0_u8; 1024];
        let mut written = 0;
        let source = source.unwrap_or("");
        let code = unsafe {
            self.0.directory_lookup.unwrap()(
                self.0.context,
                method,
                static_file.as_ptr().cast(),
                static_file.len(),
                external_file.as_ptr().cast(),
                external_file.len(),
                remote.as_ptr().cast(),
                remote.len(),
                source.as_ptr().cast(),
                source.len(),
                output.as_mut_ptr().cast(),
                output.len(),
                &mut written,
            )
        };
        if code != 0 || written > output.len() {
            return Err(Error::Operation);
        }
        std::str::from_utf8(&output[..written])
            .map(str::to_owned)
            .map_err(|_| Error::Operation)
    }
    /// Reserve one uniquely owned radio channel without starting callbacks.
    pub fn radio(&self, name: &str, maximum_frames: usize) -> Result<Radio, Error> {
        let mut handle = ptr::null_mut();
        let code = unsafe {
            self.0.radio_open.unwrap()(
                self.0.context,
                name.as_ptr().cast(),
                name.len(),
                maximum_frames,
                &mut handle,
            )
        };
        if code != 0 {
            return Err(Error::Operation);
        }
        Ok(Radio {
            services: *self,
            maximum_frames,
            handle: NonNull::new(handle).ok_or(Error::Operation)?,
        })
    }
    /// Wrap one peer whose ownership has transferred from the host callback.
    ///
    /// # Safety
    /// `handle` must be a unique live peer created by this same services table.
    pub unsafe fn peer(&self, handle: *mut c_void) -> Result<PeerIo, Error> {
        Ok(PeerIo {
            services: *self,
            handle: NonNull::new(handle).ok_or(Error::Admission)?,
        })
    }
    /// Dial one peer while the supplied generation remains current.
    pub fn dial<F>(
        &self,
        destination: &str,
        local: &str,
        maximum_frames: usize,
        mut current: F,
    ) -> Result<PeerIo, Error>
    where
        F: FnMut() -> bool,
    {
        unsafe extern "C" fn current_callback<F: FnMut() -> bool>(context: *mut c_void) -> u32 {
            u32::from(
                catch_unwind(AssertUnwindSafe(|| unsafe { &mut *context.cast::<F>() }()))
                    .unwrap_or(false),
            )
        }
        let mut handle = ptr::null_mut();
        let code = unsafe {
            self.0.peer_dial.unwrap()(
                self.0.context,
                destination.as_ptr().cast(),
                destination.len(),
                local.as_ptr().cast(),
                local.len(),
                maximum_frames,
                Some(current_callback::<F>),
                ptr::from_mut(&mut current).cast(),
                &mut handle,
            )
        };
        if code != 0 {
            return Err(Error::Operation);
        }
        unsafe { self.peer(handle) }
    }
}

/// Unique radio handle operated by one product worker.
pub struct Radio {
    services: HostServices,
    maximum_frames: usize,
    handle: NonNull<c_void>,
}
// SAFETY: the handle is uniquely owned and never used concurrently.
unsafe impl Send for Radio {}
impl Radio {
    /// Prepared native callback frame bound.
    pub fn maximum_frames(&self) -> usize {
        self.maximum_frames
    }
    /// Attach both stable endpoints and start the channel.
    ///
    /// # Safety
    /// Contexts and endpoint code remain live until this radio is dropped.
    /// The host serializes each endpoint and synchronously stops both on destroy.
    pub unsafe fn activate(
        &mut self,
        receive: abi::rptadv_radio_receive_v2,
        receive_context: *mut c_void,
        transmit: abi::rptadv_radio_transmit_v2,
        transmit_context: *mut c_void,
    ) -> Result<(), Error> {
        let code = unsafe {
            self.services.0.radio_activate.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                receive,
                receive_context,
                transmit,
                transmit_context,
            )
        };
        (code == 0).then_some(()).ok_or(Error::Operation)
    }
}
impl Drop for Radio {
    fn drop(&mut self) {
        unsafe {
            self.services.0.radio_destroy.unwrap()(self.services.0.context, self.handle.as_ptr());
        }
    }
}

/// Unique peer media/control handle operated by one reader.
pub struct PeerIo {
    services: HostServices,
    handle: NonNull<c_void>,
}
// SAFETY: the handle is uniquely moved to one owner thread.
unsafe impl Send for PeerIo {}
impl PeerIo {
    /// Negotiated signed-linear input/output rate.
    pub fn rate(&self) -> u32 {
        unsafe { self.services.0.peer_rate.unwrap()(self.services.0.context, self.handle.as_ptr()) }
    }
    /// Wait briefly for one input frame.
    pub fn ready(&mut self) -> Result<bool, Error> {
        match unsafe {
            self.services.0.peer_ready.unwrap()(self.services.0.context, self.handle.as_ptr())
        } {
            value if value < 0 => Err(Error::Hangup),
            value => Ok(value != 0),
        }
    }
    /// Read one frame and dispatch its borrowed canonical representation.
    pub fn read<F>(&mut self, mut dispatch: F) -> Result<(), Error>
    where
        F: FnMut(PeerInput<'_>),
    {
        struct State<'a, F> {
            dispatch: &'a mut F,
            failed: bool,
        }
        unsafe extern "C" fn callback<F: FnMut(PeerInput<'_>)>(
            context: *mut c_void,
            kind: u32,
            data: *const c_void,
            count: usize,
        ) {
            let state = unsafe { &mut *context.cast::<State<'_, F>>() };
            let result = catch_unwind(AssertUnwindSafe(|| match kind {
                1 if !data.is_null() => {
                    (state.dispatch)(PeerInput::Text(unsafe {
                        std::slice::from_raw_parts(data.cast(), count)
                    }));
                }
                2 if count == 1 && !data.is_null() => {
                    (state.dispatch)(PeerInput::Digit(unsafe { *data.cast::<u8>() } as char));
                }
                3 if !data.is_null() => {
                    (state.dispatch)(PeerInput::Audio(unsafe {
                        std::slice::from_raw_parts(data.cast(), count)
                    }));
                }
                _ => {}
            }));
            if result.is_err() {
                state.failed = true;
            }
        }
        let mut state = State {
            dispatch: &mut dispatch,
            failed: false,
        };
        let code = unsafe {
            self.services.0.peer_read.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                Some(callback::<F>),
                ptr::from_mut(&mut state).cast(),
            )
        };
        (code == 0 && !state.failed)
            .then_some(())
            .ok_or(Error::Hangup)
    }
    /// Send one NUL-free text payload.
    pub fn send_text(&mut self, text: &CStr) -> Result<(), Error> {
        let bytes = text.to_bytes();
        let code = unsafe {
            self.services.0.peer_send_text.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
            )
        };
        (code == 0).then_some(()).ok_or(Error::Write)
    }
    /// Send one conventional completed digit.
    pub fn send_digit(&mut self, digit: char) -> Result<(), Error> {
        let code = unsafe {
            self.services.0.peer_send_digit.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                digit as u8,
            )
        };
        (code == 0).then_some(()).ok_or(Error::Write)
    }
    /// Send codec-rate normalized F32 audio, or one end-of-burst marker when empty.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), Error> {
        let code = unsafe {
            self.services.0.peer_write.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                samples.as_ptr(),
                samples.len(),
            )
        };
        (code == 0).then_some(()).ok_or(Error::Write)
    }
}
impl Drop for PeerIo {
    fn drop(&mut self) {
        unsafe {
            self.services.0.peer_destroy.unwrap()(self.services.0.context, self.handle.as_ptr());
        }
    }
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
