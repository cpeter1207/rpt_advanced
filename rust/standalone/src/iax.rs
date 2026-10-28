//! Retained provider catalog and generated ABI boundary to standalone IAX2.

pub(crate) mod executor;

use crate::{abi, secrets::SecretsFile};
use std::{
    ffi::{CString, c_void},
    mem::size_of,
    net::SocketAddr,
    path::Path,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
};

const ABI_VERSION: u32 = 2;
const CAPABILITY: [u8; 16] = *b"rptadv.iax2.v2\0\0";
const SERVER_CAPABILITY: [u8; 16] = *b"rptadv.iaxsrv2\0\0";
static NEXT_CALL_NUMBER: AtomicU16 = AtomicU16::new(1);

type DialOptions = abi::rptadv_iax2_dial_options_v2;
type ClientDescriptor = abi::rptadv_iax2_client_descriptor_v2;
type ServerOptions = abi::rptadv_iax2_server_options_v2;
type ServerDescriptor = abi::rptadv_iax2_server_descriptor_v2;

#[repr(C)]
struct DescriptorHeader {
    struct_size: u32,
    abi_version: u32,
    capability: [u8; 16],
}

/// C callback used by the IAX listener to apply product inbound policy.
pub type ServerAuthorize = abi::rptadv_iax2_authorize_v1;
/// C callback used by the IAX listener to transfer one accepted peer to the product.
pub type ServerAccept = abi::rptadv_iax2_accept_v1;

/// Safe, non-secret detail for an IAX2 adapter failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IaxError {
    /// The library could not be loaded.
    Load,
    /// The required descriptor symbol is missing.
    MissingSymbol,
    /// The descriptor does not match ABI 2.
    IncompatibleAdapter,
    /// The call setup failed or returned no peer.
    Dial,
    /// The adapter rejected invalid dial parameters.
    InvalidParameters,
    /// The network adapter could not exchange datagrams.
    Network,
    /// The remote did not complete setup within the dial deadline.
    Timeout,
    /// The remote rejected the call.
    Rejected,
    /// The peer could not negotiate a supported codec.
    Codec,
    /// Call setup encountered an invalid protocol exchange.
    Protocol,
    /// The inbound UDP listener could not bind or returned no handle.
    Bind,
    /// A peer operation failed or returned malformed output.
    Operation,
}

impl std::fmt::Display for IaxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Load => "cannot load IAX2 client library",
            Self::MissingSymbol => "IAX2 client descriptor is missing",
            Self::IncompatibleAdapter => "IAX2 client ABI is incompatible",
            Self::Dial => "IAX2 call setup failed",
            Self::InvalidParameters => "IAX2 invalid dial parameters",
            Self::Network => "IAX2 network failure",
            Self::Timeout => "IAX2 call setup timed out",
            Self::Rejected => "IAX2 call rejected",
            Self::Codec => "IAX2 codec negotiation failed",
            Self::Protocol => "IAX2 protocol failure",
            Self::Bind => "cannot bind the IAX2 listener",
            Self::Operation => "IAX2 peer operation failed",
        })
    }
}

impl std::error::Error for IaxError {}

/// One event copied into buffers owned by the serialized peer owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IaxEvent {
    /// Retained native decode/PLC needs this many PCM frames; retry without loss.
    NeedCapacity(usize),
    /// No datagram or user event is available.
    None,
    /// Decoded mono F32 ULAW samples were written to the supplied sample buffer.
    Audio(usize),
    /// Counted missing input-rate samples; the shared PCM ring performs PLC in order.
    Gap(usize),
    /// ASL text bytes were written to the supplied text buffer.
    Text(usize),
    /// The remote peer ended the call.
    Hangup,
    /// One acknowledged remote DTMF digit was written to the text buffer.
    Digit(u8),
    /// Remote radio asserted receive independently of media-frame arrival.
    RadioKey,
    /// Remote radio released receive.
    RadioUnkey,
}

/// One process-lifetime dynamically loaded IAX2 client descriptor.
#[derive(Clone)]
pub struct IaxClient(Arc<IaxOwner>);

struct IaxOwner {
    library: Option<libloading::Library>,
    api: ClientDescriptor,
    catalog: NonNull<c_void>,
}
// SAFETY: the catalog is immutable; the provider retains independent state for
// each dialed/listening peer. Loaded code remains live through the final owner.
unsafe impl Send for IaxOwner {}
// SAFETY: no mutable peer handle is stored here, only the shared catalog/table.
unsafe impl Sync for IaxOwner {}
impl Drop for IaxOwner {
    fn drop(&mut self) {
        unsafe { self.api.close_catalog.unwrap()(self.catalog.as_ptr()) };
    }
}

// Composition manifest: neutral profiles only; wire IDs belong to the IAX DSO.
// iLBC's IAX mapping selects mode 30; no inferred or per-peer mode switching.
const CODEC_PROVIDERS: [(u32, &str); 16] = [
    (1, "g711"),
    (2, "g711"),
    (3, "gsm"),
    (4, "ffmpeg"),
    (5, "spandsp"),
    (6, "dialogic"),
    (7, "pcm"),
    (8, "spandsp"),
    (9, "bcg729"),
    (10, "speex"),
    (11, "ilbc"),
    (12, "spandsp"),
    (13, "spandsp"),
    (14, "pcm"),
    (15, "speex"),
    (16, "opus"),
];

fn open_catalog(api: &ClientDescriptor) -> Result<NonNull<c_void>, IaxError> {
    let paths: Vec<_> = CODEC_PROVIDERS
        .iter()
        .map(|(_, name)| {
            CString::new(format!("librptadv_codec_{name}.so.1")).expect("static DSO name")
        })
        .collect();
    let selections: Vec<_> = CODEC_PROVIDERS
        .iter()
        .zip(&paths)
        .map(|((id, _), path)| abi::rptadv_iax2_codec_selection_v2 {
            struct_size: size_of::<abi::rptadv_iax2_codec_selection_v2>() as u32,
            profile_id: *id,
            provider_path: path.as_ptr(),
        })
        .collect();
    let mut catalog = std::ptr::null_mut();
    if unsafe { api.open_catalog.unwrap()(selections.as_ptr(), selections.len(), &mut catalog) }
        != 0
    {
        return Err(IaxError::Codec);
    }
    NonNull::new(catalog).ok_or(IaxError::Codec)
}

impl IaxClient {
    /// Load one versioned IAX2 client shared object and validate its complete ABI.
    pub fn load(path: &Path) -> Result<Self, IaxError> {
        // SAFETY: the selected path is an administrator-provided trusted runtime dependency.
        let library = unsafe { libloading::Library::new(path) }.map_err(|_| IaxError::Load)?;
        // SAFETY: the library remains owned by `Self` for the descriptor's full lifetime.
        let descriptor = unsafe {
            let symbol: libloading::Symbol<unsafe extern "C" fn() -> *const ClientDescriptor> =
                library
                    .get(b"rptadv_iax2_client_descriptor_v2\0")
                    .map_err(|_| IaxError::MissingSymbol)?;
            symbol()
        };
        let api = checked_descriptor(descriptor)?;
        let catalog = open_catalog(&api)?;
        Ok(Self(Arc::new(IaxOwner {
            library: Some(library),
            api,
            catalog,
        })))
    }

    /// Establish one outbound call using the retained bidirectional catalog.
    pub fn dial(
        self,
        remote: SocketAddr,
        local_node: &str,
        remote_node: &str,
        secret: &str,
        timeout_ms: u32,
    ) -> Result<IaxPeer, IaxError> {
        let handle = dial_handle(
            &self.0.api,
            self.0.catalog.as_ptr(),
            remote,
            local_node,
            remote_node,
            secret,
            timeout_ms,
        )?;
        Ok(IaxPeer {
            api: self.0.api,
            _owner: self,
            handle,
        })
    }

    /// Dial using the local node's separate secrets-file entry or empty-secret fallback.
    pub fn dial_configured(
        self,
        remote: SocketAddr,
        local_node: &str,
        remote_node: &str,
        secrets: &SecretsFile,
        timeout_ms: u32,
    ) -> Result<IaxPeer, IaxError> {
        let secret = configured_secret(secrets, local_node);
        self.dial(remote, local_node, remote_node, secret, timeout_ms)
    }

    /// Adopt one peer handle returned by the matching inbound server descriptor.
    ///
    /// # Safety
    /// `handle` must be a unique live IaxPeer handle from the same library ABI and is transferred
    /// to this wrapper exactly once.
    pub unsafe fn adopt_inbound(self, handle: *mut c_void) -> Result<IaxPeer, IaxError> {
        let handle = NonNull::new(handle).ok_or(IaxError::Operation)?;
        Ok(IaxPeer {
            api: self.0.api,
            _owner: self,
            handle,
        })
    }
}

/// One dynamically loaded, single-socket inbound IAX server for configured local nodes.
pub struct IaxServer {
    _owner: IaxClient,
    api: ServerDescriptor,
    handle: NonNull<c_void>,
}

impl IaxServer {
    /// Route socket readiness through the shared retained Tokio executor.
    pub(crate) fn enable_async(&mut self, executor: &executor::Executor) -> Result<(), IaxError> {
        unsafe { executor.enable_server(self.handle.as_ptr()) }.map_err(|_| IaxError::Operation)
    }

    /// Bind one numeric UDP endpoint and route NEW calls to any configured local node.
    pub fn bind(
        client: &IaxClient,
        address: SocketAddr,
        local_nodes: &[String],
    ) -> Result<Self, IaxError> {
        let library = client.0.library.as_ref().ok_or(IaxError::MissingSymbol)?;
        let descriptor = unsafe {
            let symbol: libloading::Symbol<unsafe extern "C" fn() -> *const ServerDescriptor> =
                library
                    .get(b"rptadv_iax2_server_descriptor_v2\0")
                    .map_err(|_| IaxError::MissingSymbol)?;
            symbol()
        };
        let api = checked_server_descriptor(descriptor)?;
        let address = address.to_string();
        let nodes = local_nodes.join(",");
        let handle = server_handle(&api, client.0.catalog.as_ptr(), &address, &nodes)?;
        Ok(Self {
            _owner: client.clone(),
            api,
            handle,
        })
    }

    /// Process at most one datagram; `false` means the socket had no queued packet.
    ///
    /// # Safety
    /// `context` must remain valid for the synchronous callback duration. The callbacks must
    /// uphold their FFI contracts and must not unwind across the C boundary.
    pub unsafe fn poll(
        &mut self,
        now_seconds: u32,
        authorize: ServerAuthorize,
        accept: ServerAccept,
        context: *mut c_void,
    ) -> Result<bool, IaxError> {
        let result = unsafe {
            self.api.poll.unwrap()(
                self.handle.as_ptr(),
                now_seconds,
                authorize,
                accept,
                context,
            )
        };
        (result >= 0)
            .then_some(result > 0)
            .ok_or(IaxError::Operation)
    }

    /// Replace local-node identities after a successful configuration reload.
    pub fn set_local_nodes(&mut self, local_nodes: &[String]) -> Result<(), IaxError> {
        if local_nodes.is_empty()
            || local_nodes
                .iter()
                .any(|node| node.is_empty() || !node.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(IaxError::Operation);
        }
        let nodes = local_nodes.join(",");
        (unsafe {
            self.api.set_local_nodes.unwrap()(self.handle.as_ptr(), nodes.as_ptr(), nodes.len())
        } == 0)
            .then_some(())
            .ok_or(IaxError::Operation)
    }
}

impl Drop for IaxServer {
    fn drop(&mut self) {
        unsafe { self.api.destroy.unwrap()(self.handle.as_ptr()) };
    }
}

fn valid_server_descriptor(pointer: *const ServerDescriptor) -> bool {
    if pointer.is_null() {
        return false;
    }
    let header = unsafe { pointer.cast::<DescriptorHeader>().read_unaligned() };
    if header.struct_size < size_of::<ServerDescriptor>() as u32
        || header.abi_version != ABI_VERSION
        || header.capability != SERVER_CAPABILITY
    {
        return false;
    }
    let api = unsafe { &*pointer };
    api.bind.is_some()
        && api.poll.is_some()
        && api.set_local_nodes.is_some()
        && api.destroy.is_some()
}

fn checked_server_descriptor(
    pointer: *const ServerDescriptor,
) -> Result<ServerDescriptor, IaxError> {
    if !valid_server_descriptor(pointer) {
        return Err(IaxError::IncompatibleAdapter);
    }
    // SAFETY: validation checked the complete immutable descriptor before copying it.
    unsafe { pointer.as_ref() }
        .copied()
        .ok_or(IaxError::IncompatibleAdapter)
}

fn server_handle(
    api: &ServerDescriptor,
    catalog: *const c_void,
    address: &str,
    nodes: &str,
) -> Result<NonNull<c_void>, IaxError> {
    let options = ServerOptions {
        struct_size: size_of::<ServerOptions>() as u32,
        abi_version: ABI_VERSION,
        bind_address: address.as_ptr(),
        bind_address_length: address.len(),
        local_nodes: nodes.as_ptr(),
        local_nodes_length: nodes.len(),
        catalog,
    };
    let mut handle = std::ptr::null_mut();
    if unsafe { api.bind.unwrap()(&options, &mut handle) } != 0 {
        return Err(IaxError::Bind);
    }
    NonNull::new(handle).ok_or(IaxError::Bind)
}

fn configured_secret<'a>(secrets: &'a SecretsFile, local_node: &str) -> &'a str {
    secrets.for_node(local_node).unwrap_or("")
}

fn dial_handle(
    api: &ClientDescriptor,
    catalog: *const c_void,
    remote: SocketAddr,
    local_node: &str,
    remote_node: &str,
    secret: &str,
    timeout_ms: u32,
) -> Result<NonNull<c_void>, IaxError> {
    let remote = remote.to_string();
    let local_call_number = NEXT_CALL_NUMBER
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(if current >= 32767 { 1 } else { current + 1 })
        })
        .unwrap();
    let options = DialOptions {
        struct_size: size_of::<DialOptions>() as u32,
        abi_version: ABI_VERSION,
        remote_address: remote.as_ptr(),
        remote_address_length: remote.len(),
        local_call_number,
        reserved: 0,
        local_node: local_node.as_ptr(),
        local_node_length: local_node.len(),
        remote_node: remote_node.as_ptr(),
        remote_node_length: remote_node.len(),
        secret: secret.as_ptr(),
        secret_length: secret.len(),
        timeout_ms,
        catalog,
    };
    let mut handle = std::ptr::null_mut();
    // SAFETY: the options and their borrowed UTF-8 strings live for this synchronous call.
    let result = unsafe { api.dial.unwrap()(&options, &mut handle) };
    if result != 0 {
        return Err(match result {
            -1 => IaxError::InvalidParameters,
            -2 => IaxError::Network,
            -3 => IaxError::Timeout,
            -4 => IaxError::Rejected,
            -5 => IaxError::Codec,
            -6 => IaxError::Protocol,
            _ => IaxError::Dial,
        });
    }
    NonNull::new(handle).ok_or(IaxError::Dial)
}

/// One IAX peer whose calls are serialized by its owning control/media worker.
pub struct IaxPeer {
    _owner: IaxClient,
    api: ClientDescriptor,
    handle: NonNull<c_void>,
}

impl IaxPeer {
    /// Feed the peer's retained encoder without committing a network packet.
    pub(crate) fn prepare_audio(
        &mut self,
        pcm: &[f32],
        output: &mut [u8],
        flush: u32,
        result: &mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        unsafe {
            self.api.prepare_audio.unwrap()(
                self.handle.as_ptr(),
                pcm.as_ptr(),
                pcm.len(),
                output.as_mut_ptr(),
                output.len(),
                flush,
                result,
            )
        }
    }
    /// Abandon expired input before processing a fresh source generation.
    pub(crate) fn reset_encoder(&mut self) -> i32 {
        unsafe { self.api.reset_encoder.unwrap()(self.handle.as_ptr()) }
    }
    /// Preserve congestion as a retryable result; accepted reliable frames belong to IAX.
    pub(crate) fn send_prepared(
        &mut self,
        bytes: &[u8],
        frames: u32,
        timestamp: u32,
    ) -> Result<bool, IaxError> {
        match unsafe {
            self.api.send_prepared_audio.unwrap()(
                self.handle.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                frames,
                timestamp,
            )
        } {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(IaxError::Operation),
        }
    }
    /// Monotonic call age used to anchor the radio source timeline off the callback.
    pub(crate) fn elapsed_ms(&self) -> u32 {
        unsafe { self.api.elapsed_ms.unwrap()(self.handle.as_ptr()) }
    }
    /// Opaque peer identity for the retained executor from the same protocol DSO.
    pub(crate) fn raw_handle(&self) -> *mut c_void {
        self.handle.as_ptr()
    }

    /// Return the selected codec's linear PCM rate.
    pub fn sample_rate_hz(&self) -> u32 {
        // SAFETY: the uniquely owned peer is live and the validated ABI owns the call.
        unsafe { self.api.sample_rate_hz.unwrap()(self.handle.as_ptr()) }
    }

    /// Send one ASL text payload.
    pub fn send_text(&mut self, text: &[u8]) -> Result<(), IaxError> {
        // SAFETY: the peer has one mutable owner and the borrowed slice remains live.
        (unsafe { self.api.send_text.unwrap()(self.handle.as_ptr(), text.as_ptr(), text.len()) }
            == 0)
            .then_some(())
            .ok_or(IaxError::Operation)
    }

    /// Send one completed DTMF digit.
    pub fn send_digit(&mut self, digit: u8) -> Result<(), IaxError> {
        // SAFETY: the peer has one mutable owner and the validated function table remains loaded.
        (unsafe { self.api.send_digit.unwrap()(self.handle.as_ptr(), digit) } == 0)
            .then_some(())
            .ok_or(IaxError::Operation)
    }

    /// Poll once without blocking and copy one event into the caller's buffers.
    pub fn poll(&mut self, samples: &mut [f32], text: &mut [u8]) -> Result<IaxEvent, IaxError> {
        let mut kind = 0;
        let mut length = 0;
        // SAFETY: the peer has one mutable owner and both output slices are writable.
        let result = unsafe {
            self.api.poll.unwrap()(
                self.handle.as_ptr(),
                samples.as_mut_ptr(),
                samples.len(),
                text.as_mut_ptr(),
                text.len(),
                &mut kind,
                &mut length,
            )
        };
        if result == 1 && kind == 0 {
            return if length > samples.len() && length <= isize::MAX as usize / size_of::<f32>() {
                Ok(IaxEvent::NeedCapacity(length))
            } else {
                Err(IaxError::Operation)
            };
        }
        if result != 0 {
            return Err(IaxError::Operation);
        }
        match kind {
            0 => Ok(IaxEvent::None),
            1 if length <= samples.len() => Ok(IaxEvent::Audio(length)),
            2 if length <= text.len() => Ok(IaxEvent::Text(length)),
            3 => Ok(IaxEvent::Hangup),
            4 if length == 1
                && text.first().is_some_and(
                    |digit| matches!(*digit, b'0'..=b'9' | b'A'..=b'D' | b'*' | b'#'),
                ) =>
            {
                Ok(IaxEvent::Digit(text[0]))
            }
            5 if length == 0 => Ok(IaxEvent::RadioKey),
            6 if length == 0 => Ok(IaxEvent::RadioUnkey),
            7 if length != 0 && length <= u32::MAX as usize => Ok(IaxEvent::Gap(length)),
            _ => Err(IaxError::Operation),
        }
    }
}

impl Drop for IaxPeer {
    fn drop(&mut self) {
        // SAFETY: this uniquely owned peer is destroyed once before its client is released.
        unsafe { self.api.destroy.unwrap()(self.handle.as_ptr()) };
    }
}

fn valid_descriptor(pointer: *const ClientDescriptor) -> bool {
    if pointer.is_null() {
        return false;
    }
    // SAFETY: the external descriptor guarantees at least its fixed prefix is readable.
    let header = unsafe { pointer.cast::<DescriptorHeader>().read_unaligned() };
    if header.struct_size < size_of::<ClientDescriptor>() as u32
        || header.abi_version != ABI_VERSION
        || header.capability != CAPABILITY
    {
        return false;
    }
    // SAFETY: the validated size covers the complete function table.
    let api = unsafe { &*pointer };
    api.open_catalog.is_some()
        && api.close_catalog.is_some()
        && api.dial.is_some()
        && api.sample_rate_hz.is_some()
        && api.send_text.is_some()
        && api.poll.is_some()
        && api.hangup.is_some()
        && api.destroy.is_some()
        && api.send_digit.is_some()
        && api.prepare_audio.is_some()
        && api.reset_encoder.is_some()
        && api.send_prepared_audio.is_some()
        && api.elapsed_ms.is_some()
}

/// Check the complete current capability without opening a call.
///
/// # Safety
/// Pointer must refer to a readable provider descriptor prefix, with its stated storage live.
pub(crate) unsafe fn valid_client_descriptor(pointer: *const c_void) -> bool {
    valid_descriptor(pointer.cast())
}

fn checked_descriptor(pointer: *const ClientDescriptor) -> Result<ClientDescriptor, IaxError> {
    if !valid_descriptor(pointer) {
        return Err(IaxError::IncompatibleAdapter);
    }
    // SAFETY: validation checked the complete immutable descriptor before copying it.
    unsafe { pointer.as_ref() }
        .copied()
        .ok_or(IaxError::IncompatibleAdapter)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        cell::Cell,
        sync::{Mutex, MutexGuard},
    };

    /// Construct a uniquely owned peer for higher-level lifecycle tests.
    pub(crate) fn runtime_test_peer() -> IaxPeer {
        unsafe extern "C" fn destroy(_: *mut c_void) {}
        let mut api = descriptor();
        api.destroy = Some(destroy);
        IaxPeer {
            _owner: fixture_client(api),
            api,
            handle: NonNull::dangling(),
        }
    }

    thread_local! {
        static EVENT: Cell<u32> = const { Cell::new(0) };
        static EVENT_LENGTH: Cell<usize> = const { Cell::new(1) };
        static EVENT_DIGIT: Cell<u32> = const { Cell::new(b'5' as u32) };
        static DIAL_RESULT: Cell<u32> = const { Cell::new(0) };
        static DIAL_NULL_HANDLE: Cell<bool> = const { Cell::new(false) };
        static DIAL_OPTIONS_VALID: Cell<u32> = const { Cell::new(0) };
        static DIAL_CALL_NUMBER: Cell<u32> = const { Cell::new(0) };
        static SENT_DIGIT: Cell<u32> = const { Cell::new(0) };
        static OPERATION_RESULT: Cell<i32> = const { Cell::new(0) };
        static SERVER_POLL_RESULT: Cell<i32> = const { Cell::new(0) };
        static SERVER_SET_RESULT: Cell<i32> = const { Cell::new(0) };
    }
    static DIAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn dial_test_guard() -> MutexGuard<'static, ()> {
        DIAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    unsafe extern "C" fn dial(options: *const DialOptions, output: *mut *mut c_void) -> i32 {
        let options = unsafe { &*options };
        let address = unsafe {
            std::slice::from_raw_parts(options.remote_address, options.remote_address_length)
        };
        let secret = unsafe { std::slice::from_raw_parts(options.secret, options.secret_length) };
        let local =
            unsafe { std::slice::from_raw_parts(options.local_node, options.local_node_length) };
        let remote =
            unsafe { std::slice::from_raw_parts(options.remote_node, options.remote_node_length) };
        let valid = address == b"127.0.0.1:4569"
            && secret == b"node-secret"
            && options.local_call_number > 0
            && options.local_call_number <= 32767
            && local == b"524950"
            && remote == b"506315"
            && options.timeout_ms == 1000;
        DIAL_OPTIONS_VALID.with(|value| value.set(u32::from(valid)));
        DIAL_CALL_NUMBER.with(|value| value.set(u32::from(options.local_call_number)));
        let result = DIAL_RESULT.with(Cell::get) as i32;
        if result == 0 && !DIAL_NULL_HANDLE.with(Cell::get) {
            unsafe { output.write(1_usize as *mut c_void) };
        }
        result
    }
    unsafe extern "C" fn rate(_: *const c_void) -> u32 {
        8000
    }
    unsafe extern "C" fn send_text(_: *mut c_void, _: *const u8, _: usize) -> i32 {
        OPERATION_RESULT.with(Cell::get)
    }
    unsafe extern "C" fn send_digit(_: *mut c_void, digit: u8) -> i32 {
        SENT_DIGIT.with(|value| value.set(u32::from(digit)));
        OPERATION_RESULT.with(Cell::get)
    }
    unsafe extern "C" fn poll(
        _: *mut c_void,
        samples: *mut f32,
        sample_capacity: usize,
        text: *mut u8,
        text_capacity: usize,
        kind: *mut u32,
        length: *mut usize,
    ) -> i32 {
        let event = EVENT.with(Cell::get);
        let event_length = EVENT_LENGTH.with(Cell::get);
        unsafe {
            kind.write(event);
            match event {
                1 if sample_capacity != 0 && event_length != 0 => {
                    samples.write(0.5);
                }
                2 if text_capacity != 0 && event_length != 0 => {
                    text.write(b'x');
                }
                4 if text_capacity != 0 && event_length != 0 => {
                    text.write(EVENT_DIGIT.with(Cell::get) as u8);
                }
                _ => {}
            }
            length.write(event_length);
        }
        0
    }
    unsafe extern "C" fn failed_poll(
        _: *mut c_void,
        _: *mut f32,
        _: usize,
        _: *mut u8,
        _: usize,
        _: *mut u32,
        _: *mut usize,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn hangup(_: *mut c_void) -> i32 {
        0
    }
    unsafe extern "C" fn destroy(_: *mut c_void) {}

    unsafe extern "C" fn server_bind(_: *const ServerOptions, _: *mut *mut c_void) -> i32 {
        0
    }
    unsafe extern "C" fn successful_server_bind(
        _: *const ServerOptions,
        output: *mut *mut c_void,
    ) -> i32 {
        unsafe { output.write(1_usize as *mut c_void) };
        0
    }
    unsafe extern "C" fn failed_server_bind(_: *const ServerOptions, _: *mut *mut c_void) -> i32 {
        -1
    }
    unsafe extern "C" fn server_poll(
        _: *mut c_void,
        _: u32,
        _: ServerAuthorize,
        _: ServerAccept,
        _: *mut c_void,
    ) -> i32 {
        SERVER_POLL_RESULT.with(Cell::get)
    }
    unsafe extern "C" fn server_set_nodes(_: *mut c_void, _: *const u8, _: usize) -> i32 {
        SERVER_SET_RESULT.with(Cell::get)
    }
    unsafe extern "C" fn server_destroy(_: *mut c_void) {}

    fn descriptor() -> ClientDescriptor {
        ClientDescriptor {
            struct_size: size_of::<ClientDescriptor>() as u32,
            abi_version: ABI_VERSION,
            capability: CAPABILITY,
            open_catalog: Some(test_open_catalog),
            close_catalog: Some(test_close_catalog),
            dial: Some(dial),
            sample_rate_hz: Some(rate),
            send_text: Some(send_text),
            poll: Some(poll),
            hangup: Some(hangup),
            destroy: Some(destroy),
            send_digit: Some(send_digit),
            prepare_audio: Some(prepare_audio),
            reset_encoder: Some(hangup),
            send_prepared_audio: Some(send_prepared),
            elapsed_ms: Some(elapsed),
        }
    }

    unsafe extern "C" fn test_open_catalog(
        _: *const abi::rptadv_iax2_codec_selection_v2,
        _: usize,
        output: *mut *mut c_void,
    ) -> i32 {
        unsafe { output.write(1_usize as *mut c_void) };
        0
    }
    unsafe extern "C" fn test_close_catalog(_: *mut c_void) {}
    fn fixture_client(api: ClientDescriptor) -> IaxClient {
        IaxClient(Arc::new(IaxOwner {
            library: None,
            api,
            catalog: NonNull::new(1_usize as *mut c_void).unwrap(),
        }))
    }
    unsafe extern "C" fn prepare_audio(
        _: *mut c_void,
        _: *const f32,
        count: usize,
        _: *mut u8,
        _: usize,
        _: u32,
        result: *mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        if OPERATION_RESULT.get() != 0 {
            return -1;
        }
        unsafe {
            (*result).status = OPERATION_RESULT.get() as u32;
            (*result).consumed_frames = count;
            (*result).encoded_bytes = count;
            (*result).packet_frames = count as u32;
            (*result).source_frames = count as u32;
        }
        OPERATION_RESULT.get()
    }
    unsafe extern "C" fn send_prepared(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: u32,
        _: u32,
    ) -> i32 {
        OPERATION_RESULT.get()
    }
    unsafe extern "C" fn elapsed(_: *const c_void) -> u32 {
        1234
    }

    /// Constrain a loopback listener to one real provider without changing live policy.
    pub(crate) fn selected_client(library: &Path, profile: u32) -> IaxClient {
        let mut client = IaxClient::load(library).unwrap();
        let owner = Arc::get_mut(&mut client.0).unwrap();
        let backend = CODEC_PROVIDERS
            .iter()
            .find(|(id, _)| *id == profile)
            .unwrap()
            .1;
        let path = CString::new(format!("librptadv_codec_{backend}.so.1")).unwrap();
        let selection = abi::rptadv_iax2_codec_selection_v2 {
            struct_size: size_of::<abi::rptadv_iax2_codec_selection_v2>() as u32,
            profile_id: profile,
            provider_path: path.as_ptr(),
        };
        let mut catalog = std::ptr::null_mut();
        unsafe {
            assert_eq!(
                owner.api.open_catalog.unwrap()(&selection, 1, &mut catalog),
                0
            );
            owner.api.close_catalog.unwrap()(owner.catalog.as_ptr());
        }
        owner.catalog = NonNull::new(catalog).unwrap();
        client
    }

    /// Feed a loopback peer through the same stateful preparation contract as production.
    pub(crate) fn send_pcm(peer: &mut IaxPeer, samples: &[f32]) {
        let mut output = vec![0_u8; 65507];
        let mut position = 0;
        let mut frames = 0_u64;
        loop {
            let flushing = position == samples.len();
            let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
            result.struct_size = size_of::<abi::rptadv_codec_encode_result_v1>() as u32;
            assert!(
                peer.prepare_audio(
                    &samples[position..],
                    &mut output,
                    u32::from(flushing),
                    &mut result
                ) >= 0
            );
            position += result.consumed_frames;
            if result.encoded_bytes == 0 && flushing && result.status == 0 {
                break;
            }
            if result.encoded_bytes != 0 {
                assert_eq!(
                    peer.send_prepared(
                        &output[..result.encoded_bytes],
                        result.packet_frames,
                        (frames * 1000 / u64::from(peer.sample_rate_hz())) as u32
                    ),
                    Ok(true)
                );
                frames += u64::from(result.packet_frames);
            }
        }
    }

    pub(crate) fn peer_for_host_callback(
        event: u32,
        fail_poll: bool,
        fail_operations: bool,
    ) -> IaxPeer {
        EVENT.with(|value| value.set(event));
        EVENT_LENGTH.with(|value| value.set(if matches!(event, 1 | 2 | 4 | 7) { 1 } else { 0 }));
        OPERATION_RESULT.with(|value| value.set(i32::from(fail_operations)));
        let mut api = descriptor();
        if fail_poll {
            api.poll = Some(failed_poll);
        }
        IaxPeer {
            _owner: fixture_client(api),
            api,
            handle: NonNull::new(1_usize as *mut c_void).unwrap(),
        }
    }

    fn server_descriptor() -> ServerDescriptor {
        ServerDescriptor {
            struct_size: size_of::<ServerDescriptor>() as u32,
            abi_version: ABI_VERSION,
            capability: SERVER_CAPABILITY,
            bind: Some(server_bind),
            poll: Some(server_poll),
            set_local_nodes: Some(server_set_nodes),
            destroy: Some(server_destroy),
        }
    }

    #[test]
    fn inbound_server_descriptor_requires_matching_abi_and_all_operations() {
        let mut api = server_descriptor();
        assert!(valid_server_descriptor(&api));
        api.abi_version += 1;
        assert!(!valid_server_descriptor(&api));
        api = server_descriptor();
        api.struct_size -= 1;
        assert!(!valid_server_descriptor(&api));
        api = server_descriptor();
        api.capability[0] ^= 1;
        assert!(!valid_server_descriptor(&api));
        api = server_descriptor();
        api.bind = None;
        assert!(!valid_server_descriptor(&api));
        api = server_descriptor();
        api.poll = None;
        assert!(!valid_server_descriptor(&api));
        api = server_descriptor();
        api.set_local_nodes = None;
        assert!(!valid_server_descriptor(&api));
        api = server_descriptor();
        api.destroy = None;
        assert!(!valid_server_descriptor(&api));
        assert!(matches!(
            checked_server_descriptor(std::ptr::null()),
            Err(IaxError::IncompatibleAdapter)
        ));
        let mut api = server_descriptor();
        api.bind = Some(successful_server_bind);
        assert!(server_handle(&api, std::ptr::null(), "127.0.0.1:4569", "524950").is_ok());
        api.bind = Some(failed_server_bind);
        assert_eq!(
            server_handle(&api, std::ptr::null(), "127.0.0.1:4569", "524950").err(),
            Some(IaxError::Bind)
        );
        api.bind = Some(server_bind);
        assert_eq!(
            server_handle(&api, std::ptr::null(), "127.0.0.1:4569", "524950").err(),
            Some(IaxError::Bind)
        );
    }

    #[test]
    fn descriptor_requires_exact_version_capability_and_all_functions() {
        unsafe extern "C" fn failed_catalog(
            _: *const abi::rptadv_iax2_codec_selection_v2,
            _: usize,
            _: *mut *mut c_void,
        ) -> i32 {
            -1
        }
        let mut invalid = descriptor();
        invalid.open_catalog = Some(failed_catalog);
        assert_eq!(open_catalog(&invalid), Err(IaxError::Codec));
        let header = DescriptorHeader {
            struct_size: size_of::<DescriptorHeader>() as u32,
            abi_version: ABI_VERSION,
            capability: CAPABILITY,
        };
        assert!(!valid_descriptor(std::ptr::from_ref(&header).cast()));
        let mut api = descriptor();
        assert!(valid_descriptor(&api));
        api.abi_version += 1;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.struct_size -= 1;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.capability[0] = b'x';
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.dial = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.sample_rate_hz = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.open_catalog = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.send_text = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.poll = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.hangup = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.destroy = None;
        assert!(!valid_descriptor(&api));
        api = descriptor();
        api.send_digit = None;
        assert!(!valid_descriptor(&api));
        for missing in 0..5 {
            api = descriptor();
            match missing {
                0 => api.prepare_audio = None,
                1 => api.send_prepared_audio = None,
                2 => api.elapsed_ms = None,
                3 => api.close_catalog = None,
                _ => api.reset_encoder = None,
            }
            assert!(!valid_descriptor(&api));
        }
        assert!(matches!(
            checked_descriptor(std::ptr::null()),
            Err(IaxError::IncompatibleAdapter)
        ));
    }

    #[test]
    fn iax_error_messages_are_stable() {
        for (error, expected) in [
            (IaxError::Load, "cannot load IAX2 client library"),
            (IaxError::MissingSymbol, "IAX2 client descriptor is missing"),
            (
                IaxError::IncompatibleAdapter,
                "IAX2 client ABI is incompatible",
            ),
            (IaxError::Dial, "IAX2 call setup failed"),
            (IaxError::Bind, "cannot bind the IAX2 listener"),
            (IaxError::Operation, "IAX2 peer operation failed"),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn installed_iax_adapter_loads_and_binds_loopback_without_external_network() {
        let library = Path::new("librptadviax2.so.2");
        let client = IaxClient::load(library).unwrap();
        let retained = client.clone();
        assert!(Arc::ptr_eq(&client.0, &retained.0));
        let mut server = IaxServer::bind(
            &client,
            "127.0.0.1:0".parse().unwrap(),
            &["524950".to_owned()],
        )
        .unwrap();
        unsafe {
            assert!(
                !server
                    .poll(
                        1,
                        Some(server_authorize),
                        Some(server_accept),
                        std::ptr::null_mut()
                    )
                    .unwrap()
            );
        }
        server.set_local_nodes(&["524950".to_owned()]).unwrap();
        assert!(Arc::ptr_eq(&server._owner.0, &retained.0));
    }

    unsafe extern "C" fn server_authorize(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn server_accept(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *mut c_void,
    ) -> i32 {
        0
    }

    #[test]
    fn inbound_server_rejects_missing_symbol_and_occupied_port() {
        assert_eq!(
            IaxServer::bind(
                &fixture_client(descriptor()),
                "127.0.0.1:0".parse().unwrap(),
                &["524950".to_owned()],
            )
            .err(),
            Some(IaxError::MissingSymbol)
        );
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        assert_eq!(
            IaxServer::bind(
                &IaxClient::load(Path::new("librptadviax2.so.2")).unwrap(),
                socket.local_addr().unwrap(),
                &["524950".to_owned()],
            )
            .err(),
            Some(IaxError::Bind)
        );
    }

    #[test]
    fn server_poll_and_node_update_report_adapter_failures() {
        let mut server = IaxServer {
            _owner: fixture_client(descriptor()),
            api: server_descriptor(),
            handle: NonNull::new(1_usize as *mut c_void).unwrap(),
        };
        SERVER_POLL_RESULT.with(|value| value.set(1));
        unsafe {
            assert!(
                server
                    .poll(
                        1,
                        Some(server_authorize),
                        Some(server_accept),
                        std::ptr::null_mut()
                    )
                    .unwrap()
            );
        }
        SERVER_POLL_RESULT.with(|value| value.set(-1));
        unsafe {
            assert_eq!(
                server.poll(
                    1,
                    Some(server_authorize),
                    Some(server_accept),
                    std::ptr::null_mut()
                ),
                Err(IaxError::Operation)
            );
        }
        assert_eq!(server.set_local_nodes(&[]), Err(IaxError::Operation));
        assert_eq!(
            server.set_local_nodes(&[String::new()]),
            Err(IaxError::Operation)
        );
        assert_eq!(
            server.set_local_nodes(&["not-a-node".to_owned()]),
            Err(IaxError::Operation)
        );
        SERVER_SET_RESULT.with(|value| value.set(1));
        assert_eq!(
            server.set_local_nodes(&["524950".to_owned()]),
            Err(IaxError::Operation)
        );
        SERVER_POLL_RESULT.with(|value| value.set(0));
        SERVER_SET_RESULT.with(|value| value.set(0));
    }

    #[test]
    fn peer_transfers_audio_text_and_reports_negotiated_rate() {
        let api = descriptor();
        let mut peer = IaxPeer {
            _owner: fixture_client(api),
            api,
            handle: NonNull::new(1_usize as *mut c_void).unwrap(),
        };
        assert_eq!(peer.sample_rate_hz(), 8000);
        assert_eq!(peer.elapsed_ms(), 1234);
        let mut result = unsafe { std::mem::zeroed() };
        assert_eq!(
            peer.prepare_audio(&[0.5; 160], &mut [0; 160], 0, &mut result),
            0
        );
        assert_eq!(result.consumed_frames, 160);
        assert_eq!(peer.send_prepared(&[0xff; 160], 160, 42), Ok(true));
        OPERATION_RESULT.set(1);
        assert_eq!(peer.send_prepared(&[0xff; 160], 160, 42), Ok(false));
        assert_eq!(peer.prepare_audio(&[0.5], &mut [0], 0, &mut result), -1);
        OPERATION_RESULT.set(-1);
        assert_eq!(
            peer.send_prepared(&[0xff; 160], 160, 42),
            Err(IaxError::Operation)
        );
        OPERATION_RESULT.set(0);
        assert!(peer.send_text(b"status").is_ok());
        assert!(peer.send_digit(b'7').is_ok());
        assert_eq!(SENT_DIGIT.with(Cell::get), u32::from(b'7'));
        let mut samples = [0.0; 4];
        let mut text = [0; 4];
        EVENT.with(|value| value.set(1));
        EVENT_LENGTH.with(|value| value.set(1));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::Audio(1)));
        assert_eq!(samples[0], 0.5);
        EVENT.with(|value| value.set(2));
        EVENT_LENGTH.with(|value| value.set(1));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::Text(1)));
        assert_eq!(text[0], b'x');
        EVENT.with(|value| value.set(4));
        EVENT_LENGTH.with(|value| value.set(1));
        EVENT_DIGIT.with(|value| value.set(u32::from(b'5')));
        assert_eq!(
            peer.poll(&mut samples, &mut text),
            Ok(IaxEvent::Digit(b'5'))
        );
        EVENT.with(|value| value.set(5));
        EVENT_LENGTH.with(|value| value.set(0));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::RadioKey));
        EVENT.with(|value| value.set(6));
        EVENT_LENGTH.with(|value| value.set(0));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::RadioUnkey));
        EVENT.with(|value| value.set(3));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::Hangup));
        EVENT.with(|value| value.set(0));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::None));

        EVENT.with(|value| value.set(1));
        EVENT_LENGTH.with(|value| value.set(1));
        assert_eq!(peer.poll(&mut [], &mut []), Err(IaxError::Operation));
        EVENT.with(|value| value.set(2));
        assert_eq!(peer.poll(&mut [], &mut []), Err(IaxError::Operation));
        EVENT.with(|value| value.set(4));
        EVENT_LENGTH.with(|value| value.set(1));
        EVENT_DIGIT.with(|value| value.set(u32::from(b'?')));
        assert_eq!(peer.poll(&mut samples, &mut text), Err(IaxError::Operation));
        EVENT_DIGIT.with(|value| value.set(u32::from(b'5')));
        EVENT_LENGTH.with(|value| value.set(2));
        assert_eq!(peer.poll(&mut samples, &mut text), Err(IaxError::Operation));
        EVENT.with(|value| value.set(5));
        assert_eq!(peer.poll(&mut samples, &mut text), Err(IaxError::Operation));
        EVENT.with(|value| value.set(6));
        assert_eq!(peer.poll(&mut samples, &mut text), Err(IaxError::Operation));
        EVENT.with(|value| value.set(7));
        assert_eq!(peer.poll(&mut samples, &mut text), Ok(IaxEvent::Gap(2)));
        EVENT_LENGTH.with(|value| value.set(u32::MAX as usize + 1));
        assert_eq!(peer.poll(&mut samples, &mut text), Err(IaxError::Operation));
        EVENT_LENGTH.with(|value| value.set(0));
        assert_eq!(peer.poll(&mut samples, &mut text), Err(IaxError::Operation));
        EVENT_LENGTH.with(|value| value.set(0));
    }

    #[test]
    fn peer_owns_the_loaded_client_after_dial_returns() {
        let _guard = dial_test_guard();
        let client = fixture_client(descriptor());
        let mut peer = client
            .dial(
                "127.0.0.1:4569".parse().unwrap(),
                "524950",
                "506315",
                "secret",
                1000,
            )
            .unwrap();

        assert_eq!(peer.sample_rate_hz(), 8000);
        assert_eq!(peer.reset_encoder(), 0);
    }

    #[test]
    fn inbound_client_adoption_requires_a_live_handle() {
        let client = fixture_client(descriptor());
        assert!(matches!(
            unsafe { client.adopt_inbound(std::ptr::null_mut()) },
            Err(IaxError::Operation)
        ));
        let client = fixture_client(descriptor());
        let peer = unsafe { client.adopt_inbound(1_usize as *mut c_void) }.unwrap();
        assert_eq!(peer.sample_rate_hz(), 8000);
    }

    #[test]
    fn peer_rejects_failed_polls() {
        unsafe extern "C" fn failed_poll(
            _: *mut c_void,
            _: *mut f32,
            _: usize,
            _: *mut u8,
            _: usize,
            _: *mut u32,
            _: *mut usize,
        ) -> i32 {
            -1
        }
        let mut api = descriptor();
        api.poll = Some(failed_poll);
        let mut peer = IaxPeer {
            _owner: fixture_client(api),
            api,
            handle: NonNull::new(1_usize as *mut c_void).unwrap(),
        };
        assert_eq!(peer.poll(&mut [], &mut []), Err(IaxError::Operation));
    }

    #[test]
    fn native_plc_capacity_request_is_not_hidden_as_an_idle_poll() {
        let mut peer = peer_for_capacity(1, 6000);
        assert_eq!(
            peer.poll(&mut [0.0; 4096], &mut []),
            Ok(IaxEvent::NeedCapacity(6000))
        );
        unsafe extern "C" fn invalid_capacity(
            _: *mut c_void,
            _: *mut f32,
            _: usize,
            _: *mut u8,
            _: usize,
            kind: *mut u32,
            length: *mut usize,
        ) -> i32 {
            unsafe {
                kind.write(EVENT.get());
                length.write(EVENT_LENGTH.get());
            }
            1
        }
        peer.api.poll = Some(invalid_capacity);
        for (kind, length) in [
            (1, 6000),
            (0, 4096),
            (0, isize::MAX as usize / size_of::<f32>() + 1),
        ] {
            EVENT.set(kind);
            EVENT_LENGTH.set(length);
            assert_eq!(
                peer.poll(&mut [0.0; 4096], &mut []),
                Err(IaxError::Operation)
            );
        }
    }

    /// Exercise the host's output-capacity retry without actual UDP or codec state.
    pub(crate) fn peer_for_capacity(kind: u32, length: usize) -> IaxPeer {
        unsafe extern "C" fn capacity_poll(
            _: *mut c_void,
            samples: *mut f32,
            sample_capacity: usize,
            text: *mut u8,
            text_capacity: usize,
            kind: *mut u32,
            length: *mut usize,
        ) -> i32 {
            let event = EVENT.get();
            let required = EVENT_LENGTH.get();
            let available = if event == 1 {
                sample_capacity
            } else {
                text_capacity
            };
            unsafe {
                length.write(required);
                kind.write(if available < required { 0 } else { event });
            }
            if available < required {
                return 1;
            }
            if event == 1 {
                unsafe { std::slice::from_raw_parts_mut(samples, required) }.fill(0.25);
            } else {
                unsafe { std::slice::from_raw_parts_mut(text, required) }.fill(b'x');
            }
            0
        }
        EVENT.set(kind);
        EVENT_LENGTH.set(length);
        let mut peer = runtime_test_peer();
        peer.api.poll = Some(capacity_poll);
        peer
    }

    #[test]
    fn configured_dial_uses_node_secret_and_reports_failed_setup() {
        let _guard = dial_test_guard();
        let api = descriptor();
        let secrets = SecretsFile::parse(
            "[general]\niax_secret=global-secret\n[524950]\niax_secret=node-secret\n",
        )
        .unwrap();
        assert_eq!(configured_secret(&secrets, "524950"), "node-secret");
        assert_eq!(configured_secret(&secrets, "508422"), "global-secret");
        assert_eq!(
            configured_secret(&SecretsFile::parse("").unwrap(), "524950"),
            ""
        );
        DIAL_RESULT.with(|value| value.set(0));
        DIAL_OPTIONS_VALID.with(|value| value.set(0));
        let peer = fixture_client(api).dial_configured(
            "127.0.0.1:4569".parse().unwrap(),
            "524950",
            "506315",
            &secrets,
            1000,
        );
        assert!(peer.is_ok());
        assert_eq!(DIAL_OPTIONS_VALID.with(Cell::get), 1);
        drop(peer);
        DIAL_RESULT.with(|value| value.set(1));
        assert_eq!(
            fixture_client(api)
                .dial_configured(
                    "127.0.0.1:4569".parse().unwrap(),
                    "524950",
                    "506315",
                    &secrets,
                    1000,
                )
                .err(),
            Some(IaxError::Dial)
        );
        assert_eq!(DIAL_OPTIONS_VALID.with(Cell::get), 1);
        DIAL_RESULT.with(|value| value.set(0));
    }

    #[test]
    fn dial_wraps_call_numbers_and_rejects_success_without_a_peer_handle() {
        let _guard = dial_test_guard();
        NEXT_CALL_NUMBER.store(32767, Ordering::Relaxed);
        DIAL_RESULT.with(|value| value.set(0));
        DIAL_NULL_HANDLE.with(|value| value.set(false));
        assert!(
            fixture_client(descriptor())
                .dial(
                    "127.0.0.1:4569".parse().unwrap(),
                    "524950",
                    "506315",
                    "secret",
                    1000,
                )
                .is_ok()
        );
        assert_eq!(DIAL_CALL_NUMBER.with(Cell::get), 32767);
        assert!(
            fixture_client(descriptor())
                .dial(
                    "127.0.0.1:4569".parse().unwrap(),
                    "524950",
                    "506315",
                    "secret",
                    1000,
                )
                .is_ok()
        );
        assert_eq!(DIAL_CALL_NUMBER.with(Cell::get), 1);
        DIAL_NULL_HANDLE.with(|value| value.set(true));
        assert_eq!(
            fixture_client(descriptor())
                .dial(
                    "127.0.0.1:4569".parse().unwrap(),
                    "524950",
                    "506315",
                    "secret",
                    1000,
                )
                .err(),
            Some(IaxError::Dial)
        );
        DIAL_NULL_HANDLE.with(|value| value.set(false));
        NEXT_CALL_NUMBER.store(1, Ordering::Relaxed);
    }

    #[test]
    fn dial_preserves_safe_abi_failure_reasons() {
        let _guard = dial_test_guard();
        for (status, reason) in [
            (-1, "IAX2 invalid dial parameters"),
            (-2, "IAX2 network failure"),
            (-3, "IAX2 call setup timed out"),
            (-4, "IAX2 call rejected"),
            (-5, "IAX2 codec negotiation failed"),
            (-6, "IAX2 protocol failure"),
        ] {
            DIAL_RESULT.with(|value| value.set(status as u32));
            let error = fixture_client(descriptor())
                .dial(
                    "127.0.0.1:4569".parse().unwrap(),
                    "524950",
                    "506315",
                    "secret",
                    1000,
                )
                .err()
                .unwrap();
            assert_eq!(error.to_string(), reason);
        }
        DIAL_RESULT.with(|value| value.set(0));
    }

    #[test]
    fn missing_library_reports_non_secret_load_error() {
        assert_eq!(
            IaxClient::load(Path::new("/missing/librptadviax2.so.2")).err(),
            Some(IaxError::Load)
        );
        assert_eq!(
            IaxClient::load(Path::new("libc.so.6")).err(),
            Some(IaxError::MissingSymbol)
        );
    }
}
