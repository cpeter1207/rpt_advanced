//! Thin ownership of one native station in the shared USBRadioPlus product.

use crate::{abi, providers::PhysicalProviders, station_plan::StationPlan};
use std::{ffi::c_void, ptr::NonNull};

/// A shared native station could not be created or started.
#[derive(Debug)]
pub enum RadioActivationError {
    /// A required dynamic provider could not be loaded or validated.
    Provider(crate::providers::ProviderError),
    /// Required callback functions or the frame bound are invalid.
    InvalidCallbacks,
    /// The shared product returned a failure status.
    Product(i32),
    /// Startup may have retained callbacks; keep the exact native owner until quiesced.
    Retained {
        /// Original startup failure, preserved through later cleanup.
        status: i32,
        /// Partially started owner retained by the radio reservation.
        active: Box<ActiveRadio>,
    },
}

impl std::fmt::Display for RadioActivationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::InvalidCallbacks => {
                formatter.write_str("invalid native radio callbacks or frame bound")
            }
            Self::Product(status) => write!(formatter, "shared radio product failed ({status})"),
            Self::Retained { status, .. } => write!(
                formatter,
                "shared radio startup failed ({status}); retirement pending"
            ),
        }
    }
}
impl std::error::Error for RadioActivationError {}

/// Borrowed native callback pair retained through synchronous station destruction.
pub struct ProductRadioCallbacks {
    /// Qualified receive audio callback.
    pub receive: abi::rptadv_radio_receive_v3,
    /// Caller-owned receive context.
    pub receive_context: *mut c_void,
    /// Exact-frame transmit audio and PTT/CTCSS callback.
    pub transmit: abi::rptadv_radio_transmit_v3,
    /// Caller-owned transmit context.
    pub transmit_context: *mut c_void,
    /// Same serial transmit-owner policy/activity endpoint.
    pub metadata: abi::rptadv_radio_metadata_v1,
    /// Optional pre-start feedback registration owned by the same caller.
    pub feedback: Option<abi::UrpRadioFeedbackPort>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdatePhase {
    Idle,
    AwaitingAcceptance,
    Staged,
    Finishing(bool),
    Recovery,
}

/// Native station handle; its shared product owns every hardware and DSP resource.
pub struct ActiveRadio {
    physical_providers: Option<PhysicalProviders>,
    handle: Option<NonNull<c_void>>,
    api: abi::UrpAstDescriptor,
    update: UpdatePhase,
    stop: unsafe extern "C" fn(*mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
    site_bind: unsafe extern "C" fn(*mut c_void, u32, *const abi::UrpAstSiteBinding) -> i32,
    site_publish: unsafe extern "C" fn(*mut c_void, u32, *const abi::UrpAstSiteRequest) -> i32,
    site_snapshot: unsafe extern "C" fn(*mut c_void, u32, *mut abi::UrpAstSiteSnapshot) -> i32,
}

impl std::fmt::Debug for ActiveRadio {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveRadio")
            .field("open", &self.handle.is_some())
            .field("updating", &(self.update != UpdatePhase::Idle))
            .finish_non_exhaustive()
    }
}

impl ActiveRadio {
    /// Open the prepared station while retaining its providers and callback contexts.
    pub(crate) fn open_prepared(
        plan: &StationPlan,
        generation_id: u64,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<Self, RadioActivationError> {
        let descriptors = plan
            .providers
            .as_ref()
            .radio_runtime_descriptors()
            .map_err(RadioActivationError::Provider)?;
        let manifest = abi::UrpAstProviderManifest {
            struct_size: std::mem::size_of::<abi::UrpAstProviderManifest>() as u32,
            abi_version: 1,
            ffmpeg: std::ptr::from_ref(descriptors.ffmpeg).cast(),
            radio: descriptors.radio,
            audio: std::ptr::from_ref(descriptors.audio).cast(),
            gpio: std::ptr::from_ref(descriptors.gpio).cast(),
            rnnoise: descriptors.rnnoise,
            ring: std::ptr::null(),
            samplerate: std::ptr::null(),
        };
        Self::open_with_api(
            descriptors.product,
            &manifest,
            |args, output| {
                // SAFETY: the retained plan owns this typed preparation and the
                // borrowed create records remain valid through the provider call.
                unsafe {
                    (plan.physical.create.expect("validated"))(plan.prepared.as_ptr(), args, output)
                }
            },
            Some(plan.providers.clone()),
            generation_id,
            maximum_frames,
            callbacks,
        )
    }

    fn open_with_api(
        api: &abi::UrpAstDescriptor,
        providers: &abi::UrpAstProviderManifest,
        create: impl FnOnce(&abi::UrpNativeCreateArgs, *mut *mut c_void) -> i32,
        physical_providers: Option<PhysicalProviders>,
        generation_id: u64,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<Self, RadioActivationError> {
        if callbacks.receive.is_none() || callbacks.transmit.is_none() || maximum_frames == 0 {
            return Err(RadioActivationError::InvalidCallbacks);
        }
        let (
            Some(start),
            Some(stop),
            Some(destroy),
            Some(site_bind),
            Some(site_publish),
            Some(site_snapshot),
        ) = (
            api.native_start,
            api.native_stop,
            api.native_destroy,
            api.native_site_bind,
            api.native_site_publish,
            api.native_site_snapshot,
        )
        else {
            return Err(RadioActivationError::Product(-2));
        };
        let feedback = callbacks.feedback;
        let callbacks = abi::UrpAstDirectCallbacks {
            struct_size: std::mem::size_of::<abi::UrpAstDirectCallbacks>() as u32,
            abi_version: abi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION,
            receive: callbacks.receive,
            receive_context: callbacks.receive_context,
            transmit: callbacks.transmit,
            transmit_context: callbacks.transmit_context,
            accepted_abi_version: 0,
            metadata: callbacks.metadata,
        };
        let args = abi::UrpNativeCreateArgs {
            struct_size: std::mem::size_of::<abi::UrpNativeCreateArgs>() as u32,
            abi_version: 1,
            config: std::ptr::null(),
            providers,
            callbacks: &callbacks,
            generation_id,
            maximum_frames,
        };
        let mut handle = std::ptr::null_mut();
        let status = create(&args, &mut handle);
        if status != 0 {
            if !handle.is_null() {
                // SAFETY: failure transferred a partial handle from this descriptor.
                unsafe { destroy(handle) };
            }
            return Err(RadioActivationError::Product(status));
        }
        let handle = NonNull::new(handle).ok_or(RadioActivationError::Product(-1))?;
        let active = Self {
            physical_providers,
            handle: Some(handle),
            api: retain_operations(api),
            update: UpdatePhase::Idle,
            stop,
            destroy,
            site_bind,
            site_publish,
            site_snapshot,
        };
        // SAFETY: the complete native owner is still stopped and the caller retains
        // the callback context until this station is synchronously retired.
        if let Some((bind, port)) = active.api.native_feedback_bind.zip(feedback) {
            let _ = unsafe { bind(handle.as_ptr(), &port) };
        }
        // SAFETY: successful create returns this stopped, uniquely owned station.
        let status = unsafe { start(handle.as_ptr()) };
        if status != 0 {
            return Err(RadioActivationError::Retained {
                status,
                active: Box::new(active),
            });
        }
        Ok(active)
    }

    /// Query the same native owner's typed physical status on serialized control.
    pub(crate) fn physical_status(&mut self, output: &mut abi::UrpNativePhysicalStatus) -> i32 {
        let Some((handle, read)) = self.handle.zip(self.api.native_get_status) else {
            return -7;
        };
        // SAFETY: the retained exclusive owner lends the result for this synchronous read.
        unsafe { read(handle.as_ptr(), output) }
    }

    /// Queue spare-pin ownership on the existing worker; snapshots confirm service.
    pub(crate) fn site_bind(&mut self, bank: u32, request: &abi::UrpAstSiteBinding) -> i32 {
        self.handle.map_or(-1, |handle| {
            // SAFETY: this owner retains the handle and the request is borrowed for this call.
            unsafe { (self.site_bind)(handle.as_ptr(), bank, request) }
        })
    }

    /// Queue steady output intent without opening a device or running I/O here.
    pub(crate) fn site_publish(&mut self, bank: u32, request: &abi::UrpAstSiteRequest) -> i32 {
        self.handle.map_or(-1, |handle| {
            // SAFETY: this owner serializes control operations on its retained native handle.
            unsafe { (self.site_publish)(handle.as_ptr(), bank, request) }
        })
    }

    /// Copy the hardware owner's observation without refreshing its acquisition time.
    pub(crate) fn site_snapshot(&mut self, bank: u32, output: &mut abi::UrpAstSiteSnapshot) -> i32 {
        self.handle.map_or(-1, |handle| {
            // SAFETY: the output is exclusively borrowed for this synchronous call.
            unsafe { (self.site_snapshot)(handle.as_ptr(), bank, output) }
        })
    }

    /// Pump the existing SDK control owner without a physical rig exchange.
    pub(crate) fn service(&mut self) -> i32 {
        self.handle.map_or(-1, |handle| {
            self.api.native_service.map_or(-7, |call| {
                // SAFETY: the serialized caller retains its native station.
                unsafe { call(handle.as_ptr()) }
            })
        })
    }
    /// Observe worker-side admission without waiting for a physical rig exchange.
    pub(crate) fn ready(&mut self) -> i32 {
        self.handle.map_or(-1, |handle| {
            self.api.native_activation_status.map_or(-7, |call| {
                // SAFETY: this unique control owner retains the native handle.
                unsafe { call(handle.as_ptr()) }
            })
        })
    }

    /// Keep callbacks and native ownership while physical retirement is pending.
    pub(crate) fn quiesce(&mut self) -> i32 {
        self.handle.map_or(0, |handle| {
            let call = self.api.native_quiesce.unwrap_or(self.stop);
            // SAFETY: no handle or callback is released by this poll.
            unsafe { call(handle.as_ptr()) }
        })
    }

    /// Copy intent into the shared physical owner's bounded queue.
    pub(crate) fn remote_submit(
        &mut self,
        operation: &abi::UrpRemoteOperation,
        flags: u32,
        output: &mut abi::rptadv_remote_ticket_v1,
    ) -> i32 {
        let (Some(handle), Some(submit), Some(source), Some(take), Some(destroy)) = (
            self.handle,
            self.api.native_remote_submit,
            self.api.remote_source,
            self.api.remote_take,
            self.api.remote_destroy,
        ) else {
            return -7;
        };
        let mut ticket = std::ptr::null_mut();
        // SAFETY: inputs are borrowed and success transfers one independent SDK ticket.
        let code = unsafe { submit(handle.as_ptr(), operation, flags, &mut ticket) };
        if code != 0 || ticket.is_null() {
            if !ticket.is_null() {
                // SAFETY: even an erroneous partial transfer retains its matching destructor.
                unsafe { destroy(ticket) };
            }
            return if code == 0 { -1 } else { code };
        }
        let mut ticket = rptadv_peer_support::remote_ticket::Ticket {
            context: ticket,
            source,
            take,
            destroy,
        };
        if let Some(providers) = &self.physical_providers {
            ticket = ticket.retaining(providers.clone());
        }
        *output = abi::rptadv_remote_ticket_v1 {
            struct_size: std::mem::size_of::<abi::rptadv_remote_ticket_v1>() as u32,
            abi_version: 1,
            context: ticket.context,
            source: Some(ticket.source),
            take: Some(ticket.take),
            destroy: Some(ticket.destroy),
        };
        0
    }
}

impl Drop for ActiveRadio {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: this sole owner invokes the provider's synchronous destroy
            // contract even when its best-effort preceding stop reports failure.
            unsafe {
                (self.stop)(handle.as_ptr());
                (self.destroy)(handle.as_ptr());
            }
        }
    }
}

impl ActiveRadio {
    fn begin_update(
        &mut self,
        plan: &StationPlan,
        generation_id: u64,
        maximum_frames: u32,
    ) -> Result<Option<NativeUpdateToken>, i32> {
        if self.update != UpdatePhase::Idle {
            return Err(-5);
        }
        let Some(handle) = self.handle else {
            return Err(-7);
        };
        let (Some(begin), Some(_), Some(_), Some(destroy)) = (
            self.api.native_begin_update,
            self.api.native_accept_update,
            self.api.native_finish_update,
            self.api.native_destroy_update,
        ) else {
            return Ok(None);
        };
        let args = abi::UrpNativeCreateArgs {
            struct_size: std::mem::size_of::<abi::UrpNativeCreateArgs>() as u32,
            abi_version: 1,
            config: std::ptr::null(),
            providers: std::ptr::null(),
            callbacks: std::ptr::null(),
            generation_id,
            maximum_frames,
        };
        let mut prepared = std::ptr::null_mut();
        // SAFETY: the native owner retains its providers and callbacks. The update
        // request and all source spans are borrowed only for this preparation.
        let code = unsafe {
            (plan.physical.prepare_update.expect("validated"))(
                handle.as_ptr(),
                plan.prepared.as_ptr(),
                &args,
                &mut prepared,
            )
        };
        let token = NativeUpdateToken {
            handle: prepared,
            destroy,
        };
        if code == -3 {
            return Ok(None);
        }
        if code != 0 {
            return Err(code);
        }
        if token.handle.is_null() {
            return Err(-1);
        }
        self.update = UpdatePhase::AwaitingAcceptance;
        // SAFETY: this uniquely owned token is transferred into its original
        // station mailbox. Destroying its emptied wrapper never frees live ports.
        let code = unsafe { begin(handle.as_ptr(), token.handle) };
        if code == 0 {
            Ok(Some(token))
        } else {
            // Begin can publish to native callbacks before a later step fails.
            // Keep the exact owner available for an explicit recovery finish.
            Err(code)
        }
    }

    fn accept_once(&self) -> i32 {
        let (Some(handle), Some(accept)) = (self.handle, self.api.native_accept_update) else {
            return -7;
        };
        // SAFETY: this serialized native station owns the same in-flight update.
        unsafe { accept(handle.as_ptr()) }
    }

    /// Stage on this native owner and poll once. Only a preparation-time incompatibility
    /// may request an untouched device handoff; an error after begin requires recovery.
    pub(crate) fn stage_typed(
        &mut self,
        plan: &crate::station_plan::StationPlan,
        generation: u64,
        maximum: u32,
    ) -> i32 {
        match self.begin_update(plan, generation, maximum) {
            Ok(Some(_token)) => self.poll_typed_stage(),
            Ok(None) => 1,
            Err(code) => {
                if self.update == UpdatePhase::AwaitingAcceptance {
                    self.update = UpdatePhase::Recovery;
                }
                if code < 0 { code } else { -7 }
            }
        }
    }

    pub(crate) fn poll_typed_stage(&mut self) -> i32 {
        match self.update {
            UpdatePhase::Staged => 0,
            UpdatePhase::AwaitingAcceptance => match self.accept_once() {
                0 => {
                    self.update = UpdatePhase::Staged;
                    0
                }
                1 => 2,
                code => {
                    self.update = UpdatePhase::Recovery;
                    if code < 0 { code } else { -7 }
                }
            },
            _ => -7,
        }
    }

    pub(crate) fn has_typed_update(&self) -> bool {
        self.update != UpdatePhase::Idle
    }

    pub(crate) fn candidate_acknowledged(&self) -> bool {
        matches!(
            self.update,
            UpdatePhase::Staged | UpdatePhase::Finishing(true)
        )
    }

    /// Poll one finish step. Reversal remains possible during partial adoption, but an
    /// unacknowledged or recovery-only edit can never be committed.
    pub(crate) fn finish_typed(&mut self, commit: bool) -> i32 {
        if !self.has_typed_update() || (commit && !self.candidate_acknowledged()) {
            return -7;
        }
        let (Some(handle), Some(finish)) = (self.handle, self.api.native_finish_update) else {
            return -7;
        };
        // SAFETY: the exact candidate and station remain retained between these bounded polls.
        match unsafe { finish(handle.as_ptr(), u32::from(commit)) } {
            0 => {
                self.update = UpdatePhase::Idle;
                0
            }
            1 => {
                self.update = UpdatePhase::Finishing(commit);
                2
            }
            code => {
                self.update = UpdatePhase::Recovery;
                if code < 0 { code } else { -7 }
            }
        }
    }

    /// Borrow the existing native owner for one typed command or session intent.
    /// Mutable command results and provider error codes cross this boundary intact.
    pub(crate) fn maintenance(
        &mut self,
        request: &mut abi::rptadv_radio_control_v1,
        sink: abi::rptadv_text_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        let Some(handle) = self.handle else {
            return -7;
        };
        // SAFETY: the serialized caller retains the native owner and request;
        // this composition retains every copied provider operation through drop.
        unsafe {
            match request.operation {
                1 => forward_text(self.api.native_status_text, handle.as_ptr(), sink, context),
                2 => self
                    .api
                    .native_command
                    .map_or(-7, |call| call(handle.as_ptr(), &mut request.command)),
                3 => self.api.native_set_transmit.map_or(-7, |call| {
                    call(
                        handle.as_ptr(),
                        request.keyed,
                        request.forced_ctcss_tenths_hz,
                    )
                }),
                4 => self
                    .api
                    .native_clear_manual
                    .map_or(-7, |call| call(handle.as_ptr())),
                5 => forward_text(
                    self.api.native_processing_status_text,
                    handle.as_ptr(),
                    sink,
                    context,
                ),
                _ => -1,
            }
        }
    }
}

struct NativeUpdateToken {
    handle: *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
}
impl Drop for NativeUpdateToken {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: preparation transferred this wrapper exactly once.
            unsafe { (self.destroy)(self.handle) };
        }
    }
}

fn retain_operations(api: &abi::UrpAstDescriptor) -> abi::UrpAstDescriptor {
    // SAFETY: the flat descriptor contains only nullable pointers and scalar fields.
    let mut retained: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
    macro_rules! complete {
        ($($field:ident),+ $(,)?) => { $(
            if api.struct_size as usize >= std::mem::offset_of!(abi::UrpAstDescriptor, $field)
                + std::mem::size_of::<Option<unsafe extern "C" fn()>>() {
                retained.$field = api.$field;
            }
        )+ };
    }
    complete!(
        native_status_text,
        native_get_status,
        native_feedback_bind,
        native_command,
        native_set_transmit,
        native_clear_manual,
        native_begin_update,
        native_accept_update,
        native_finish_update,
        native_destroy_update,
        native_processing_status_text,
        native_service,
        native_quiesce,
        native_remote_submit,
        remote_source,
        remote_take,
        remote_destroy,
        native_activation_status
    );
    retained
}

unsafe fn forward_text(
    operation: Option<
        unsafe extern "C" fn(*mut c_void, abi::UrpNativeProcessingDiagnostic, *mut c_void) -> i32,
    >,
    handle: *mut c_void,
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    let Some(operation) = operation else {
        return -7;
    };
    let Some(sink) = sink else {
        return -1;
    };
    struct TextSink {
        call: unsafe extern "C" fn(*mut c_void, *const std::ffi::c_char, usize),
        context: *mut c_void,
        invalid: bool,
    }
    unsafe extern "C" fn append(context: *mut c_void, text: *const u8, length: u32) {
        // SAFETY: forward_text retains this bridge through the synchronous call.
        let sink = unsafe { &mut *context.cast::<TextSink>() };
        if (text.is_null() && length != 0) || length > 65_536 {
            sink.invalid = true;
            return;
        }
        // SAFETY: the shared provider lends this complete span until callback return.
        unsafe { (sink.call)(sink.context, text.cast(), length as usize) };
    }
    let mut bridge = TextSink {
        call: sink,
        context,
        invalid: false,
    };
    // SAFETY: the native handle, executable operation and bridge remain retained.
    let code = unsafe { operation(handle, Some(append), std::ptr::from_mut(&mut bridge).cast()) };
    if code == 0 && bridge.invalid {
        -1
    } else {
        code
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    thread_local! {
        static CALLS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static RESULTS: Cell<(i32, bool, i32)> = const { Cell::new((0, true, 0)) };
        static REMOTE_RESULT: Cell<(i32, bool)> = const { Cell::new((0, true)) };
    }
    unsafe extern "C" fn create(
        args: *const abi::UrpNativeCreateArgs,
        output: *mut *mut c_void,
    ) -> i32 {
        CALLS.with(|calls| calls.borrow_mut().push("create"));
        // SAFETY: open_with_api retains all nested request records for this call.
        let (args, providers, callbacks) = unsafe {
            let args = &*args;
            (args, &*args.providers, &*args.callbacks)
        };
        assert_eq!(args.struct_size as usize, std::mem::size_of_val(args));
        assert_eq!(
            (args.abi_version, args.generation_id, args.maximum_frames),
            (1, 73, 128)
        );
        assert!(args.config.is_null());
        assert_eq!(providers.abi_version, 1);
        assert_eq!(
            callbacks.abi_version,
            abi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION
        );
        assert_eq!(
            callbacks.struct_size as usize,
            std::mem::size_of_val(callbacks)
        );
        assert_eq!(
            callbacks.receive_context,
            NonNull::<u8>::dangling().as_ptr().cast()
        );
        assert_eq!(callbacks.transmit_context, callbacks.receive_context);
        assert!(callbacks.receive.is_some() && callbacks.transmit.is_some());
        let (status, handle, _) = RESULTS.get();
        // SAFETY: the client supplies one writable handle destination.
        unsafe {
            *output = if handle {
                NonNull::<u8>::dangling().as_ptr().cast()
            } else {
                std::ptr::null_mut()
            }
        };
        status
    }
    unsafe extern "C" fn start(_: *mut c_void) -> i32 {
        CALLS.with(|calls| calls.borrow_mut().push("start"));
        RESULTS.get().2
    }
    unsafe extern "C" fn receive(
        _: *mut c_void,
        _: *const abi::UrpNativeReceiveMetadata,
        _: *mut f32,
        _: u32,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn transmit(
        _: *mut c_void,
        _: *mut f32,
        _: u32,
        _: *mut u32,
        _: *mut u32,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn stop(_: *mut c_void) -> i32 {
        CALLS.with(|calls| calls.borrow_mut().push("stop"));
        0
    }

    #[test]
    fn native_quiesce_retains_the_station_until_the_physical_owner_finishes() {
        thread_local! { static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
        unsafe extern "C" fn quiesce(_: *mut c_void) -> i32 {
            CALLS.with(|calls| calls.borrow_mut().push("quiesce"));
            if READY.get() { 0 } else { 1 }
        }
        CALLS.with(|calls| calls.borrow_mut().clear());
        // SAFETY: flat nullable descriptor; only the declared fake callbacks are used.
        let mut api: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.struct_size = std::mem::size_of::<abi::UrpAstDescriptor>() as u32;
        api.native_quiesce = Some(quiesce);
        let mut radio = fixture_active_radio(api);
        assert_eq!(radio.quiesce(), 1);
        assert!(radio.handle.is_some());
        READY.set(true);
        assert_eq!(radio.quiesce(), 0);
        CALLS.with(|calls| assert_eq!(&*calls.borrow(), &["quiesce", "quiesce"]));
        drop(radio);
        CALLS
            .with(|calls| assert_eq!(&*calls.borrow(), &["quiesce", "quiesce", "stop", "destroy"]));
    }
    unsafe extern "C" fn destroy(_: *mut c_void) {
        CALLS.with(|calls| calls.borrow_mut().push("destroy"));
    }

    unsafe extern "C" fn site_bind(
        _: *mut c_void,
        _: u32,
        _: *const abi::UrpAstSiteBinding,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn site_publish(
        _: *mut c_void,
        _: u32,
        _: *const abi::UrpAstSiteRequest,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn site_snapshot(
        _: *mut c_void,
        _: u32,
        _: *mut abi::UrpAstSiteSnapshot,
    ) -> i32 {
        -1
    }

    pub(crate) fn empty_active_radio() -> ActiveRadio {
        ActiveRadio {
            physical_providers: None,
            handle: None,
            // SAFETY: test operations are nullable pointers until assigned explicitly.
            api: unsafe { std::mem::zeroed() },
            update: UpdatePhase::Idle,
            stop,
            destroy,
            site_bind,
            site_publish,
            site_snapshot,
        }
    }

    pub(crate) fn fixture_station_radio(
        api: abi::UrpAstDescriptor,
        providers: crate::providers::PhysicalProviders,
    ) -> ActiveRadio {
        let mut active = fixture_active_radio(api);
        active.physical_providers = Some(providers);
        active
    }

    pub(crate) fn fixture_active_radio(api: abi::UrpAstDescriptor) -> ActiveRadio {
        let mut active = empty_active_radio();
        active.handle = Some(NonNull::<u8>::dangling().cast());
        active.api = api;
        active
    }

    unsafe extern "C" fn remote_service(handle: *mut c_void) -> i32 {
        assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
        CALLS.with(|calls| calls.borrow_mut().push("service"));
        -31
    }
    unsafe extern "C" fn remote_ready(handle: *mut c_void) -> i32 {
        assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
        CALLS.with(|calls| calls.borrow_mut().push("ready"));
        1
    }
    unsafe extern "C" fn remote_quiesce(handle: *mut c_void) -> i32 {
        assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
        CALLS.with(|calls| calls.borrow_mut().push("quiesce"));
        0
    }
    unsafe extern "C" fn remote_submit(
        handle: *mut c_void,
        operation: *const abi::UrpRemoteOperation,
        flags: u32,
        output: *mut *mut c_void,
    ) -> i32 {
        assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
        // SAFETY: the client lends the complete operation and writable ticket destination.
        assert_eq!(unsafe { (*operation).operation }, 1);
        assert_eq!(flags, 3);
        CALLS.with(|calls| calls.borrow_mut().push("submit"));
        let (code, transferred) = REMOTE_RESULT.get();
        // SAFETY: this synchronous callback supplies one opaque fixture ticket or null.
        unsafe {
            *output = if transferred {
                NonNull::<u32>::dangling().as_ptr().cast()
            } else {
                std::ptr::null_mut()
            };
        }
        code
    }
    unsafe extern "C" fn ticket_source(ticket: *mut c_void, flags: u32) -> i32 {
        assert_eq!(ticket, NonNull::<u32>::dangling().as_ptr().cast());
        assert_eq!(flags, 2);
        CALLS.with(|calls| calls.borrow_mut().push("source"));
        0
    }
    unsafe extern "C" fn ticket_take(
        ticket: *mut c_void,
        _: abi::UrpNativeProcessingDiagnostic,
        _: *mut c_void,
    ) -> i32 {
        assert_eq!(ticket, NonNull::<u32>::dangling().as_ptr().cast());
        CALLS.with(|calls| calls.borrow_mut().push("take"));
        0
    }
    unsafe extern "C" fn ticket_destroy(ticket: *mut c_void) {
        assert_eq!(ticket, NonNull::<u32>::dangling().as_ptr().cast());
        CALLS.with(|calls| calls.borrow_mut().push("ticket_destroy"));
    }

    pub(crate) fn remote_fixture_api() -> abi::UrpAstDescriptor {
        REMOTE_RESULT.set((0, true));
        // SAFETY: the flat descriptor's unused operations are nullable pointers.
        let mut api: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.struct_size = std::mem::size_of_val(&api) as u32;
        api.abi_version = 1;
        api.native_service = Some(remote_service);
        api.native_activation_status = Some(remote_ready);
        api.native_quiesce = Some(remote_quiesce);
        api.native_remote_submit = Some(remote_submit);
        api.remote_source = Some(ticket_source);
        api.remote_take = Some(ticket_take);
        api.remote_destroy = Some(ticket_destroy);
        api
    }

    pub(crate) fn remote_operation() -> abi::UrpRemoteOperation {
        abi::UrpRemoteOperation {
            struct_size: std::mem::size_of::<abi::UrpRemoteOperation>() as u32,
            abi_version: 1,
            operation: 1,
            selector: 0,
            signed_value: 0,
            magnitude: 0,
            decimal_places: 0,
            flags: 0,
        }
    }

    #[test]
    fn native_remote_control_preserves_status_and_independent_ticket_ownership() {
        let operation = remote_operation();
        let mut radio = empty_active_radio();
        assert_eq!(radio.service(), -1);
        assert_eq!(radio.ready(), -1);
        radio.handle = Some(NonNull::<u8>::dangling().cast());
        assert_eq!(radio.service(), -7);
        assert_eq!(radio.ready(), -7);
        radio.api = remote_fixture_api();
        assert_eq!(radio.service(), -31);
        assert_eq!(radio.ready(), 1);
        for missing in 0..5 {
            radio.api = remote_fixture_api();
            CALLS.with(|calls| calls.borrow_mut().clear());
            match missing {
                0 => radio.handle = None,
                1 => radio.api.native_remote_submit = None,
                2 => radio.api.remote_source = None,
                3 => radio.api.remote_take = None,
                _ => radio.api.remote_destroy = None,
            }
            // SAFETY: an empty ticket consists solely of valid scalar/pointer values.
            let mut output: abi::rptadv_remote_ticket_v1 = unsafe { std::mem::zeroed() };
            output.context = NonNull::<u64>::dangling().as_ptr().cast();
            assert_eq!(radio.remote_submit(&operation, 3, &mut output), -7);
            assert_eq!(output.context, NonNull::<u64>::dangling().as_ptr().cast());
            CALLS.with(|calls| {
                assert!(
                    calls.borrow().is_empty(),
                    "incomplete SDK tables must not dispatch"
                )
            });
            radio.handle = Some(NonNull::<u8>::dangling().cast());
        }
        radio.api = remote_fixture_api();
        for (code, transferred, expected) in [
            (0, true, 0),
            (-9, true, -9),
            (-9, false, -9),
            (0, false, -1),
        ] {
            REMOTE_RESULT.set((code, transferred));
            CALLS.with(|calls| calls.borrow_mut().clear());
            // SAFETY: the caller initializes complete writable ticket output storage.
            let mut output: abi::rptadv_remote_ticket_v1 = unsafe { std::mem::zeroed() };
            assert_eq!(radio.remote_submit(&operation, 3, &mut output), expected);
            if expected == 0 {
                assert_eq!(output.struct_size as usize, std::mem::size_of_val(&output));
                assert_eq!(output.abi_version, 1);
                // SAFETY: success transferred this ticket and its matching lifecycle table.
                unsafe {
                    assert_eq!(output.source.unwrap()(output.context, 2), 0);
                    assert_eq!(
                        output.take.unwrap()(output.context, None, std::ptr::null_mut()),
                        0
                    );
                    output.destroy.unwrap()(output.context);
                }
                CALLS.with(|calls| {
                    assert_eq!(
                        &*calls.borrow(),
                        &["submit", "source", "take", "ticket_destroy"]
                    )
                });
            } else {
                assert!(output.context.is_null());
                CALLS.with(|calls| {
                    assert_eq!(
                        &*calls.borrow(),
                        if transferred {
                            &["submit", "ticket_destroy"][..]
                        } else {
                            &["submit"][..]
                        }
                    )
                });
            }
        }
    }

    #[test]
    fn maintenance_forwards_mutable_commands_and_manual_cleanup_to_the_same_handle() {
        unsafe extern "C" fn command(
            handle: *mut c_void,
            command: *mut abi::UrpAstChannelCommand,
        ) -> i32 {
            assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
            // SAFETY: the caller retains this complete mutable command record.
            let command = unsafe { &mut *command };
            assert_eq!(
                (command.command, command.target, command.value),
                (2, 1, 222)
            );
            command.value = 333;
            command.flags = 3;
            command.eeprom_words[60] = 654;
            0
        }
        unsafe extern "C" fn manual(handle: *mut c_void, keyed: u32, tone: u32) -> i32 {
            assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
            assert_eq!((keyed, tone), (1, 1_000));
            CALLS.with(|calls| calls.borrow_mut().push("manual"));
            -9
        }
        unsafe extern "C" fn clear(_: *mut c_void) -> i32 {
            CALLS.with(|calls| calls.borrow_mut().push("clear"));
            0
        }
        unsafe extern "C" fn text(
            _: *mut c_void,
            sink: abi::UrpNativeProcessingDiagnostic,
            context: *mut c_void,
        ) -> i32 {
            // SAFETY: the caller retains the synchronous diagnostic callback.
            unsafe { sink.unwrap()(context, b"native=ready\n".as_ptr(), 13) };
            0
        }
        unsafe extern "C" fn append(
            context: *mut c_void,
            text: *const std::ffi::c_char,
            length: usize,
        ) {
            // SAFETY: the fixture forwards its complete borrowed text span.
            unsafe { &mut *context.cast::<String>() }.push_str(
                std::str::from_utf8(unsafe {
                    std::slice::from_raw_parts(text.cast::<u8>(), length)
                })
                .unwrap(),
            );
        }
        let mut active = empty_active_radio();
        active.handle = Some(NonNull::<u8>::dangling().cast());
        active.api.native_command = Some(command);
        active.api.native_set_transmit = Some(manual);
        active.api.native_clear_manual = Some(clear);
        active.api.native_status_text = Some(text);
        active.api.native_processing_status_text = Some(text);
        // SAFETY: this request is a flat scalar/nullable-pointer ABI record.
        let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
        request.struct_size = std::mem::size_of_val(&request) as u32;
        request.abi_version = 1;
        request.operation = 2;
        request.command.struct_size = std::mem::size_of_val(&request.command) as u32;
        request.command.abi_version = 1;
        request.command.command = 2;
        request.command.target = 1;
        request.command.value = 222;
        CALLS.with(|calls| calls.borrow_mut().clear());
        assert_eq!(
            active.maintenance(&mut request, None, std::ptr::null_mut()),
            0
        );
        assert_eq!(
            (
                request.command.value,
                request.command.flags,
                request.command.eeprom_words[60]
            ),
            (333, 3, 654)
        );
        request.operation = 3;
        request.keyed = 1;
        request.forced_ctcss_tenths_hz = 1_000;
        assert_eq!(
            active.maintenance(&mut request, None, std::ptr::null_mut()),
            -9
        );
        request.operation = 4;
        assert_eq!(
            active.maintenance(&mut request, None, std::ptr::null_mut()),
            0
        );
        CALLS.with(|calls| assert_eq!(&*calls.borrow(), &["manual", "clear"]));
        let mut output = String::new();
        for operation in [1, 5] {
            request.operation = operation;
            assert_eq!(
                active.maintenance(
                    &mut request,
                    Some(append),
                    std::ptr::from_mut(&mut output).cast()
                ),
                0
            );
        }
        assert_eq!(output, "native=ready\nnative=ready\n");
        request.operation = 0;
        assert_eq!(
            active.maintenance(&mut request, None, std::ptr::null_mut()),
            -1
        );
        active.handle = None;
        assert_eq!(
            active.maintenance(&mut request, None, std::ptr::null_mut()),
            -7
        );
    }

    #[test]
    fn optional_native_operations_require_their_complete_published_tail() {
        unsafe extern "C" fn clear(_: *mut c_void) -> i32 {
            0
        }
        // SAFETY: the descriptor is a flat nullable-pointer record.
        let mut api: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.native_clear_manual = Some(clear);
        let boundary = std::mem::offset_of!(abi::UrpAstDescriptor, native_clear_manual);
        api.struct_size = boundary as u32;
        assert!(retain_operations(&api).native_clear_manual.is_none());
        api.struct_size += std::mem::size_of_val(&api.native_clear_manual) as u32;
        assert!(retain_operations(&api).native_clear_manual.is_some());
    }

    #[test]
    fn diagnostic_text_bridge_preserves_provider_errors_and_bounds_borrowed_spans() {
        unsafe extern "C" fn diagnostic(
            owner: *mut c_void,
            sink: abi::UrpNativeProcessingDiagnostic,
            context: *mut c_void,
        ) -> i32 {
            // SAFETY: the synchronous test retains this provider's mode record.
            let (mode, status) = unsafe { *owner.cast::<(u8, i32)>() };
            let sink = sink.unwrap();
            // SAFETY: malformed spans are deliberately rejected before forwarding.
            unsafe {
                match mode {
                    0 => sink(context, b"ready\n".as_ptr(), 6),
                    1 => sink(context, std::ptr::null(), 1),
                    2 => sink(context, NonNull::<u8>::dangling().as_ptr(), 65_537),
                    _ => sink(context, std::ptr::null(), 0),
                }
            };
            status
        }
        unsafe extern "C" fn append(
            context: *mut c_void,
            text: *const std::ffi::c_char,
            length: usize,
        ) {
            // SAFETY: the bridge forwards only the fixture's valid complete text span.
            let bytes = if length == 0 {
                &[]
            } else {
                // SAFETY: nonempty spans remain fully readable during this callback.
                unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) }
            };
            unsafe { &mut *context.cast::<String>() }.push_str(std::str::from_utf8(bytes).unwrap());
        }
        for mode in 0..4 {
            for status in [0, -9] {
                let mut owner = (mode, status);
                let mut output = String::new();
                // SAFETY: both stack contexts and executable callbacks outlive this synchronous call.
                let actual = unsafe {
                    forward_text(
                        Some(diagnostic),
                        std::ptr::from_mut(&mut owner).cast(),
                        Some(append),
                        std::ptr::from_mut(&mut output).cast(),
                    )
                };
                assert_eq!(
                    actual,
                    if status != 0 {
                        status
                    } else if matches!(mode, 0 | 3) {
                        0
                    } else {
                        -1
                    }
                );
                assert_eq!(output, if mode == 0 { "ready\n" } else { "" });
            }
        }
        // SAFETY: missing optional operations/sinks reject before dereferencing contexts.
        unsafe {
            assert_eq!(
                forward_text(
                    None,
                    std::ptr::null_mut(),
                    Some(append),
                    std::ptr::null_mut()
                ),
                -7
            );
            assert_eq!(
                forward_text(
                    Some(diagnostic),
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null_mut()
                ),
                -1
            );
        }
    }

    #[test]
    fn site_bridge_forwards_on_the_same_native_owner() {
        unsafe extern "C" fn bind(
            handle: *mut c_void,
            bank: u32,
            request: *const abi::UrpAstSiteBinding,
        ) -> i32 {
            assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
            assert_eq!(bank, 1);
            let request = unsafe { &*request };
            assert_eq!((request.generation, request.output_mask), (73, 4));
            assert_eq!((request.initial_high_mask, request.safe_high_mask), (4, 0));
            -3
        }
        unsafe extern "C" fn publish(
            handle: *mut c_void,
            bank: u32,
            request: *const abi::UrpAstSiteRequest,
        ) -> i32 {
            assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
            assert_eq!(bank, 1);
            let request = unsafe { &*request };
            assert_eq!(
                (request.generation, request.output_mask, request.high_mask),
                (74, 4, 4)
            );
            -4
        }
        unsafe extern "C" fn snapshot(
            handle: *mut c_void,
            bank: u32,
            output: *mut abi::UrpAstSiteSnapshot,
        ) -> i32 {
            assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
            assert_eq!(bank, 1);
            unsafe { (*output).generation = 74 };
            0
        }
        CALLS.with(|calls| calls.borrow_mut().clear());
        let mut radio = empty_active_radio();
        radio.handle = Some(NonNull::<u8>::dangling().cast());
        radio.site_bind = bind;
        radio.site_publish = publish;
        radio.site_snapshot = snapshot;
        let binding = abi::UrpAstSiteBinding {
            struct_size: std::mem::size_of::<abi::UrpAstSiteBinding>() as u32,
            abi_version: 1,
            generation: 73,
            output_mask: 4,
            initial_high_mask: 4,
            safe_high_mask: 0,
        };
        let request = abi::UrpAstSiteRequest {
            struct_size: std::mem::size_of::<abi::UrpAstSiteRequest>() as u32,
            abi_version: 1,
            generation: 74,
            output_mask: 4,
            high_mask: 4,
        };
        let mut observed: abi::UrpAstSiteSnapshot = unsafe { std::mem::zeroed() };
        assert_eq!(radio.site_bind(1, &binding), -3);
        assert_eq!(radio.site_publish(1, &request), -4);
        assert_eq!(radio.site_snapshot(1, &mut observed), 0);
        assert_eq!(observed.generation, 74);
        CALLS.with(|calls| assert!(calls.borrow().is_empty(), "site I/O must not open hardware"));
        drop(radio);
        CALLS.with(|calls| assert_eq!(&*calls.borrow(), &["stop", "destroy"]));
    }

    fn fixture_open(
        api: &abi::UrpAstDescriptor,
        providers: &abi::UrpAstProviderManifest,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<ActiveRadio, RadioActivationError> {
        ActiveRadio::open_with_api(
            api,
            providers,
            |args, output| {
                // SAFETY: the fixture receives the same live request and exclusive output
                // as the synchronous typed provider.
                unsafe { create(args, output) }
            },
            None,
            73,
            maximum_frames,
            callbacks,
        )
    }

    #[test]
    fn native_client_forwards_request_and_releases_each_transferred_handle() {
        // SAFETY: these C records contain only nullable pointers and scalar values.
        let (mut api, mut providers): (abi::UrpAstDescriptor, abi::UrpAstProviderManifest) =
            unsafe { std::mem::zeroed() };
        unsafe extern "C" fn publish_feedback(_: *mut c_void, _: *const abi::UrpRadioFeedback) {}
        unsafe extern "C" fn bind_feedback(
            _: *mut c_void,
            port: *const abi::UrpRadioFeedbackPort,
        ) -> i32 {
            // SAFETY: the serial fixture receives the complete borrowed SDK port.
            let port = unsafe { &*port };
            assert!(port.publish.is_some());
            assert_eq!(port.abi_version, abi::URP_RADIO_FEEDBACK_ABI_VERSION);
            CALLS.with(|calls| calls.borrow_mut().push("feedback"));
            0
        }
        api.struct_size = std::mem::size_of::<abi::UrpAstDescriptor>() as u32;
        api.native_feedback_bind = Some(bind_feedback);
        api.native_start = Some(start);
        api.native_stop = Some(stop);
        api.native_destroy = Some(destroy);
        api.native_site_bind = Some(site_bind);
        api.native_site_publish = Some(site_publish);
        api.native_site_snapshot = Some(site_snapshot);
        providers.abi_version = 1;
        for (scenario, expected_status, expected_calls) in [
            (
                (0, true, 0),
                None,
                &["create", "feedback", "start", "stop", "destroy"][..],
            ),
            (
                (0, true, -8),
                Some(-8),
                &["create", "feedback", "start", "stop", "destroy"][..],
            ),
            ((-7, true, 0), Some(-7), &["create", "destroy"][..]),
            ((-7, false, 0), Some(-7), &["create"][..]),
            ((0, false, 0), Some(-1), &["create"][..]),
        ] {
            CALLS.with(|calls| calls.borrow_mut().clear());
            RESULTS.set(scenario);
            let callbacks = ProductRadioCallbacks {
                receive: Some(receive),
                receive_context: NonNull::<u8>::dangling().as_ptr().cast(),
                transmit: Some(transmit),
                transmit_context: NonNull::<u8>::dangling().as_ptr().cast(),
                metadata: None,
                feedback: Some(abi::UrpRadioFeedbackPort {
                    struct_size: std::mem::size_of::<abi::UrpRadioFeedbackPort>() as u32,
                    abi_version: abi::URP_RADIO_FEEDBACK_ABI_VERSION,
                    context: std::ptr::null_mut(),
                    publish: Some(publish_feedback),
                }),
            };
            let result = fixture_open(&api, &providers, 128, callbacks);
            let status = result.as_ref().err().map(|error| match error {
                RadioActivationError::Product(status)
                | RadioActivationError::Retained { status, .. } => *status,
                error => panic!("unexpected client error: {error}"),
            });
            assert_eq!(status, expected_status);
            if scenario == (0, true, -8) {
                CALLS.with(|calls| {
                    assert_eq!(
                        &*calls.borrow(),
                        &["create", "feedback", "start"],
                        "a failed startup retains its callback owner until checked cleanup"
                    )
                });
            }
            drop(result);
            CALLS.with(|calls| assert_eq!(&*calls.borrow(), expected_calls));
        }
    }

    #[test]
    fn native_client_rejects_missing_callbacks_and_operations_before_creation() {
        CALLS.with(|calls| calls.borrow_mut().clear());
        // SAFETY: these C records contain only nullable pointers and scalar values.
        let (mut api, providers): (abi::UrpAstDescriptor, abi::UrpAstProviderManifest) =
            unsafe { std::mem::zeroed() };
        api.native_start = Some(start);
        api.native_stop = Some(stop);
        api.native_destroy = Some(destroy);
        api.native_site_bind = Some(site_bind);
        api.native_site_publish = Some(site_publish);
        api.native_site_snapshot = Some(site_snapshot);
        let callbacks = || ProductRadioCallbacks {
            receive: Some(receive),
            receive_context: std::ptr::null_mut(),
            transmit: Some(transmit),
            transmit_context: std::ptr::null_mut(),
            metadata: None,
            feedback: None,
        };
        let mut missing_receive = callbacks();
        missing_receive.receive = None;
        assert!(matches!(
            fixture_open(&api, &providers, 128, missing_receive),
            Err(RadioActivationError::InvalidCallbacks)
        ));
        let mut missing_transmit = callbacks();
        missing_transmit.transmit = None;
        assert!(matches!(
            fixture_open(&api, &providers, 128, missing_transmit),
            Err(RadioActivationError::InvalidCallbacks)
        ));
        assert!(matches!(
            fixture_open(&api, &providers, 0, callbacks()),
            Err(RadioActivationError::InvalidCallbacks)
        ));
        api.native_start = None;
        assert!(matches!(
            fixture_open(&api, &providers, 128, callbacks()),
            Err(RadioActivationError::Product(-2))
        ));
        assert_eq!(
            RadioActivationError::Provider(crate::providers::ProviderError::Load("radio"))
                .to_string(),
            "cannot load required provider radio"
        );
        CALLS.with(|calls| assert!(calls.borrow().is_empty()));
    }
}
