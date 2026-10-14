//! Validated host-services clients; native handles never cross into core policy.

use crate::{
    Error, abi,
    directory::{Backend, BackendError, DirectoryResolver},
};
use rpt_advanced_core::schedule::{CivilTime, Weekday};
use std::{
    ffi::{CStr, c_void},
    mem::{MaybeUninit, offset_of, size_of},
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
    /// Counted missing source-rate samples, in the peer's negotiated PCM rate.
    Gap(usize),
    /// Remote radio asserted receive independently of media-frame arrival.
    RadioKey,
    /// Remote radio released receive.
    RadioUnkey,
}

/// Validated copy of the process-lifetime host capability.
#[derive(Clone, Copy)]
pub struct HostServices(abi::rptadv_host_services_v5);
// SAFETY: validation requires a process-lifetime immutable table. Opaque objects remain
// uniquely owned; the host documents independent operation on their owner threads.
unsafe impl Send for HostServices {}
// SAFETY: the immutable table and context may be copied; object operations remain serialized.
unsafe impl Sync for HostServices {}

#[derive(Default)]
struct DirectoryResults {
    values: Vec<String>,
    port: u16,
    valid: bool,
}
impl DirectoryResults {
    fn new() -> Self {
        Self {
            valid: true,
            ..Self::default()
        }
    }
    fn finish(self, code: i32) -> Result<Self, BackendError> {
        if !self.valid || code < 0 {
            Err(BackendError::Rejected)
        } else if code > 0 {
            Err(BackendError::Unavailable)
        } else {
            Ok(self)
        }
    }
    fn single(mut self) -> Result<Option<String>, BackendError> {
        if self.values.len() > 1 {
            Err(BackendError::Rejected)
        } else {
            Ok(self.values.pop())
        }
    }
}

unsafe extern "C" fn directory_text(
    context: *mut c_void,
    text: *const std::ffi::c_char,
    length: usize,
) {
    // SAFETY: callbacks borrow this exact stack-owned collector synchronously.
    let results = unsafe { &mut *context.cast::<DirectoryResults>() };
    if text.is_null() && length != 0 {
        results.valid = false;
        return;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) }
    };
    match std::str::from_utf8(bytes) {
        Ok(value) => results.values.push(value.to_owned()),
        Err(_) => results.valid = false,
    }
}
unsafe extern "C" fn directory_srv(
    context: *mut c_void,
    host: *const std::ffi::c_char,
    length: usize,
    port: u16,
) {
    unsafe { directory_text(context, host, length) };
    unsafe { (*context.cast::<DirectoryResults>()).port = port };
}

impl Backend for HostServices {
    fn record(&self, path: &str, node: &str) -> Result<Option<String>, BackendError> {
        let mut result = DirectoryResults::new();
        let code = unsafe {
            self.0.directory_record.unwrap()(
                self.0.context,
                path.as_ptr().cast(),
                path.len(),
                node.as_ptr().cast(),
                node.len(),
                Some(directory_text),
                ptr::from_mut(&mut result).cast(),
            )
        };
        result.finish(code)?.single()
    }
    fn srv(&self, service: &str) -> Result<Option<(String, u16)>, BackendError> {
        let mut result = DirectoryResults::new();
        let code = unsafe {
            self.0.directory_srv.unwrap()(
                self.0.context,
                service.as_ptr().cast(),
                service.len(),
                Some(directory_srv),
                ptr::from_mut(&mut result).cast(),
            )
        };
        let result = result.finish(code)?;
        let port = result.port;
        result.single().map(|host| host.map(|host| (host, port)))
    }
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<std::net::IpAddr>, BackendError> {
        let mut result = DirectoryResults::new();
        let code = unsafe {
            self.0.directory_addresses.unwrap()(
                self.0.context,
                host.as_ptr().cast(),
                host.len(),
                port,
                Some(directory_text),
                ptr::from_mut(&mut result).cast(),
            )
        };
        result
            .finish(code)?
            .values
            .iter()
            .map(|value| value.parse().map_err(|_| BackendError::Rejected))
            .collect()
    }
}

fn complete(api: &abi::rptadv_host_services_v5) -> bool {
    [
        api.local_time.is_some(),
        api.command_notice.is_some(),
        api.reaper_acquire.is_some(),
        api.reaper_release.is_some(),
        api.directory_record.is_some(),
        api.directory_srv.is_some(),
        api.directory_addresses.is_some(),
        api.directory_notice.is_some(),
        api.radio_open.is_some(),
        api.radio_activate.is_some(),
        api.radio_destroy.is_some(),
        api.peer_dial.is_some(),
        api.peer_bind_radio.is_some(),
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
    pub unsafe fn open(pointer: *const abi::rptadv_host_services_v5) -> Result<Self, Error> {
        if pointer.is_null() {
            return Err(Error::Admission);
        }
        let size = unsafe { ptr::addr_of!((*pointer).struct_size).read() } as usize;
        let prefix = offset_of!(abi::rptadv_host_services_v5, peer_step_attach);
        let async_end = offset_of!(abi::rptadv_host_services_v5, peer_observe);
        let observe_end = offset_of!(abi::rptadv_host_services_v5, peer_prepare_audio);
        let full = size_of::<abi::rptadv_host_services_v5>();
        if size < full && ![prefix, async_end, observe_end].contains(&size) {
            return Err(Error::Admission);
        }
        // A prefix-only host has no async tail. Never form a full-size reference to it.
        let mut copied = MaybeUninit::<abi::rptadv_host_services_v5>::zeroed();
        unsafe {
            ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                copied.as_mut_ptr().cast(),
                size.min(full),
            );
        }
        let api = unsafe { copied.assume_init() };
        let async_count = [
            api.peer_step_attach.is_some(),
            api.peer_step_stop.is_some(),
            api.peer_step_detach.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        let egress_count = [
            api.peer_prepare_audio.is_some(),
            api.peer_send_prepared.is_some(),
            api.peer_elapsed_ms.is_some(),
            api.peer_step_wake.is_some(),
            api.peer_set_deadline.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if api.abi_version != 5
            || api.capability != *b"rptadv.hst5\0"
            || !complete(&api)
            || !matches!(async_count, 0 | 3)
            || !matches!(egress_count, 0 | 5)
            || (egress_count == 5 && async_count != 3)
        {
            return Err(Error::Admission);
        }
        Ok(Self(api))
    }
    /// Whether all retained peer-owner operations were supplied by this host.
    pub fn peer_step_available(&self) -> bool {
        self.0.peer_step_attach.is_some()
    }
    /// Register one stable context while peer ownership remains with the product.
    ///
    /// # Safety
    /// `peer` is exclusively owned, and `step_context` remains live until the
    /// matching detach has quiesced every host callback.
    pub unsafe fn peer_step_attach(
        &self,
        peer: *mut c_void,
        step: abi::rptadv_peer_step_v1,
        step_context: *mut c_void,
    ) -> Result<NonNull<c_void>, Error> {
        let mut registration = ptr::null_mut();
        let code = unsafe {
            self.0.peer_step_attach.unwrap()(
                self.0.context,
                peer,
                step,
                step_context,
                &mut registration,
            )
        };
        (code == 0)
            .then(|| NonNull::new(registration))
            .flatten()
            .ok_or(Error::Operation)
    }
    /// Signal peer retirement without waiting for an in-progress step.
    pub fn peer_step_stop(&self, registration: NonNull<c_void>) {
        unsafe { self.0.peer_step_stop.unwrap()(self.0.context, registration.as_ptr()) };
    }
    /// Wait off-callback until no retained step can access product state.
    pub fn peer_step_detach(&self, registration: NonNull<c_void>) {
        unsafe { self.0.peer_step_detach.unwrap()(self.0.context, registration.as_ptr()) };
    }
    /// Publish a signal-only capability after registration succeeds. Its gate
    /// must close and quiesce before the registration can be detached.
    pub(crate) fn attach_wake(
        &self,
        wake: &crate::link::dispatcher::PeerWake,
        registration: NonNull<c_void>,
    ) {
        if let Some(call) = self.0.peer_step_wake {
            unsafe { wake.attach(call, self.0.context, registration.as_ptr()) };
        }
    }
    /// Copy optional peer metrics only from that peer's serialized owner turn.
    fn peer_observe(&self, peer: *mut c_void) -> Option<abi::rptadv_peer_observation_v1> {
        let observe = self.0.peer_observe?;
        // SAFETY: this C record contains only integer fields.
        let mut raw: abi::rptadv_peer_observation_v1 = unsafe { std::mem::zeroed() };
        raw.struct_size = size_of::<abi::rptadv_peer_observation_v1>() as u32;
        raw.abi_version = abi::RPTADV_PEER_OBSERVATION_ABI_VERSION;
        if unsafe { observe(self.0.context, peer, &mut raw) } != 0
            || raw.struct_size as usize != size_of::<abi::rptadv_peer_observation_v1>()
            || raw.abi_version != abi::RPTADV_PEER_OBSERVATION_ABI_VERSION
        {
            return None;
        }
        Some(raw)
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
    /// Apply shared directory policy over the selected host's I/O operations.
    pub fn lookup(
        &self,
        method: u32,
        static_file: &str,
        external_file: &str,
        remote: &str,
        source: Option<&str>,
    ) -> Result<String, Error> {
        let result = DirectoryResolver::new(*self, method, static_file, external_file)
            .lookup(remote, source.filter(|source| !source.is_empty()));
        match result {
            Ok(destination) if destination.len() <= 1024 => Ok(destination),
            Ok(_) => Err(Error::Operation),
            Err(error) => {
                unsafe { self.0.directory_notice.unwrap()(self.0.context, error as u32) };
                Err(Error::Operation)
            }
        }
    }
    /// Reserve one uniquely owned radio without starting callbacks. The host receives the node
    /// identity and adapter-selected channel separated by a NUL byte in the length-delimited name.
    pub fn radio(&self, node: &str, channel: &str, maximum_frames: usize) -> Result<Radio, Error> {
        let name = format!("{node}\0{channel}");
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
        transmit: abi::rptadv_radio_transmit_v3,
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
    /// Copy async registration inputs before callbacks can begin touching this peer.
    pub fn async_owner(&self) -> Option<(HostServices, *mut c_void)> {
        self.services
            .peer_step_available()
            .then_some((self.services, self.handle.as_ptr()))
    }
    /// Observe on the same exclusive owner that performs peer I/O.
    pub fn observe(&self) -> Option<abi::rptadv_peer_observation_v1> {
        self.services.peer_observe(self.handle.as_ptr())
    }
    /// Bind this exclusive peer to the existing radio before starting any media.
    pub fn bind_radio(&mut self, radio: &Radio) -> Result<(), Error> {
        // SAFETY: control borrows both live handles; the host retains neither handle.
        let result = unsafe {
            self.services.0.peer_bind_radio.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                radio.handle.as_ptr(),
            )
        };
        (result == 0).then_some(()).ok_or(Error::Operation)
    }

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
    /// Read one frame and report whether the host delivered a valid event.
    pub fn read<F>(&mut self, mut dispatch: F) -> Result<bool, Error>
    where
        F: FnMut(PeerInput<'_>),
    {
        struct State<'a, F> {
            dispatch: &'a mut F,
            failed: bool,
            seen: bool,
        }
        unsafe extern "C" fn callback<F: FnMut(PeerInput<'_>)>(
            context: *mut c_void,
            kind: u32,
            data: *const c_void,
            count: usize,
        ) {
            let state = unsafe { &mut *context.cast::<State<'_, F>>() };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let input = match kind {
                    abi::RPTADV_PEER_EVENT_TEXT if !data.is_null() => {
                        Some(PeerInput::Text(unsafe {
                            std::slice::from_raw_parts(data.cast(), count)
                        }))
                    }
                    abi::RPTADV_PEER_EVENT_DIGIT if count == 1 && !data.is_null() => {
                        Some(PeerInput::Digit(unsafe { *data.cast::<u8>() } as char))
                    }
                    abi::RPTADV_PEER_EVENT_AUDIO if !data.is_null() => {
                        Some(PeerInput::Audio(unsafe {
                            std::slice::from_raw_parts(data.cast(), count)
                        }))
                    }
                    abi::RPTADV_PEER_EVENT_GAP if data.is_null() && count != 0 => {
                        Some(PeerInput::Gap(count))
                    }
                    abi::RPTADV_PEER_EVENT_RADIO_KEY if data.is_null() && count == 0 => {
                        Some(PeerInput::RadioKey)
                    }
                    abi::RPTADV_PEER_EVENT_RADIO_UNKEY if data.is_null() && count == 0 => {
                        Some(PeerInput::RadioUnkey)
                    }
                    _ => None,
                };
                if let Some(input) = input {
                    (state.dispatch)(input);
                    true
                } else {
                    false
                }
            }));
            match result {
                Ok(seen) => state.seen |= seen,
                Err(_) => state.failed = true,
            }
        }
        let mut state = State {
            dispatch: &mut dispatch,
            failed: false,
            seen: false,
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
            .then_some(state.seen)
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
    /// Whether this host supports source-timed prepared media and readiness deadlines.
    pub(crate) fn prepared_media(&self) -> bool {
        self.services.0.peer_prepare_audio.is_some()
    }
    /// Encode one codec-rate block into caller-owned storage, without sending it.
    pub(crate) fn prepare_audio(
        &mut self,
        samples: &[f32],
        output: &mut [u8],
    ) -> Result<(), Error> {
        let code = unsafe {
            self.services.0.peer_prepare_audio.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                samples.as_ptr(),
                samples.len(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        (code == 0).then_some(()).ok_or(Error::Translation)
    }
    /// Submit retained bytes; false is socket congestion, not a failed connection.
    pub(crate) fn send_prepared(&mut self, bytes: &[u8], timestamp: u32) -> Result<bool, Error> {
        match unsafe {
            self.services.0.peer_send_prepared.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                timestamp,
            )
        } {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(Error::Write),
        }
    }
    /// Provider call-clock milliseconds, sampled only by the serialized peer owner.
    pub(crate) fn elapsed_ms(&self) -> u32 {
        unsafe {
            self.services.0.peer_elapsed_ms.unwrap()(self.services.0.context, self.handle.as_ptr())
        }
    }
    /// Publish the next relative wake; MAX clears the timer. PCM-only hosts ignore it.
    pub(crate) fn set_deadline(&mut self, after_ns: u64) -> Result<(), Error> {
        let Some(set) = self.services.0.peer_set_deadline else {
            return Ok(());
        };
        (unsafe { set(self.services.0.context, self.handle.as_ptr(), after_ns) } == 0)
            .then_some(())
            .ok_or(Error::Write)
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
