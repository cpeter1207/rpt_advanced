//! Shared non-audio host ownership. Accounts retain their context and provider DSO.
use super::{Api, abi};
use libloading::Library;
mod accounts;
pub use accounts::Accounts;
use std::{
    net::SocketAddrV4,
    path::Path,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Safe, non-secret provider setup failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shared object or its descriptor could not be loaded.
    Load,
    /// Descriptor, operation group, or returned handle is incomplete.
    Contract,
    /// Provider rejected the operation.
    Status(abi::Status),
    /// Private account credentials could not be loaded safely.
    Secrets,
    /// The selected control owner could not quiesce off executor.
    Control,
}

fn completed(status: abi::Status) -> Result<(), Error> {
    if status == abi::Status::Ok {
        Ok(())
    } else {
        Err(Error::Status(status))
    }
}

unsafe fn text(span: abi::ByteSpan, maximum: usize) -> Result<String, Error> {
    if span.length > maximum || (span.data.is_null() && span.length != 0) {
        return Err(Error::Contract);
    }
    let bytes = if span.length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(span.data, span.length) }
    };
    let value = std::str::from_utf8(bytes).map_err(|_| Error::Contract)?;
    if value.chars().any(char::is_control) {
        return Err(Error::Contract);
    }
    Ok(value.into())
}

/// One dynamically loaded provider and worker context shared by both host adapters.
pub struct Provider {
    api: Api,
    context: NonNull<abi::Context>,
    // Drop only after context_destroy has returned.
    _library: Library,
}
// The ABI context is thread-safe; stateful QSO work has its own serial owner.
unsafe impl Send for Provider {}
unsafe impl Sync for Provider {}

impl Provider {
    /// Load and validate all advertised operations before creating any context.
    /// This does not bind sockets, log in, or open radio hardware.
    ///
    /// # Safety
    /// The path names a trusted native provider. Configuration spans remain
    /// readable throughout context creation; the provider copies retained data.
    pub unsafe fn load(path: &Path, config: &abi::RuntimeConfig) -> Result<Arc<Self>, Error> {
        let library = unsafe { Library::new(path) }.map_err(|_| Error::Load)?;
        let descriptor = unsafe {
            library.get::<unsafe extern "C" fn() -> *const abi::Descriptor>(
                b"rptadv_echolink_descriptor_v1\0",
            )
        }
        .map_err(|_| Error::Load)?;
        let api = unsafe { Api::open(descriptor()) }.map_err(|_| Error::Contract)?;
        let mut context = std::ptr::null_mut();
        completed(unsafe { api.table().context_create.unwrap()(config, &mut context) })?;
        Ok(Arc::new(Self {
            api,
            context: NonNull::new(context).ok_or(Error::Contract)?,
            _library: library,
        }))
    }

    /// Copy a resolved account without activating network I/O. Enabled accounts
    /// require the validated network and source-media groups before preparation.
    ///
    /// # Safety
    /// All foreign spans/lists in both records remain readable throughout this
    /// synchronous call. Preparation copies them before returning.
    pub unsafe fn prepare(
        self: &Arc<Self>,
        config: &abi::AccountConfig,
        secrets: &abi::AccountSecrets,
    ) -> Result<Arc<Account>, Error> {
        if config.enabled != 0 && !self.api.prepared_media_available() {
            return Err(Error::Status(abi::Status::Unsupported));
        }
        let local = unsafe { text(config.local_node, 32) }?;
        let mut handle = std::ptr::null_mut();
        completed(unsafe {
            self.api.table().account_prepare.unwrap()(
                self.context.as_ptr(),
                config,
                secrets,
                &mut handle,
            )
        })?;
        Ok(Arc::new(Account {
            handle: NonNull::new(handle).ok_or(Error::Contract)?,
            provider: Arc::clone(self),
            local,
            stopped: AtomicBool::new(false),
        }))
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        unsafe { self.api.table().context_destroy.unwrap()(self.context.as_ptr()) };
    }
}

/// An off-audio account owner; drop only after its host callbacks are quiescent.
pub struct Account {
    handle: NonNull<abi::Account>,
    provider: Arc<Provider>,
    local: String,
    stopped: AtomicBool,
}
// Account operations are serialized internally and never called by audio callbacks.
unsafe impl Send for Account {}
unsafe impl Sync for Account {}
impl Account {
    /// Activate on host control after product startup. The callback may execute
    /// before return, so its stable context must already be fully initialized.
    ///
    /// # Safety
    /// Callback/context are thread-safe, cannot unwind, and remain live through
    /// off-executor account stop plus accepted control-handoff drainage. Never
    /// call from a provider or audio executor.
    pub unsafe fn activate(
        &self,
        callback: Option<abi::EventFn>,
        context: *mut std::ffi::c_void,
    ) -> Result<(), Error> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        if !self.provider.api.network_available() {
            return Err(Error::Status(abi::Status::Unsupported));
        }
        completed(unsafe {
            self.provider.api.table().account_activate.unwrap()(
                self.handle.as_ptr(),
                callback,
                context,
            )
        })
    }

    /// Copy/commit one resolved update on the retained account owner. A changed
    /// controller node requires a different owner; Echo identity/socket changes
    /// use the provider's quiescent account-only handoff.
    ///
    /// # Safety
    /// Resolved configuration/secret spans remain readable for this synchronous
    /// control-only call. Retain the original callback context through disable
    /// and re-enable, and quiesce it only with permanent stop.
    pub unsafe fn reconfigure(
        &self,
        config: &abi::AccountConfig,
        secrets: &abi::AccountSecrets,
    ) -> Result<(), Error> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        if !self.provider.api.network_available() {
            return Err(Error::Status(abi::Status::Unsupported));
        }
        if unsafe { text(config.local_node, 32) }? != self.local {
            return Err(Error::Contract);
        }
        completed(unsafe {
            self.provider.api.table().account_reconfigure.unwrap()(
                self.handle.as_ptr(),
                config,
                secrets,
            )
        })
    }

    /// Dial one real EchoLink ID on host control/dial work, never on a provider
    /// or audio callback. Outbound calls have no observed incoming endpoint.
    pub fn dial(self: &Arc<Self>, node: u32) -> Result<Arc<Peer>, Error> {
        if node == 0 {
            return Err(Error::Status(abi::Status::Invalid));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        if !self.provider.api.network_available() {
            return Err(Error::Status(abi::Status::Unsupported));
        }
        let mut handle = std::ptr::null_mut();
        completed(unsafe {
            self.provider.api.table().dial.unwrap()(self.handle.as_ptr(), node, &mut handle)
        })?;
        let handle = NonNull::new(handle).ok_or(Error::Contract)?;
        let identity = unsafe { Identity::read(&self.provider.api, handle) }.and_then(|identity| {
            if identity.local == self.local && identity.node_number == node {
                Ok(identity)
            } else {
                Err(Error::Contract)
            }
        });
        match identity {
            Ok(identity) => Ok(Arc::new(Peer {
                handle,
                account: Arc::clone(self),
                remote: node.to_string(),
                identity,
            })),
            Err(error) => {
                unsafe { self.provider.api.table().peer_destroy.unwrap()(handle.as_ptr()) };
                Err(error)
            }
        }
    }

    /// Gate incoming work and join account notifications before releasing host state.
    /// Repeated stop is harmless. Never call from a provider executor or audio callback.
    pub fn stop(&self) -> Result<(), Error> {
        self.stopped.store(true, Ordering::Release);
        completed(unsafe { self.provider.api.table().account_stop.unwrap()(self.handle.as_ptr()) })
    }

    /// Claim an incoming QSO only after fresh provider-backed identity validation.
    /// Failure leaves ownership and its acknowledgement with the caller/provider.
    ///
    /// # Safety
    /// Run off the provider executor, on host control. The live event and writable
    /// acknowledgement remain borrowed throughout this call. Its unique QSO came
    /// from this exact account; the host must transfer/destroy accepted peers on
    /// control and drain accepted handoffs before releasing the callback context.
    pub unsafe fn take_pending(self: &Arc<Self>, event: &abi::Event) -> Result<Arc<Peer>, Error> {
        let handle = unsafe { self.pending_handle(event) }?;
        let identity = unsafe { Identity::read(&self.provider.api, handle) }?;
        if identity.local != self.local
            || identity.qso_generation != event.generation
            || identity.source.is_none()
        {
            return Err(Error::Contract);
        }
        let peer = Arc::new(Peer {
            handle,
            account: Arc::clone(self),
            remote: identity.node_number.to_string(),
            identity,
        });
        unsafe { event.taken.write(1) };
        Ok(peer)
    }

    unsafe fn pending_handle(&self, event: &abi::Event) -> Result<NonNull<abi::Qso>, Error> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        if event.struct_size < std::mem::size_of::<abi::Event>() as u32
            || event.kind != 1
            || event.status != abi::Status::Pending
            || event.taken.is_null()
            || unsafe { event.taken.read() } != 0
        {
            return Err(Error::Contract);
        }
        NonNull::new(event.qso).ok_or(Error::Contract)
    }

    /// Transfer a provider callback's pending QSO to the host's existing bounded
    /// control executor, without waiting or retaining the borrowed event/ack.
    /// Rejection leaves provider ownership unchanged. Accepted work may already
    /// have run and retired the QSO before submission returns.
    ///
    /// # Safety
    /// Event/ack obey `take_pending`'s live-borrow contract. Submission cannot
    /// unwind; success transfers the payload to off-provider control and drains
    /// it before host teardown, while rejection returns that untouched payload.
    /// The account callback context remains retained through stop and drain.
    pub unsafe fn dispatch_pending(
        self: &Arc<Self>,
        event: &abi::Event,
        submit: impl FnOnce(PendingIncoming) -> Result<(), (Error, PendingIncoming)>,
    ) -> Result<(), Error> {
        let handle = unsafe { self.pending_handle(event) }?;
        let pending = PendingIncoming {
            handle: Some(handle),
            account: Arc::clone(self),
            generation: event.generation,
        };
        match submit(pending) {
            Ok(()) => {
                // Do not touch the accepted payload: it may already be released.
                unsafe { event.taken.write(1) };
                Ok(())
            }
            Err((error, mut pending)) => {
                pending.handle = None; // Original provider still owns rejection.
                Err(error)
            }
        }
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        if self.stop().is_ok() {
            unsafe { self.provider.api.table().account_destroy.unwrap()(self.handle.as_ptr()) };
        } else {
            // A failed join may still execute provider code. Retain its context/DSO
            // along with the foreign account rather than unload beneath that work.
            std::mem::forget(Arc::clone(&self.provider));
        }
    }
}

/// One accepted control handoff retaining its exact account and QSO generation.
/// Claim or retire on host control, never on the provider/audio executor.
pub struct PendingIncoming {
    handle: Option<NonNull<abi::Qso>>,
    account: Arc<Account>,
    generation: u64,
}
// Transfer only to the existing serialized control executor; no mutable sharing.
unsafe impl Send for PendingIncoming {}
impl PendingIncoming {
    /// Freshly validate and claim this owned QSO. Failure retires it, rather than
    /// leak an accepted handoff or treat copied metadata as authentication.
    pub fn claim(mut self) -> Result<Arc<Peer>, Error> {
        let mut taken = 0;
        let event = abi::Event {
            struct_size: std::mem::size_of::<abi::Event>() as u32,
            kind: 1,
            qso: self.handle.unwrap().as_ptr(),
            generation: self.generation,
            status: abi::Status::Pending,
            taken: &mut taken,
        };
        let peer = unsafe { self.account.take_pending(&event) }?;
        self.handle = None; // Peer now owns the accepted QSO.
        Ok(peer)
    }
}
impl Drop for PendingIncoming {
    fn drop(&mut self) {
        if let Some(handle) = self.handle {
            unsafe { self.account.provider.api.table().peer_destroy.unwrap()(handle.as_ptr()) };
        }
    }
}

/// Copied verified identity, never a caller-supplied authentication assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Complete positive EchoLink ID.
    pub node_number: u32,
    /// Provider network/media/text mask, intersected with complete advertised groups.
    pub capabilities: u32,
    /// Exact account generation.
    pub account_generation: u64,
    /// Exact connection generation.
    pub qso_generation: u64,
    /// Directory-validated callsign, not a represented chat sender.
    pub callsign: String,
    /// Owning controller node.
    pub local: String,
    /// Observed incoming SDES endpoint, with port zero for Proxy framing. None
    /// for outbound QSOs; pending incoming admission always requires Some.
    pub source: Option<SocketAddrV4>,
}
impl Identity {
    /// Translate to host-services bits without granting ASL remote commands.
    pub fn host_capabilities(&self) -> u32 {
        ((self.capabilities & 2) >> 1)
            | if self.capabilities & 4 != 0 {
                2 | 16
            } else {
                0
            }
            | ((self.capabilities & 8) >> 1)
    }

    unsafe fn read(api: &Api, handle: NonNull<abi::Qso>) -> Result<Self, Error> {
        let validate = api.table().validate_pending.ok_or(Error::Contract)?;
        let mut output = abi::PeerInfo {
            struct_size: std::mem::size_of::<abi::PeerInfo>() as u32,
            abi_version: 1,
            node_number: 0,
            capabilities: 0,
            account_generation: 0,
            qso_generation: 0,
            callsign: abi::ByteSpan::default(),
            local_node: abi::ByteSpan::default(),
            source: abi::ByteSpan::default(),
        };
        completed(unsafe { validate(handle.as_ptr(), &mut output) })?;
        let capabilities = output.capabilities & api.table().capabilities as u32;
        if output.struct_size < std::mem::size_of::<abi::PeerInfo>() as u32
            || output.abi_version != 1
            || output.node_number == 0
            || output.account_generation == 0
            || output.qso_generation == 0
            || output.capabilities & !14 != 0
            || capabilities & 6 != 6
        {
            return Err(Error::Contract);
        }
        let callsign = unsafe { text(output.callsign, 15) }?;
        let local = unsafe { text(output.local_node, 32) }?;
        let source = unsafe { text(output.source, 64) }?;
        let source = if source.is_empty() {
            None
        } else {
            Some(source.parse().map_err(|_| Error::Contract)?)
        };
        if callsign.is_empty()
            || local.is_empty()
            || !local.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(Error::Contract);
        }
        Ok(Self {
            node_number: output.node_number,
            capabilities,
            account_generation: output.account_generation,
            qso_generation: output.qso_generation,
            callsign,
            local,
            source,
        })
    }
}

/// One claimed QSO. Its account/context/DSO remain live through final reclamation.
pub struct Peer {
    handle: NonNull<abi::Qso>,
    account: Arc<Account>,
    identity: Identity,
    remote: String,
}
// Immutable metadata is copied; mutable provider media is accessed only by its serial owner.
unsafe impl Send for Peer {}
unsafe impl Sync for Peer {}
impl Peer {
    /// Borrow the copied identity; fresh admission still uses provider revalidation.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    /// Borrow the canonical real decimal ID for host ABI metadata.
    pub fn remote(&self) -> &str {
        &self.remote
    }

    /// Read source-rate PCM/text on the one serial peer owner. The provider
    /// retains every unaccepted suffix and honors exact played/discard completion.
    ///
    /// # Safety
    /// Sink/text callbacks and their contexts remain valid for this synchronous
    /// turn, cannot unwind or reenter this QSO, and return truthful prefix credit.
    pub unsafe fn read(
        &self,
        completion: u32,
        sink: Option<&abi::PcmSink>,
        text: Option<abi::TextFn>,
        context: *mut std::ffi::c_void,
    ) -> Result<abi::ReadResult, Error> {
        let mut result = abi::ReadResult::default();
        completed(unsafe {
            self.account.provider.api.table().peer_read.unwrap()(
                self.handle.as_ptr(),
                completion,
                sink.map_or(std::ptr::null(), std::ptr::from_ref),
                text,
                context,
                &mut result,
            )
        })?;
        Ok(result)
    }

    /// Prepare one bounded packet without consuming the caller's output on
    /// backpressure. The common codec result carries its own status and counts.
    ///
    /// # Safety
    /// Call on the serial owner with a complete writable released
    /// rptadv_codec_encode_result_v1 at `result`. No same-QSO reentry is allowed.
    pub unsafe fn prepare_audio(
        &self,
        pcm: &[f32],
        output: &mut [u8],
        flush: u32,
        result: *mut std::ffi::c_void,
    ) -> Result<(), Error> {
        completed(unsafe {
            self.account
                .provider
                .api
                .table()
                .peer_prepare_audio
                .unwrap()(
                self.handle.as_ptr(),
                pcm.as_ptr(),
                pcm.len(),
                output.as_mut_ptr(),
                output.len(),
                flush,
                result,
            )
        })
    }

    /// Reset encoder segment history on its serial owner, not connection clocks.
    ///
    /// # Safety
    /// Only the registered serial owner may access this QSO's media state.
    pub unsafe fn reset_encoder(&self) -> Result<(), Error> {
        completed(unsafe {
            self.account
                .provider
                .api
                .table()
                .peer_reset_encoder
                .unwrap()(self.handle.as_ptr())
        })
    }

    /// Submit the exact prepared bytes and pacing hints; false retains caller
    /// ownership for an unchanged retry on ordinary provider backpressure.
    ///
    /// # Safety
    /// Only the registered serial owner may access this QSO's media state.
    pub unsafe fn send_prepared(
        &self,
        bytes: &[u8],
        frames: u32,
        timestamp: u32,
    ) -> Result<bool, Error> {
        match unsafe {
            self.account
                .provider
                .api
                .table()
                .peer_send_prepared
                .unwrap()(
                self.handle.as_ptr(),
                abi::ByteSpan {
                    data: bytes.as_ptr(),
                    length: bytes.len(),
                },
                frames,
                timestamp,
            )
        } {
            abi::Status::Ok => Ok(true),
            abi::Status::Pending | abi::Status::Full => Ok(false),
            status => Err(Error::Status(status)),
        }
    }

    /// Copy the provider's monotonic clock without creating a second host epoch.
    pub fn elapsed_ms(&self) -> Result<u64, Error> {
        let mut elapsed = 0;
        completed(unsafe {
            self.account.provider.api.table().peer_elapsed_ms.unwrap()(
                self.handle.as_ptr(),
                &mut elapsed,
            )
        })?;
        Ok(elapsed)
    }

    /// Set or clear the serial owner's next deadline; no new task is created.
    ///
    /// # Safety
    /// Call only from the registered serial peer owner.
    pub unsafe fn set_deadline(&self, after_ns: u64) -> Result<(), Error> {
        completed(unsafe {
            self.account.provider.api.table().peer_set_deadline.unwrap()(
                self.handle.as_ptr(),
                after_ns,
            )
        })
    }

    /// Send structured human text, never an ASL control envelope or speech PCM.
    ///
    /// # Safety
    /// Only the registered serial owner may access this QSO's media state.
    pub unsafe fn send_text(&self, sender: &[u8], body: &[u8]) -> Result<(), Error> {
        let operation = self
            .account
            .provider
            .api
            .table()
            .peer_send_text
            .ok_or(Error::Contract)?;
        completed(unsafe {
            operation(
                self.handle.as_ptr(),
                abi::ByteSpan {
                    data: sender.as_ptr(),
                    length: sender.len(),
                },
                abi::ByteSpan {
                    data: body.as_ptr(),
                    length: body.len(),
                },
            )
        })
    }

    /// Recheck directory/account/QSO state at final admission, not as a live-call
    /// heartbeat. Copied metadata alone never grants a new attachment.
    pub fn revalidate(&self) -> Result<(), Error> {
        if self.account.stopped.load(Ordering::Acquire) {
            return Err(Error::Status(abi::Status::Closed));
        }
        let current = unsafe { Identity::read(&self.account.provider.api, self.handle) }?;
        if current == self.identity {
            Ok(())
        } else {
            Err(Error::Contract)
        }
    }

    /// Attach the one serial product step after revalidating its exact generation.
    /// No mutable peer borrow spans the potentially reentrant provider call.
    ///
    /// # Safety
    /// Call once off the provider executor. The callback/context are thread-safe,
    /// cannot unwind, and stay live until off-executor Registration drop returns.
    /// Stateful media calls occur only from that serial step. Drop/join the
    /// registration on host control, never from its callback or an audio thread.
    pub unsafe fn attach(
        self: &Arc<Self>,
        callback: Option<abi::StepFn>,
        context: *mut std::ffi::c_void,
    ) -> Result<Registration, Error> {
        if callback.is_none() {
            return Err(Error::Contract);
        }
        self.revalidate()?;
        let peer = Arc::clone(self);
        let api = self.account.provider.api;
        let provider_context = self.account.provider.context.as_ptr();
        let qso = self.handle.as_ptr();
        let mut handle = std::ptr::null_mut();
        completed(unsafe {
            api.table().peer_attach.unwrap()(provider_context, qso, callback, context, &mut handle)
        })?;
        Ok(Registration {
            handle: NonNull::new(handle).ok_or(Error::Contract)?,
            peer,
        })
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        // The host's control owner quiesces registration before releasing its QSO.
        unsafe { self.account.provider.api.table().peer_destroy.unwrap()(self.handle.as_ptr()) };
    }
}

/// Serial registration retains the peer, its account and DSO through the join.
/// Stop/wake do not join; drop is the off-executor notification barrier.
pub struct Registration {
    handle: NonNull<abi::Registration>,
    peer: Arc<Peer>,
}
// Provider notification controls are thread-safe; only the lifecycle owner drops/join.
unsafe impl Send for Registration {}
unsafe impl Sync for Registration {}
impl Registration {
    /// Gate new notifications without waiting on the serial callback.
    pub fn stop(&self) {
        unsafe { self.peer.account.provider.api.table().peer_stop.unwrap()(self.handle.as_ptr()) };
    }
    /// Notify the existing serial owner without creating a task or thread.
    pub fn wake(&self) {
        unsafe { self.peer.account.provider.api.table().peer_wake.unwrap()(self.handle.as_ptr()) };
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.stop();
        unsafe {
            self.peer.account.provider.api.table().peer_detach.unwrap()(self.handle.as_ptr())
        };
    }
}

/// Host-specific native payloads retain their existing implementation. Echo
/// peers share this owner without creating an Asterisk channel or PCM workspace.
pub enum HostPeer<Iax> {
    /// Existing host-native IAX/channel owner.
    Iax(Iax),
    /// Verified EchoLink QSO retained through its serial registration.
    Echo(Arc<Peer>),
}
