//! Thin ownership of one native station in the shared USBRadioPlus product.

use crate::{ResolvedRadioNode, abi, providers::ProviderSet};
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
    pub receive: abi::rptadv_radio_receive_v2,
    /// Caller-owned receive context.
    pub receive_context: *mut c_void,
    /// Exact-frame transmit audio and PTT/CTCSS callback.
    pub transmit: abi::rptadv_radio_transmit_v3,
    /// Caller-owned transmit context.
    pub transmit_context: *mut c_void,
    /// Same serial transmit-owner policy/activity endpoint.
    pub metadata: abi::rptadv_radio_metadata_v1,
}

/// Native station handle; its shared product owns every hardware and DSP resource.
pub struct ActiveRadio {
    handle: Option<NonNull<c_void>>,
    api: abi::UrpAstDescriptor,
    updating: bool,
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
            .field("updating", &self.updating)
            .finish_non_exhaustive()
    }
}

impl ActiveRadio {
    /// Copy resolved policy into the product, then start its native hardware path.
    /// Providers and callback contexts remain alive until this owner is dropped.
    pub fn open(
        providers: &'static ProviderSet,
        radio: &ResolvedRadioNode,
        generation_id: u64,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<Self, RadioActivationError> {
        let descriptors = providers
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
            &radio.radio.request(),
            generation_id,
            maximum_frames,
            callbacks,
        )
    }

    fn open_with_api(
        api: &abi::UrpAstDescriptor,
        providers: &abi::UrpAstProviderManifest,
        config: &abi::UrpNativeStationConfig,
        generation_id: u64,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<Self, RadioActivationError> {
        if callbacks.receive.is_none() || callbacks.transmit.is_none() || maximum_frames == 0 {
            return Err(RadioActivationError::InvalidCallbacks);
        }
        let (
            Some(create),
            Some(start),
            Some(stop),
            Some(destroy),
            Some(site_bind),
            Some(site_publish),
            Some(site_snapshot),
        ) = (
            api.native_create,
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
            config,
            providers,
            callbacks: &callbacks,
            generation_id,
            maximum_frames,
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: all request spans are retained for this synchronous create;
        // providers and callback contexts outlive the resulting native handle.
        let status = unsafe { create(&args, &mut handle) };
        if status != 0 {
            if !handle.is_null() {
                // SAFETY: failure transferred a partial handle from this descriptor.
                unsafe { destroy(handle) };
            }
            return Err(RadioActivationError::Product(status));
        }
        let handle = NonNull::new(handle).ok_or(RadioActivationError::Product(-1))?;
        let active = Self {
            handle: Some(handle),
            api: retain_operations(api),
            updating: false,
            stop,
            destroy,
            site_bind,
            site_publish,
            site_snapshot,
        };
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
        *output = abi::rptadv_remote_ticket_v1 {
            struct_size: std::mem::size_of::<abi::rptadv_remote_ticket_v1>() as u32,
            abi_version: 1,
            context: ticket,
            source: Some(source),
            take: Some(take),
            destroy: Some(destroy),
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
    /// Stage a prepared same-device radio edit and wait for the shared owners'
    /// independent acknowledgments. Its reverse remains retained until finish.
    pub(crate) fn stage_update(
        &mut self,
        config: &abi::UrpNativeStationConfig,
        generation_id: u64,
        maximum_frames: u32,
    ) -> Result<bool, i32> {
        if self.updating {
            return Err(-5);
        }
        let Some(handle) = self.handle else {
            return Err(-7);
        };
        let (Some(prepare), Some(begin), Some(accept), Some(_), Some(destroy)) = (
            self.api.native_prepare_update,
            self.api.native_begin_update,
            self.api.native_accept_update,
            self.api.native_finish_update,
            self.api.native_destroy_update,
        ) else {
            return Ok(false);
        };
        let args = abi::UrpNativeCreateArgs {
            struct_size: std::mem::size_of::<abi::UrpNativeCreateArgs>() as u32,
            abi_version: 1,
            config,
            providers: std::ptr::null(),
            callbacks: std::ptr::null(),
            generation_id,
            maximum_frames,
        };
        let mut prepared = std::ptr::null_mut();
        // SAFETY: the native owner retains its providers and callbacks. The update
        // request and all source spans are borrowed only for this preparation.
        let code = unsafe { prepare(handle.as_ptr(), &args, &mut prepared) };
        let token = NativeUpdateToken {
            handle: prepared,
            destroy,
        };
        if code == -3 {
            return Ok(false);
        }
        if code != 0 {
            return Err(code);
        }
        if token.handle.is_null() {
            return Err(-1);
        }
        self.updating = true;
        // SAFETY: this uniquely owned token is transferred into its original
        // station mailbox. Destroying its emptied wrapper never frees live ports.
        let code = unsafe { begin(handle.as_ptr(), token.handle) };
        let code = if code == 0 {
            // SAFETY: the original serialized station remains retained while polling.
            await_update(|| unsafe { accept(handle.as_ptr()) })
        } else {
            code
        };
        if code != 0 {
            // Aggregate rollback owns the abort and can gate the controller if
            // restoring this retained owner fails. Keep partial adoption alive.
            return Err(code);
        }
        Ok(true)
    }

    /// Commit only after aggregate admission, or use the shared prepared reverse
    /// to restore the exact active processors after any later device fails.
    pub(crate) fn finish_update(&mut self, commit: bool) -> Result<(), i32> {
        if !self.updating {
            return Ok(());
        }
        let handle = self.handle.ok_or(-7)?;
        let finish = self.api.native_finish_update.ok_or(-2)?;
        // SAFETY: the same owner retains its bounded transaction during ACK polling.
        let code = await_update(|| unsafe { finish(handle.as_ptr(), u32::from(commit)) });
        if code != 0 {
            return Err(code);
        }
        self.updating = false;
        Ok(())
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

fn await_update(mut operation: impl FnMut() -> i32) -> i32 {
    // Use the existing two-second physical-control response bound, off audio.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let code = operation();
        if code != 1 {
            return code;
        }
        if std::time::Instant::now() >= deadline {
            return -7;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
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
        native_command,
        native_set_transmit,
        native_clear_manual,
        native_prepare_update,
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
        static EXPECTED_RADIO: Cell<Option<*const c_void>> = const { Cell::new(None) };
    }
    unsafe extern "C" fn create(
        args: *const abi::UrpNativeCreateArgs,
        output: *mut *mut c_void,
    ) -> i32 {
        CALLS.with(|calls| calls.borrow_mut().push("create"));
        // SAFETY: open_with_api retains all nested request records for this call.
        let (args, config, providers, callbacks) = unsafe {
            let args = &*args;
            (args, &*args.config, &*args.providers, &*args.callbacks)
        };
        assert_eq!(args.struct_size as usize, std::mem::size_of_val(args));
        assert_eq!(
            (args.abi_version, args.generation_id, args.maximum_frames),
            (1, 73, 128)
        );
        assert_eq!(config.receive_output_gain_db, -8);
        assert_eq!(providers.abi_version, 1);
        if let Some(expected) = EXPECTED_RADIO.get() {
            assert_eq!(providers.radio, expected);
            // SAFETY: this test retains the complete published descriptor while
            // the native owner borrows its full session table, not only its header.
            let radio = unsafe {
                &*providers
                    .radio
                    .cast::<crate::providers::tests::PublishedRadioDescriptor>()
            };
            assert_eq!(radio.struct_size, 112);
            assert_eq!(radio.abi_version, 4);
        }
        assert_eq!(callbacks.abi_version, 5);
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
    unsafe extern "C" fn receive(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
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
            handle: None,
            // SAFETY: test operations are nullable pointers until assigned explicitly.
            api: unsafe { std::mem::zeroed() },
            updating: false,
            stop,
            destroy,
            site_bind,
            site_publish,
            site_snapshot,
        }
    }

    pub(crate) fn fixture_active_radio(api: abi::UrpAstDescriptor) -> ActiveRadio {
        let mut active = empty_active_radio();
        active.handle = Some(NonNull::<u8>::dangling().cast());
        active.api = api;
        active
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
    fn native_update_retains_reverse_until_commit_or_rollback_and_never_reopens() {
        thread_local! {
            static PREPARATION: Cell<(i32, bool)> = const { Cell::new((0, true)) };
            static FINISH: Cell<i32> = const { Cell::new(0) };
        }
        unsafe extern "C" fn prepare(
            handle: *mut c_void,
            _: *const abi::UrpNativeCreateArgs,
            output: *mut *mut c_void,
        ) -> i32 {
            assert_eq!(handle, NonNull::<u8>::dangling().as_ptr().cast());
            CALLS.with(|calls| calls.borrow_mut().push("prepare-update"));
            // SAFETY: the caller supplies a writable update destination.
            unsafe {
                *output = if PREPARATION.get().1 {
                    Box::into_raw(Box::new(0_u8)).cast()
                } else {
                    std::ptr::null_mut()
                }
            };
            PREPARATION.get().0
        }
        unsafe extern "C" fn begin(_: *mut c_void, update: *mut c_void) -> i32 {
            assert!(!update.is_null());
            CALLS.with(|calls| calls.borrow_mut().push("begin-update"));
            0
        }
        unsafe extern "C" fn accept(_: *mut c_void) -> i32 {
            CALLS.with(|calls| calls.borrow_mut().push("accept-update"));
            0
        }
        unsafe extern "C" fn finish(_: *mut c_void, commit: u32) -> i32 {
            CALLS.with(|calls| {
                calls.borrow_mut().push(if commit == 1 {
                    "commit-update"
                } else {
                    "rollback-update"
                })
            });
            FINISH.get()
        }
        unsafe extern "C" fn destroy_update(update: *mut c_void) {
            // SAFETY: prepare transferred this token exactly once.
            drop(unsafe { Box::from_raw(update.cast::<u8>()) });
            CALLS.with(|calls| calls.borrow_mut().push("destroy-update"));
        }
        for commit in [false, true] {
            let mut active = empty_active_radio();
            active.handle = Some(NonNull::<u8>::dangling().cast());
            active.api.native_prepare_update = Some(prepare);
            active.api.native_begin_update = Some(begin);
            active.api.native_accept_update = Some(accept);
            active.api.native_finish_update = Some(finish);
            active.api.native_destroy_update = Some(destroy_update);
            // SAFETY: this test's prepare never reads the scalar-only dummy request.
            let config: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
            CALLS.with(|calls| calls.borrow_mut().clear());
            assert!(active.stage_update(&config, 9, 960).unwrap());
            assert_eq!(active.stage_update(&config, 10, 960), Err(-5));
            CALLS.with(|calls| {
                assert_eq!(
                    &*calls.borrow(),
                    &[
                        "prepare-update",
                        "begin-update",
                        "accept-update",
                        "destroy-update"
                    ]
                )
            });
            FINISH.set(-6);
            assert_eq!(active.finish_update(commit), Err(-6));
            assert!(active.updating);
            FINISH.set(0);
            active.finish_update(commit).unwrap();
            active.finish_update(false).unwrap();
            CALLS.with(|calls| {
                assert_eq!(
                    calls.borrow().last(),
                    Some(&if commit {
                        "commit-update"
                    } else {
                        "rollback-update"
                    })
                )
            });
            active.handle = None;
        }
        let mut inactive = empty_active_radio();
        // SAFETY: missing ownership rejects before this dummy record is read.
        let config: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
        assert_eq!(inactive.stage_update(&config, 10, 960), Err(-7));

        unsafe extern "C" fn reject(_: *mut c_void, _: *mut c_void) -> i32 {
            -9
        }
        let mut active = empty_active_radio();
        active.handle = Some(NonNull::<u8>::dangling().cast());
        // SAFETY: the fixture never reads this scalar-only dummy request.
        let config: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
        assert!(matches!(active.stage_update(&config, 10, 960), Ok(false)));
        active.api.native_prepare_update = Some(prepare);
        active.api.native_begin_update = Some(reject);
        active.api.native_accept_update = Some(accept);
        active.api.native_finish_update = Some(finish);
        active.api.native_destroy_update = Some(destroy_update);
        CALLS.with(|calls| calls.borrow_mut().clear());
        assert!(matches!(active.stage_update(&config, 10, 960), Err(-9)));
        assert!(active.updating);
        active.finish_update(false).unwrap();
        assert!(!active.updating);
        CALLS.with(|calls| {
            assert_eq!(
                &*calls.borrow(),
                &["prepare-update", "destroy-update", "rollback-update"]
            )
        });
        for (code, token, expected) in [
            (-3, false, Ok(false)),
            (-9, false, Err(-9)),
            (0, false, Err(-1)),
        ] {
            PREPARATION.set((code, token));
            assert_eq!(active.stage_update(&config, 10, 960), expected);
            assert!(!active.updating);
        }
        active.handle = None;
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
    fn update_ack_polling_retries_only_pending_and_enforces_the_control_deadline() {
        let calls = Cell::new(0);
        assert_eq!(
            await_update(|| {
                calls.set(calls.get() + 1);
                if calls.get() < 3 { 1 } else { 0 }
            }),
            0
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(await_update(|| -9), -9);
        let started = std::time::Instant::now();
        assert_eq!(await_update(|| 1), -7);
        assert!(started.elapsed() >= std::time::Duration::from_secs(2));
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
    fn released_radio_descriptor_is_forwarded_unchanged_to_the_native_owner() {
        use crate::providers::tests::{PublishedRadioDescriptor, replace_descriptor};

        let mut providers = ProviderSet::load().unwrap();
        // SAFETY: the loaded provider owns this complete, validated descriptor
        // and retains the processing operations while lifecycle calls are replaced.
        let mut native = unsafe {
            *providers
                .descriptor("libusbradioplus_product.so.1")
                .unwrap()
                .as_ptr()
                .cast::<abi::UrpAstDescriptor>()
        };
        native.native_create = Some(create);
        native.native_start = Some(start);
        native.native_stop = Some(stop);
        native.native_destroy = Some(destroy);
        native.native_site_bind = Some(site_bind);
        native.native_site_publish = Some(site_publish);
        native.native_site_snapshot = Some(site_snapshot);
        replace_descriptor(&mut providers, "libusbradioplus_product.so.1", native);
        let descriptor = PublishedRadioDescriptor::new();
        assert_eq!(std::mem::size_of_val(&descriptor), 112);
        replace_descriptor(&mut providers, "librptadvradio.so.4", descriptor);
        // SAFETY: the provider set retains this fixture descriptor throughout activation.
        let expected = unsafe {
            providers
                .descriptor("librptadvradio.so.4")
                .unwrap()
                .as_ptr()
        };
        let providers = Box::leak(Box::new(providers));
        let radio = crate::configuration::resolve_radio_nodes(
            "[1000]\nradio_channel=fixture\n[radio 1000]\nreceive_output_gain_db=-8\n",
        )
        .unwrap()
        .remove(0);
        CALLS.with(|calls| calls.borrow_mut().clear());
        RESULTS.set((0, true, 0));
        EXPECTED_RADIO.set(Some(expected));
        let active = ActiveRadio::open(
            providers,
            &radio,
            73,
            128,
            ProductRadioCallbacks {
                receive: Some(receive),
                receive_context: NonNull::<u8>::dangling().as_ptr().cast(),
                transmit: Some(transmit),
                transmit_context: NonNull::<u8>::dangling().as_ptr().cast(),
                metadata: None,
            },
        )
        .unwrap();
        drop(active);
        EXPECTED_RADIO.set(None);
        CALLS.with(|calls| assert_eq!(&*calls.borrow(), &["create", "start", "stop", "destroy"]));
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

    #[test]
    fn native_client_forwards_request_and_releases_each_transferred_handle() {
        // SAFETY: these C records contain only nullable pointers and scalar values.
        let (mut api, mut providers, mut config): (
            abi::UrpAstDescriptor,
            abi::UrpAstProviderManifest,
            abi::UrpNativeStationConfig,
        ) = unsafe { std::mem::zeroed() };
        api.native_create = Some(create);
        api.native_start = Some(start);
        api.native_stop = Some(stop);
        api.native_destroy = Some(destroy);
        api.native_site_bind = Some(site_bind);
        api.native_site_publish = Some(site_publish);
        api.native_site_snapshot = Some(site_snapshot);
        providers.abi_version = 1;
        config.receive_output_gain_db = -8;
        for (scenario, expected_status, expected_calls) in [
            (
                (0, true, 0),
                None,
                &["create", "start", "stop", "destroy"][..],
            ),
            (
                (0, true, -8),
                Some(-8),
                &["create", "start", "stop", "destroy"][..],
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
            };
            let result = ActiveRadio::open_with_api(&api, &providers, &config, 73, 128, callbacks);
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
                        &["create", "start"],
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
        let (mut api, providers, config): (
            abi::UrpAstDescriptor,
            abi::UrpAstProviderManifest,
            abi::UrpNativeStationConfig,
        ) = unsafe { std::mem::zeroed() };
        api.native_create = Some(create);
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
        };
        let mut missing_receive = callbacks();
        missing_receive.receive = None;
        assert!(matches!(
            ActiveRadio::open_with_api(&api, &providers, &config, 73, 128, missing_receive),
            Err(RadioActivationError::InvalidCallbacks)
        ));
        let mut missing_transmit = callbacks();
        missing_transmit.transmit = None;
        assert!(matches!(
            ActiveRadio::open_with_api(&api, &providers, &config, 73, 128, missing_transmit),
            Err(RadioActivationError::InvalidCallbacks)
        ));
        assert!(matches!(
            ActiveRadio::open_with_api(&api, &providers, &config, 73, 0, callbacks()),
            Err(RadioActivationError::InvalidCallbacks)
        ));
        api.native_start = None;
        assert!(matches!(
            ActiveRadio::open_with_api(&api, &providers, &config, 73, 128, callbacks()),
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
