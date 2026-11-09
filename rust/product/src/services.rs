//! Validated host-services clients; native handles never cross into core policy.

use crate::{
    Error, abi,
    directory::{Backend, BackendError, DirectoryResolver},
};
use rpt_advanced_core::link::{PeerIdentity, PhoneAccountId, ProtocolId};
use rpt_advanced_core::schedule::{CivilTime, Weekday};
use std::{
    ffi::{CStr, c_void},
    mem::{offset_of, size_of, size_of_val},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
};

#[path = "services_remote.rs"]
mod remote;
pub use remote::RemoteTicket;

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

/// Borrowed provider-owned source input. Only the accepted prefix transfers.
pub enum SourceInput<'a> {
    /// Codec-rate PCM and its original position; codec PLC bypasses detection.
    Audio {
        /// Original source-rate position of the offered prefix.
        start: u64,
        /// Borrowed decoded PCM, retained by the provider until accepted.
        samples: &'a [f32],
        /// Codec-native PLC, never fresh in-band detector input.
        concealed: bool,
    },
    /// Unresolved counted loss in the same source timeline.
    Gap {
        /// Original source position of the missing prefix.
        start: u64,
        /// Offered source samples, not native output frames.
        count: usize,
    },
    /// Represented sender and literal body; never an ASL control envelope.
    Text {
        /// Represented sender; admission identity remains with the peer.
        sender: &'a str,
        /// Literal UTF-8 body, not a command or message template.
        body: &'a str,
        /// Fresh represented identity from the same read turn, zero if unresolved.
        represented_node_number: u32,
    },
}

/// Exact disposition of the previously accepted product-ring prefix.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum SourceCompletion {
    /// The product still owns accepted audio.
    #[default]
    Pending = 0,
    /// The exact finite token reached successful playout.
    Drained = 1,
    /// An explicit generation retirement discarded the accepted prefix.
    Cancelled = 2,
}

/// Copied provider progress and selected activity, separate from PCM occupancy.
pub struct SourceRead {
    /// 0 idle, 1 advanced, 2 blocked, 3 provider media drained.
    pub progress: u32,
    /// Selected key/unkey only; heartbeats and contenders are not activity.
    pub activity: Option<bool>,
    /// The provider needs a finite product-ring token, not an implicit flush.
    pub finish_pending: bool,
    /// Terminal state after both provider and product media drained.
    pub closed: bool,
}

/// Validated copy of the process-lifetime host capability.
#[derive(Clone, Copy)]
pub struct HostServices(abi::rptadv_host_services_v10);
// SAFETY: validation requires a process-lifetime immutable table. Opaque objects remain
// uniquely owned; the host documents independent operation on their owner threads.
unsafe impl Send for HostServices {}
// SAFETY: the immutable table and context may be copied; object operations remain serialized.
unsafe impl Sync for HostServices {}

/// Uniquely owned native-rate peer processor prepared by the selected host.
pub struct PcmProcessor {
    api: abi::rptadv_pcm_processor_v1,
    maximum: usize,
}
// SAFETY: the host transfers one owner; its process calls remain serial on the
// transmit worker, and destruction waits for that generation's quiescence.
unsafe impl Send for PcmProcessor {}

impl PcmProcessor {
    fn process(&mut self, samples: &mut [f32]) -> bool {
        if samples.len() > self.maximum {
            return false;
        }
        // SAFETY: preparation validates the function and retains its unique
        // context. The selected provider promises bounded native in-place work.
        unsafe {
            self.api.process.unwrap()(self.api.context, samples.as_mut_ptr(), samples.len()) == 0
        }
    }
}
impl Drop for PcmProcessor {
    fn drop(&mut self) {
        if let Some(destroy) = self.api.destroy.filter(|_| !self.api.context.is_null()) {
            // SAFETY: the unique transferred owner is destroyed once, including
            // rejected complete records, after all audio borrows are quiescent.
            unsafe { destroy(self.api.context) };
        }
    }
}

/// One generation-owned peer ring followed by its optional native-rate graph.
/// Local receive, telemetry and regeneration do not select this peer graph.
/// Current DTMF muting gates its output after stateful processing as well as the ring.
/// Idle native blocks advance only the graph with silence, retaining its gain history.
pub struct ProcessedPeer<P> {
    input: P,
    processor: Option<PcmProcessor>,
}
impl<P> ProcessedPeer<P> {
    /// Retain preallocated processing alongside this unique ring consumer.
    pub fn new(input: P, processor: Option<PcmProcessor>) -> Self {
        Self { input, processor }
    }
}
impl<P: rpt_advanced_core::link::PeerInput> rpt_advanced_core::link::PeerInput
    for ProcessedPeer<P>
{
    fn source_rate(&self) -> u32 {
        self.input.source_rate()
    }
    fn signals(&self) -> &rpt_advanced_core::link::PeerSignals {
        self.input.signals()
    }
    fn available(&self) -> u64 {
        self.input.available()
    }
    fn finite_playout(&self) -> Option<bool> {
        self.input.finite_playout()
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        match self.input.render(output) {
            Some(real)
                if self
                    .processor
                    .as_mut()
                    .is_none_or(|processor| processor.process(output)) =>
            {
                if self.input.signals().dtmf_muted() {
                    output.fill(0.0);
                }
                Some(real)
            }
            _ => {
                output.fill(0.0);
                None
            }
        }
    }
    fn silence(&mut self, scratch: &mut [f32]) {
        scratch.fill(0.0);
        if let Some(processor) = self.processor.as_mut() {
            let _ = processor.process(scratch);
        }
        scratch.fill(0.0);
    }
    fn render_sample(&mut self) -> Option<f32> {
        let mut sample = self.input.render_sample()?;
        if self
            .processor
            .as_mut()
            .is_none_or(|processor| processor.process(std::slice::from_mut(&mut sample)))
        {
            Some(if self.input.signals().dtmf_muted() {
                0.0
            } else {
                sample
            })
        } else {
            None
        }
    }
}

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

fn complete(api: &abi::rptadv_host_services_v10) -> bool {
    [
        api.local_time.is_some(),
        api.command_notice.is_some(),
        api.scheduler_notice.is_some(),
        api.reaper_acquire.is_some(),
        api.reaper_release.is_some(),
        api.directory_record.is_some(),
        api.directory_srv.is_some(),
        api.directory_addresses.is_some(),
        api.directory_notice.is_some(),
        api.radio_open.is_some(),
        api.radio_activate.is_some(),
        api.radio_destroy.is_some(),
        api.radio_site_bind.is_some(),
        api.radio_site_publish.is_some(),
        api.radio_site_snapshot.is_some(),
        api.peer_dial.is_some(),
        api.peer_bind_radio.is_some(),
        api.peer_rate.is_some(),
        api.peer_ready.is_some(),
        api.peer_read.is_some(),
        api.peer_send_text.is_some(),
        api.peer_send_digit.is_some(),
        api.peer_destroy.is_some(),
        api.peer_info.is_some(),
    ]
    .into_iter()
    .all(|present| present)
}

impl HostServices {
    /// Enabled remote rigs require the complete nonblocking lifecycle and
    /// callback-metadata bridge before any physical device can be reserved.
    pub fn remote_control_available(self) -> bool {
        self.0.radio_metadata_bind.is_some()
            && self.0.radio_service.is_some()
            && self.0.radio_quiesce.is_some()
            && self.0.radio_remote_submit.is_some()
            && self.0.radio_ready.is_some()
    }
    /// Validate the complete table before any callback or context is retained.
    ///
    /// # Safety
    /// `pointer` exposes a truthful readable size/version prefix and a process-lifetime
    /// immutable complete table. Its code/context remain live through product stop; context
    /// calls are thread-safe, while each returned handle permits exactly one serial owner.
    /// Callbacks are synchronous, do not retain borrowed buffers, provide aligned bounded
    /// slices, follow documented ownership/status values, and never unwind.
    pub unsafe fn open(pointer: *const abi::rptadv_host_services_v10) -> Result<Self, Error> {
        if pointer.is_null() {
            return Err(Error::Admission);
        }
        let size = unsafe { ptr::addr_of!((*pointer).struct_size).read() } as usize;
        let required = offset_of!(abi::rptadv_host_services_v10, peer_processing_prepare);
        let table_size = size_of::<abi::rptadv_host_services_v10>();
        let processor_tail = offset_of!(abi::rptadv_host_services_v10, radio_control);
        let maintenance_tail = offset_of!(abi::rptadv_host_services_v10, radio_update);
        let metadata_tail = offset_of!(abi::rptadv_host_services_v10, radio_metadata_bind);
        let remote_tail = offset_of!(abi::rptadv_host_services_v10, radio_service);
        let ready_tail = offset_of!(abi::rptadv_host_services_v10, radio_ready);
        if size < table_size
            && ![
                required,
                processor_tail,
                maintenance_tail,
                metadata_tail,
                remote_tail,
                ready_tail,
            ]
            .contains(&size)
        {
            return Err(Error::Admission);
        }
        // ABI 10's required prefix is complete; its optional processor tail is
        // copied only when fully readable. Missing tails remain nullable.
        let mut api: abi::rptadv_host_services_v10 = unsafe { std::mem::zeroed() };
        unsafe {
            ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                ptr::from_mut(&mut api).cast::<u8>(),
                size.min(table_size),
            )
        };
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
            api.peer_reset_encoder.is_some(),
            api.peer_send_prepared.is_some(),
            api.peer_elapsed_ms.is_some(),
            api.peer_step_wake.is_some(),
            api.peer_set_deadline.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if api.abi_version != 10
            || api.capability != *b"rptadv.hst10\0\0\0\0"
            || !complete(&api)
            || !matches!(async_count, 0 | 3)
            || !matches!(egress_count, 0 | 6)
            || (egress_count == 6 && async_count != 3)
            || (egress_count == 0 && api.peer_write.is_none())
            || (api.peer_read_source.is_some() && egress_count != 6)
        {
            return Err(Error::Admission);
        }
        Ok(Self(api))
    }
    /// Prepare one native-rate peer processor on control; absent hosts supply upstream DSP.
    pub fn prepare_peer_processing(
        &self,
        configuration: &str,
        local: &str,
        maximum: usize,
    ) -> Result<Option<PcmProcessor>, Error> {
        let Some(prepare) = self.0.peer_processing_prepare else {
            return Ok(None);
        };
        let mut api = abi::rptadv_pcm_processor_v1 {
            struct_size: size_of::<abi::rptadv_pcm_processor_v1>() as u32,
            abi_version: 1,
            context: ptr::null_mut(),
            process: None,
            destroy: None,
        };
        let code = unsafe {
            prepare(
                self.0.context,
                configuration.as_ptr().cast(),
                configuration.len(),
                local.as_ptr().cast(),
                local.len(),
                maximum,
                &mut api,
            )
        };
        match code {
            1 => Ok(None),
            0 => {
                let processor = PcmProcessor { api, maximum };
                (processor.api.struct_size as usize >= size_of::<abi::rptadv_pcm_processor_v1>()
                    && processor.api.abi_version == 1
                    && !processor.api.context.is_null()
                    && processor.api.process.is_some()
                    && processor.api.destroy.is_some())
                .then_some(Some(processor))
                .ok_or(Error::Operation)
            }
            _ => Err(Error::Operation),
        }
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
    /// Forward a borrowed scheduler warning on the non-audio control path.
    pub fn scheduler_notice(&self, text: &str) {
        unsafe {
            self.0.scheduler_notice.unwrap()(self.0.context, text.as_ptr().cast(), text.len());
        }
    }
    /// Query optional account diagnostics on control, even with no live peers.
    /// A failed query is unavailable evidence, not proof that an account is offline.
    pub fn echo_account_status(&self, local: &str) -> Result<Option<String>, BackendError> {
        let Some(call) = self.0.echo_account_status else {
            return Ok(None);
        };
        let mut result = DirectoryResults::new();
        let code = unsafe {
            call(
                self.0.context,
                local.as_ptr().cast(),
                local.len(),
                Some(directory_text),
                ptr::from_mut(&mut result).cast(),
            )
        };
        result.finish(code)?.single()
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
    /// Validate copied metadata, then adopt one peer from the host callback.
    /// On error ownership stays with the caller; outbound dial destroys its own rejected handle.
    ///
    /// # Safety
    /// `handle` must be a unique live peer created by this same services table.
    pub unsafe fn peer(&self, handle: *mut c_void) -> Result<PeerIo, Error> {
        let handle = NonNull::new(handle).ok_or(Error::Admission)?;
        let metadata = unsafe { self.inspect_peer(handle.as_ptr()) }?;
        Ok(PeerIo {
            services: *self,
            handle,
            identity: metadata.identity,
            local: metadata.local,
            callsign: metadata.callsign,
            capabilities: metadata.capabilities,
            account_generation: metadata.account_generation,
            qso_generation: metadata.qso_generation,
            provider_account: metadata.provider_account,
        })
    }
    /// Copy admission metadata while borrowing a pending peer; never adopt or destroy it.
    ///
    /// # Safety
    /// `handle` is a live peer from this table, exclusively borrowed during the callback.
    pub unsafe fn inspect_peer(&self, handle: *const c_void) -> Result<PeerMetadata, Error> {
        let handle = NonNull::new(handle.cast_mut()).ok_or(Error::Admission)?;
        let mut raw: abi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
        raw.struct_size = size_of_val(&raw) as u32;
        raw.abi_version = 1;
        if unsafe { self.0.peer_info.unwrap()(self.0.context, handle.as_ptr(), &mut raw) } != 0
            || raw.struct_size < offset_of!(abi::rptadv_peer_info_v1, provider_account) as u32
            || raw.abi_version != 1
        {
            return Err(Error::Admission);
        }
        let protocol = match raw.protocol {
            1 => ProtocolId::AllStarLink,
            2 => ProtocolId::EchoLink,
            3 => ProtocolId::Phone,
            _ => return Err(Error::Admission),
        };
        let bytes = |span: abi::rptadv_peer_bytes_v1, maximum: usize| {
            if span.length > maximum || (span.data.is_null() && span.length != 0) {
                return Err(Error::Admission);
            }
            let data = if span.length == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(span.data, span.length) }
            };
            std::str::from_utf8(data)
                .map(str::to_owned)
                .map_err(|_| Error::Admission)
        };
        let remote = bytes(raw.remote, 32)?;
        let identity = PeerIdentity::parse(protocol, &remote).map_err(|_| Error::Admission)?;
        let local = bytes(
            raw.local,
            if protocol == ProtocolId::Phone {
                63
            } else {
                32
            },
        )?;
        let local_valid = if protocol == ProtocolId::Phone {
            rpt_advanced_core::config::NodeId::new(local.as_str()).is_ok()
        } else {
            !local.is_empty() && local.bytes().all(|byte| byte.is_ascii_digit())
        };
        let callsign = bytes(raw.callsign, 64)?;
        let capabilities = raw.capabilities;
        let provider_account = if protocol == ProtocolId::Phone {
            if raw.struct_size < size_of_val(&raw) as u32 {
                return Err(Error::Admission);
            }
            Some(
                PhoneAccountId::new(&bytes(raw.provider_account, 64)?)
                    .map_err(|_| Error::Admission)?,
            )
        } else {
            None
        };
        if identity.wire_identity() != remote
            || !local_valid
            || capabilities & !63 != 0
            || (protocol != ProtocolId::Phone && capabilities & 32 != 0)
            || (protocol == ProtocolId::Phone
                && (capabilities & 32 == 0
                    || capabilities & !35 != 0
                    || raw.account_generation == 0
                    || raw.qso_generation == 0))
            || (capabilities & 1 != 0 && !self.peer_step_available())
            || (capabilities & 2 != 0
                && (capabilities & 1 == 0 || self.0.peer_prepare_audio.is_none()))
            || (capabilities & 2 == 0 && self.0.peer_write.is_none())
            || (capabilities & 16 != 0
                && (protocol != ProtocolId::EchoLink
                    || self.0.peer_read_source.is_none()
                    || capabilities & 3 != 3))
            || (protocol == ProtocolId::EchoLink
                && capabilities & 4 != 0
                && self.0.peer_send_human_text.is_none())
            || !callsign
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-/*_".contains(&byte))
            || (protocol == ProtocolId::EchoLink
                && (raw.account_generation == 0
                    || raw.qso_generation == 0
                    || callsign.is_empty()
                    || capabilities & 8 != 0
                    || capabilities & 19 != 19))
        {
            return Err(Error::Admission);
        }
        Ok(PeerMetadata {
            identity,
            local,
            callsign,
            capabilities,
            account_generation: raw.account_generation,
            qso_generation: raw.qso_generation,
            provider_account,
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
        match unsafe { self.peer(handle) } {
            Ok(peer) => Ok(peer),
            Err(error) => {
                if !handle.is_null() {
                    unsafe { self.0.peer_destroy.unwrap()(self.0.context, handle) };
                }
                Err(error)
            }
        }
    }
}

/// Resolved speech admission and reserve required by the existing local receive ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReceivePolicy {
    pub(crate) mode: u32,
    pub(crate) preroll_frames: u32,
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
    /// Read the configured receive reserve before activation or after a live edit.
    pub(crate) fn receive_policy(&mut self) -> Result<ReceivePolicy, i32> {
        let call = self.services.0.radio_control.ok_or(-7)?;
        let mut request = abi::rptadv_radio_control_v1 {
            struct_size: size_of::<abi::rptadv_radio_control_v1>() as u32,
            abi_version: 1,
            operation: 7,
            keyed: 0,
            forced_ctcss_tenths_hz: 0,
            command: abi::UrpAstChannelCommand {
                struct_size: size_of::<abi::UrpAstChannelCommand>() as u32,
                abi_version: 1,
                command: abi::URP_AST_COMMAND_GET_RECEIVE_POLICY,
                target: 0,
                value: -1,
                flags: u32::MAX,
                eeprom_words: [0; 64],
            },
        };
        // SAFETY: the unique radio and complete writable request outlive this serial query.
        let status = unsafe {
            call(
                self.services.0.context,
                self.handle.as_ptr(),
                &mut request,
                None,
                ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(status);
        }
        if request.struct_size as usize != size_of_val(&request)
            || request.abi_version != 1
            || request.operation != 7
            || request.command.struct_size as usize != size_of_val(&request.command)
            || request.command.abi_version != 1
            || request.command.command != abi::URP_AST_COMMAND_GET_RECEIVE_POLICY
            || request.command.target != 0
            || !matches!(
                (request.command.value, request.command.flags),
                (0, 0) | (1 | 2, 7680)
            )
        {
            return Err(-1);
        }
        Ok(ReceivePolicy {
            mode: request.command.value as u32,
            preroll_frames: request.command.flags,
        })
    }
    /// Stage, commit, or restore a same-device edit on the retained native owner.
    pub(crate) fn configuration_update(&mut self, phase: u32) -> Result<bool, i32> {
        let Some(update) = self.services.0.radio_update else {
            return if phase == 0 { Ok(false) } else { Err(-7) };
        };
        // SAFETY: the unique radio lease serializes control and destruction.
        match unsafe { update(self.services.0.context, self.handle.as_ptr(), phase) } {
            0 => Ok(true),
            1 if phase == 0 => Ok(false),
            code => Err(code),
        }
    }

    /// Dispatch bounded physical maintenance through the existing host radio owner.
    pub(crate) fn maintenance(
        &mut self,
        request: &mut abi::rptadv_radio_control_v1,
    ) -> Result<String, i32> {
        unsafe extern "C" fn append(
            context: *mut c_void,
            text: *const std::ffi::c_char,
            length: usize,
        ) {
            // SAFETY: the synchronous caller retains this output and validates borrowed UTF-8.
            let output = unsafe { &mut *context.cast::<Result<String, i32>>() };
            if text.is_null() && length != 0 {
                *output = Err(-1);
                return;
            }
            let bytes = if length == 0 {
                &[]
            } else {
                // SAFETY: a conforming host advertises a complete borrowed text span.
                unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) }
            };
            match std::str::from_utf8(bytes) {
                Ok(text) if text.len() <= 65536 => {
                    if let Ok(value) = output {
                        if value.len().saturating_add(text.len()) <= 65536 {
                            value.push_str(text);
                        } else {
                            *output = Err(-1);
                        }
                    }
                }
                _ => *output = Err(-1),
            }
        }
        let call = self.services.0.radio_control.ok_or(-7)?;
        let mut output = Ok(String::new());
        // SAFETY: this unique radio lease serializes all maintenance and destruction.
        let code = unsafe {
            call(
                self.services.0.context,
                self.handle.as_ptr(),
                request,
                Some(append),
                ptr::from_mut(&mut output).cast(),
            )
        };
        if code != 0 {
            return Err(code);
        }
        output
    }

    /// Replace site output claims on this radio's existing owner, off audio callbacks.
    /// Zero accepts a request; the snapshot's serviced generation confirms application.
    pub fn site_bind(&mut self, bank: u32, binding: &abi::UrpAstSiteBinding) -> Result<(), i32> {
        let code = unsafe {
            self.services.0.radio_site_bind.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                bank,
                binding,
            )
        };
        (code == 0).then_some(()).ok_or(code)
    }

    /// Submit logical levels through the current site binding, preserving provider errors.
    pub fn site_publish(&mut self, bank: u32, request: &abi::UrpAstSiteRequest) -> Result<(), i32> {
        let code = unsafe {
            self.services.0.radio_site_publish.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                bank,
                request,
            )
        };
        (code == 0).then_some(()).ok_or(code)
    }

    /// Copy site observations on the control path. Input age and physical readback
    /// remain provider observations, distinct from requested or serviced output levels.
    pub fn site_snapshot(&mut self, bank: u32) -> Result<abi::UrpAstSiteSnapshot, i32> {
        let mut snapshot: abi::UrpAstSiteSnapshot = unsafe { std::mem::zeroed() };
        snapshot.struct_size = size_of::<abi::UrpAstSiteSnapshot>() as u32;
        snapshot.abi_version = 1;
        let code = unsafe {
            self.services.0.radio_site_snapshot.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                bank,
                &mut snapshot,
            )
        };
        if code != 0 {
            return Err(code);
        }
        if snapshot.struct_size < size_of::<abi::UrpAstSiteSnapshot>() as u32
            || snapshot.abi_version != 1
        {
            return Err(-1);
        }
        Ok(snapshot)
    }
    /// Prepared native callback frame bound.
    pub fn maximum_frames(&self) -> usize {
        self.maximum_frames
    }
    /// Bind optional metadata on control before callback activation. An old host
    /// may omit this tail for ordinary radios; enabled remote radios require it.
    pub fn bind_metadata(&mut self, metadata: abi::rptadv_radio_metadata_v1) -> Result<(), Error> {
        let Some(bind) = self.services.0.radio_metadata_bind else {
            return Ok(());
        };
        // SAFETY: the unique radio lease serializes this pre-activation call.
        let code = unsafe { bind(self.services.0.context, self.handle.as_ptr(), metadata) };
        (code == 0).then_some(()).ok_or(Error::Operation)
    }
    /// Attach both stable endpoints and start the channel.
    ///
    /// # Safety
    /// Contexts and endpoint code remain live until this radio is dropped.
    /// The host serializes each endpoint and synchronously stops both on destroy.
    pub unsafe fn activate(
        &mut self,
        receive: abi::rptadv_radio_receive_v3,
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

/// Immutable admission metadata copied without adopting a provider's pending handle.
pub struct PeerMetadata {
    identity: PeerIdentity,
    local: String,
    callsign: String,
    capabilities: u32,
    account_generation: u64,
    qso_generation: u64,
    provider_account: Option<PhoneAccountId>,
}
impl PeerMetadata {
    /// Real protocol-qualified address, without trusting a request's claimed origin.
    pub fn identity(&self) -> &PeerIdentity {
        &self.identity
    }
    /// Provider-bound local controller target.
    pub fn local(&self) -> &str {
        &self.local
    }
}

/// Unique peer media/control handle operated by one reader.
pub struct PeerIo {
    services: HostServices,
    handle: NonNull<c_void>,
    identity: PeerIdentity,
    local: String,
    callsign: String,
    capabilities: u32,
    account_generation: u64,
    qso_generation: u64,
    provider_account: Option<PhoneAccountId>,
}
// SAFETY: the handle is uniquely moved to one owner thread.
unsafe impl Send for PeerIo {}
impl PeerIo {
    /// Canonical protocol-qualified identity copied before taking ownership.
    pub fn identity(&self) -> &PeerIdentity {
        &self.identity
    }
    /// Owning local controller node, copied before the host callback returns.
    pub fn local(&self) -> &str {
        &self.local
    }
    /// Validated callsign, empty when the IAX transport has no callsign metadata.
    pub fn callsign(&self) -> &str {
        &self.callsign
    }
    /// Admission generations copied from the provider-bound pending QSO.
    pub fn generations(&self) -> (u64, u64) {
        (self.account_generation, self.qso_generation)
    }
    /// Copied local provider account; absent for conventional node peers.
    pub fn phone_account(&self) -> Option<&PhoneAccountId> {
        self.provider_account.as_ref()
    }
    /// Human-text routing support belongs to this connection, not the host table.
    pub fn can_forward_text(&self) -> bool {
        self.capabilities & 4 != 0
    }
    /// Copy async registration inputs before callbacks can begin touching this peer.
    pub fn async_owner(&self) -> Option<(HostServices, *mut c_void)> {
        (self.capabilities & 1 != 0).then_some((self.services, self.handle.as_ptr()))
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
    /// Whether this peer retains unaccepted source prefixes and finite-boundary state.
    pub fn source_media(&self) -> bool {
        self.capabilities & 16 != 0
    }

    /// Read bounded provider-owned input; never copy or consume its rejected suffix.
    pub fn read_source<F>(
        &mut self,
        completion: SourceCompletion,
        dispatch: F,
    ) -> Result<SourceRead, Error>
    where
        F: FnMut(SourceInput<'_>) -> usize,
    {
        if !self.source_media() {
            return Err(Error::Operation);
        }
        struct State<F> {
            dispatch: F,
            failed: bool,
            completing: bool,
            text: Option<(String, String)>,
        }
        fn accept<F: FnMut(SourceInput<'_>) -> usize>(
            state: &mut State<F>,
            input: SourceInput<'_>,
            maximum: usize,
        ) -> usize {
            // A prior burst's completion cannot acknowledge samples offered in
            // this same turn. The provider retains them for the next read.
            if state.completing && !matches!(&input, SourceInput::Text { .. }) {
                return 0;
            }
            match catch_unwind(AssertUnwindSafe(|| (state.dispatch)(input))) {
                Ok(count) if count <= maximum => count,
                _ => {
                    state.failed = true;
                    0
                }
            }
        }
        unsafe extern "C" fn audio<F: FnMut(SourceInput<'_>) -> usize>(
            context: *mut c_void,
            start: u64,
            data: *const f32,
            count: usize,
            concealed: u32,
        ) -> usize {
            let state = unsafe { &mut *context.cast::<State<F>>() };
            if count > 4096
                || (data.is_null() && count != 0)
                || concealed > 1
                || start.checked_add(count as u64).is_none()
            {
                state.failed = true;
                return 0;
            }
            let samples = if count == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(data, count) }
            };
            accept(
                state,
                SourceInput::Audio {
                    start,
                    samples,
                    concealed: concealed == 1,
                },
                count,
            )
        }
        unsafe extern "C" fn gap<F: FnMut(SourceInput<'_>) -> usize>(
            context: *mut c_void,
            start: u64,
            count: u32,
        ) -> u32 {
            let state = unsafe { &mut *context.cast::<State<F>>() };
            if start.checked_add(u64::from(count)).is_none() {
                state.failed = true;
                return 0;
            }
            accept(
                state,
                SourceInput::Gap {
                    start,
                    count: count as usize,
                },
                count as usize,
            ) as u32
        }
        unsafe extern "C" fn text<F: FnMut(SourceInput<'_>) -> usize>(
            context: *mut c_void,
            sender: abi::rptadv_peer_bytes_v1,
            body: abi::rptadv_peer_bytes_v1,
        ) {
            let state = unsafe { &mut *context.cast::<State<F>>() };
            let bytes = |span: abi::rptadv_peer_bytes_v1, maximum: usize| {
                if span.length > maximum || span.length == 0 || span.data.is_null() {
                    return None;
                }
                std::str::from_utf8(unsafe { std::slice::from_raw_parts(span.data, span.length) })
                    .ok()
            };
            let (Some(sender), Some(body)) = (bytes(sender, 64), bytes(body, 4096)) else {
                state.failed = true;
                return;
            };
            if state.text.is_some() {
                state.failed = true;
            } else {
                state.text = Some((sender.to_owned(), body.to_owned()));
            }
        }
        let mut state = State {
            dispatch,
            failed: false,
            completing: completion != SourceCompletion::Pending,
            text: None,
        };
        let context = ptr::from_mut(&mut state).cast();
        let sink = abi::rptadv_peer_source_sink_v1 {
            struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
            abi_version: 1,
            context,
            audio: Some(audio::<F>),
            gap: Some(gap::<F>),
        };
        let mut result: abi::rptadv_peer_source_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of_val(&result) as u32;
        result.abi_version = 1;
        let code = unsafe {
            self.services.0.peer_read_source.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                completion as u32,
                &sink,
                Some(text::<F>),
                context,
                &mut result,
            )
        };
        if code != 0
            || state.failed
            || result.struct_size < size_of_val(&result) as u32
            || result.abi_version != 1
            || result.progress > 3
            || result.activity > 2
            || result.finish_pending > 1
            || result.state > 3
            || (result.represented_node_number != 0 && state.text.is_none())
        {
            return Err(Error::InvalidFrame);
        }
        if let Some((sender, body)) = state.text.take() {
            accept(
                &mut state,
                SourceInput::Text {
                    sender: &sender,
                    body: &body,
                    represented_node_number: result.represented_node_number,
                },
                usize::MAX,
            );
        }
        if state.failed {
            return Err(Error::InvalidFrame);
        }
        Ok(SourceRead {
            progress: result.progress,
            activity: match result.activity {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            finish_pending: result.finish_pending == 1,
            closed: result.state == 3,
        })
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
    /// Send structured human content only through a capable EchoLink connection.
    pub fn send_human_text(&mut self, sender: &str, body: &str) -> Result<(), Error> {
        let operation = self
            .services
            .0
            .peer_send_human_text
            .filter(|_| self.identity.protocol() == ProtocolId::EchoLink && self.can_forward_text())
            .ok_or(Error::Operation)?;
        let bytes = |value: &str| abi::rptadv_peer_bytes_v1 {
            data: value.as_ptr(),
            length: value.len(),
        };
        let code = unsafe {
            operation(
                self.services.0.context,
                self.handle.as_ptr(),
                bytes(sender),
                bytes(body),
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
        self.capabilities & 2 != 0
    }
    /// Feed one retained encoder, reporting consumption and output independently.
    pub(crate) fn prepare_audio(
        &mut self,
        samples: &[f32],
        output: &mut [u8],
        flush: bool,
    ) -> Result<abi::rptadv_codec_encode_result_v1, Error> {
        let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of_val(&result) as u32;
        let code = unsafe {
            self.services.0.peer_prepare_audio.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                samples.as_ptr(),
                samples.len(),
                output.as_mut_ptr(),
                output.len(),
                u32::from(flush),
                &mut result,
            )
        };
        if !(0..=2).contains(&code)
            || result.status != code as u32
            || result.struct_size < size_of_val(&result) as u32
            || result.consumed_frames > samples.len()
            || result.encoded_bytes > output.len()
            || result.source_frames > result.packet_frames
            || (result.encoded_bytes != 0 && result.packet_frames == 0)
        {
            return Err(Error::Translation);
        }
        Ok(result)
    }
    /// Reset discarded/expired PCM before accepting another source span.
    pub(crate) fn reset_encoder(&mut self) -> Result<(), Error> {
        (unsafe {
            self.services.0.peer_reset_encoder.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
            )
        } == 0)
            .then_some(())
            .ok_or(Error::Translation)
    }
    /// Submit retained bytes; false is socket congestion, not a failed connection.
    /// Empty bytes submit the timestamped end-of-burst marker with the same contract.
    pub(crate) fn send_prepared(
        &mut self,
        bytes: &[u8],
        frames: u32,
        timestamp: u32,
    ) -> Result<bool, Error> {
        match unsafe {
            self.services.0.peer_send_prepared.unwrap()(
                self.services.0.context,
                self.handle.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                frames,
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
        if self.capabilities & 1 == 0 {
            return Ok(());
        }
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
