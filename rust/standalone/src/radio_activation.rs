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
}

impl std::fmt::Display for RadioActivationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::InvalidCallbacks => {
                formatter.write_str("invalid native radio callbacks or frame bound")
            }
            Self::Product(status) => write!(formatter, "shared radio product failed ({status})"),
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
}

/// Native station handle; its shared product owns every hardware and DSP resource.
pub struct ActiveRadio {
    handle: Option<NonNull<c_void>>,
    stop: unsafe extern "C" fn(*mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
    site_bind: unsafe extern "C" fn(*mut c_void, u32, *const abi::UrpAstSiteBinding) -> i32,
    site_publish: unsafe extern "C" fn(*mut c_void, u32, *const abi::UrpAstSiteRequest) -> i32,
    site_snapshot: unsafe extern "C" fn(*mut c_void, u32, *mut abi::UrpAstSiteSnapshot) -> i32,
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
            radio: std::ptr::from_ref(descriptors.radio).cast(),
            audio: std::ptr::from_ref(descriptors.audio).cast(),
            gpio: std::ptr::from_ref(descriptors.gpio).cast(),
            rnnoise: std::ptr::null(),
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
            abi_version: 3,
            receive: callbacks.receive,
            receive_context: callbacks.receive_context,
            transmit: callbacks.transmit,
            transmit_context: callbacks.transmit_context,
            accepted_abi_version: 0,
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
            stop,
            destroy,
            site_bind,
            site_publish,
            site_snapshot,
        };
        // SAFETY: successful create returns this stopped, uniquely owned station.
        let status = unsafe { start(handle.as_ptr()) };
        if status != 0 {
            return Err(RadioActivationError::Product(status));
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    thread_local! {
        static CALLS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static RESULTS: Cell<(i32, bool, i32)> = const { Cell::new((0, true, 0)) };
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
        assert_eq!(callbacks.abi_version, 3);
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
            stop,
            destroy,
            site_bind,
            site_publish,
            site_snapshot,
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
            };
            let result = ActiveRadio::open_with_api(&api, &providers, &config, 73, 128, callbacks);
            let status = result.as_ref().err().map(|error| match error {
                RadioActivationError::Product(status) => *status,
                error => panic!("unexpected client error: {error}"),
            });
            assert_eq!(status, expected_status);
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
