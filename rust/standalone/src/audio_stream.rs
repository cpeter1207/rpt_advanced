//! Direct ownership of one PortAudio/ALSA stream for a resolved CM119 radio.

use crate::{ResolvedRadioNode, abi, providers::ResolvedCm119Device};
use std::{marker::PhantomData, ptr::NonNull};

/// One stream operation failed before or during PortAudio startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamError {
    /// The adapter is missing a required operation.
    IncompleteAdapter,
    /// A frame bound or resolved device setting is invalid.
    InvalidConfiguration,
    /// The adapter rejected an operation.
    Adapter(i32),
    /// The stream is already active.
    AlreadyStarted,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteAdapter => formatter.write_str("audio adapter is incomplete"),
            Self::InvalidConfiguration => formatter.write_str("invalid audio stream settings"),
            Self::Adapter(code) => write!(formatter, "audio adapter operation failed ({code})"),
            Self::AlreadyStarted => formatter.write_str("audio stream is already started"),
        }
    }
}

impl std::error::Error for StreamError {}

/// Callback functions and exclusively borrowed contexts retained by one stream.
pub struct AudioWorkers<'a> {
    receive: abi::rptadv_audio_receive_worker,
    receive_context: *mut std::ffi::c_void,
    transmit: abi::rptadv_audio_transmit_worker,
    transmit_context: *mut std::ffi::c_void,
    _receive_owner: Option<Box<dyn AudioWorkerContext + 'a>>,
    _transmit_owner: Option<Box<dyn AudioWorkerContext + 'a>>,
    _contexts: PhantomData<(&'a mut (), &'a mut ())>,
}

trait AudioWorkerContext {}

impl<T> AudioWorkerContext for T {}

impl<'a> AudioWorkers<'a> {
    /// Bind stable, separately owned RX and TX contexts to the PortAudio callbacks.
    pub fn new<R, T>(
        receive: abi::rptadv_audio_receive_worker,
        receive_context: &'a mut R,
        transmit: abi::rptadv_audio_transmit_worker,
        transmit_context: &'a mut T,
    ) -> Self {
        Self {
            receive,
            receive_context: std::ptr::from_mut(receive_context).cast(),
            transmit,
            transmit_context: std::ptr::from_mut(transmit_context).cast(),
            _receive_owner: None,
            _transmit_owner: None,
            _contexts: PhantomData,
        }
    }

    /// Own stable callback contexts so the stream can release them after stopping callbacks.
    pub fn owned<R: 'a, T: 'a>(
        receive: abi::rptadv_audio_receive_worker,
        receive_context: R,
        transmit: abi::rptadv_audio_transmit_worker,
        transmit_context: T,
    ) -> Self {
        let mut receive_owner = Box::new(receive_context);
        let mut transmit_owner = Box::new(transmit_context);
        let receive_context = std::ptr::from_mut(&mut *receive_owner).cast();
        let transmit_context = std::ptr::from_mut(&mut *transmit_owner).cast();
        let receive_owner: Box<dyn AudioWorkerContext + 'a> = receive_owner;
        let transmit_owner: Box<dyn AudioWorkerContext + 'a> = transmit_owner;
        Self {
            receive,
            receive_context,
            transmit,
            transmit_context,
            _receive_owner: Some(receive_owner),
            _transmit_owner: Some(transmit_owner),
            _contexts: PhantomData,
        }
    }
}

/// An opened PortAudio stream that retains both callback contexts until teardown.
pub struct NativeAudioStream<'a> {
    start: unsafe extern "C" fn(*mut abi::rptadv_audio_stream) -> i32,
    stop: unsafe extern "C" fn(*mut abi::rptadv_audio_stream) -> i32,
    destroy: unsafe extern "C" fn(*mut abi::rptadv_audio_stream),
    handle: NonNull<abi::rptadv_audio_stream>,
    _workers: AudioWorkers<'a>,
    started: bool,
}

impl<'a> NativeAudioStream<'a> {
    /// Open the exact resolved CM119 endpoints without starting callbacks.
    pub fn open(
        api: &'a abi::rptadv_audio_adapter_descriptor,
        radio: &ResolvedRadioNode,
        device: &ResolvedCm119Device,
        maximum_frames: u32,
        workers: AudioWorkers<'a>,
    ) -> Result<Self, StreamError> {
        let (Some(create), Some(start), Some(stop), Some(destroy)) = (
            api.stream_create,
            api.stream_start,
            api.stream_stop,
            api.stream_destroy,
        ) else {
            return Err(StreamError::IncompleteAdapter);
        };
        if maximum_frames == 0
            || device.input_device_index < 0
            || device.output_device_index < 0
            || radio.settings.input_device_channels == 0
            || radio.settings.input_device_channels > 2
            || radio.settings.output_device_channels == 0
            || radio.settings.output_device_channels > 2
            || radio.settings.input_extra_buffer_ms > 500
            || radio.settings.output_extra_buffer_ms > 500
            || workers.receive.is_none()
            || workers.transmit.is_none()
        {
            return Err(StreamError::InvalidConfiguration);
        }
        let config = abi::rptadv_audio_stream_config {
            struct_size: std::mem::size_of::<abi::rptadv_audio_stream_config>() as u32,
            abi_version: 2,
            native_sample_rate_hz: 48_000,
            maximum_receive_frame_count: maximum_frames,
            maximum_transmit_frame_count: maximum_frames,
            input_device_index: device.input_device_index,
            output_device_index: device.output_device_index,
            input_device_channels: radio.settings.input_device_channels,
            output_device_channels: radio.settings.output_device_channels,
            receive_worker: workers.receive,
            receive_worker_context: workers.receive_context,
            transmit_worker: workers.transmit,
            transmit_worker_context: workers.transmit_context,
            extra_output_buffer_milliseconds: radio.settings.output_extra_buffer_ms,
            extra_input_buffer_milliseconds: radio.settings.input_extra_buffer_ms,
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: callback contexts stay borrowed in `workers`, and config is copied synchronously.
        let result = unsafe { create(&config, &mut handle) };
        if result != 0 {
            if !handle.is_null() {
                // SAFETY: a returned handle remains adapter-owned even when creation reports failure.
                unsafe { destroy(handle) };
            }
            return Err(StreamError::Adapter(result));
        }
        let handle = NonNull::new(handle).ok_or(StreamError::InvalidConfiguration)?;
        let stream = Self {
            start,
            stop,
            destroy,
            handle,
            _workers: workers,
            started: false,
        };
        Ok(stream)
    }

    /// Start the PortAudio callback workers after the caller has completed setup.
    pub fn start(&mut self) -> Result<(), StreamError> {
        if self.started {
            return Err(StreamError::AlreadyStarted);
        }
        // SAFETY: handle is live; owned callback contexts remain borrowed until stream destruction.
        let result = unsafe { (self.start)(self.handle.as_ptr()) };
        if result != 0 {
            return Err(StreamError::Adapter(result));
        }
        self.started = true;
        Ok(())
    }

    /// Stop callbacks synchronously while retaining the opened device reservation.
    pub fn stop(&mut self) -> Result<(), StreamError> {
        if !self.started {
            return Ok(());
        }
        // SAFETY: this control-plane call synchronously quiesces both callbacks.
        let result = unsafe { (self.stop)(self.handle.as_ptr()) };
        if result != 0 {
            return Err(StreamError::Adapter(result));
        }
        self.started = false;
        Ok(())
    }
}

impl Drop for NativeAudioStream<'_> {
    fn drop(&mut self) {
        // SAFETY: `open` requires destroy; it synchronously closes before workers drop.
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioWorkers, NativeAudioStream, StreamError};
    use crate::{ResolvedRadioNode, abi, providers::ResolvedCm119Device};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static WORKER_CONTEXTS_DROPPED: AtomicUsize = AtomicUsize::new(0);
    static DESTROY_SAW_LIVE_CONTEXTS: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn creates_starts_and_destroys_the_stream_with_resolved_settings() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let api = fake_api();
        let radio = radio();
        let device = device();
        let mut receive_context = 7_u32;
        let mut transmit_context = 9_u32;
        let workers = AudioWorkers::new(
            Some(receive),
            &mut receive_context,
            Some(transmit),
            &mut transmit_context,
        );
        let mut stream = NativeAudioStream::open(&api, &radio, &device, 1024, workers).unwrap();

        stream.start().unwrap();
        drop(stream);

        assert_eq!(CALLS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn rejects_invalid_frame_bounds_before_adapter_open() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let api = fake_api();
        let radio = radio();
        let device = device();
        let mut receive_context = ();
        let mut transmit_context = ();
        let workers = AudioWorkers::new(
            Some(receive),
            &mut receive_context,
            Some(transmit),
            &mut transmit_context,
        );

        assert!(matches!(
            NativeAudioStream::open(&api, &radio, &device, 0, workers),
            Err(StreamError::InvalidConfiguration)
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stream_errors_have_stable_user_facing_messages() {
        assert_eq!(
            StreamError::IncompleteAdapter.to_string(),
            "audio adapter is incomplete"
        );
        assert_eq!(
            StreamError::InvalidConfiguration.to_string(),
            "invalid audio stream settings"
        );
        assert_eq!(
            StreamError::Adapter(-4).to_string(),
            "audio adapter operation failed (-4)"
        );
        assert_eq!(
            StreamError::AlreadyStarted.to_string(),
            "audio stream is already started"
        );
    }

    #[test]
    fn rejects_each_missing_adapter_operation() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut api = fake_api();
        api.stream_create = None;
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::IncompleteAdapter,
        );

        api = fake_api();
        api.stream_start = None;
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::IncompleteAdapter,
        );

        api = fake_api();
        api.stream_stop = None;
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::IncompleteAdapter,
        );

        api = fake_api();
        api.stream_destroy = None;
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::IncompleteAdapter,
        );
    }

    #[test]
    fn rejects_each_invalid_stream_setting_before_adapter_open() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let api = fake_api();

        let mut invalid_radio = radio();
        invalid_radio.settings.input_device_channels = 0;
        assert_open_error(
            &api,
            &invalid_radio,
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_radio = radio();
        invalid_radio.settings.input_device_channels = 3;
        assert_open_error(
            &api,
            &invalid_radio,
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_radio = radio();
        invalid_radio.settings.output_device_channels = 0;
        assert_open_error(
            &api,
            &invalid_radio,
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_radio = radio();
        invalid_radio.settings.output_device_channels = 3;
        assert_open_error(
            &api,
            &invalid_radio,
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_radio = radio();
        invalid_radio.settings.input_extra_buffer_ms = 501;
        assert_open_error(
            &api,
            &invalid_radio,
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_radio = radio();
        invalid_radio.settings.output_extra_buffer_ms = 501;
        assert_open_error(
            &api,
            &invalid_radio,
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_device = device();
        invalid_device.input_device_index = -1;
        assert_open_error(
            &api,
            &radio(),
            &invalid_device,
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        let mut invalid_device = device();
        invalid_device.output_device_index = -1;
        assert_open_error(
            &api,
            &radio(),
            &invalid_device,
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );

        assert_open_error(
            &api,
            &radio(),
            &device(),
            0,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            None,
            Some(transmit),
            StreamError::InvalidConfiguration,
        );
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            None,
            StreamError::InvalidConfiguration,
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reports_adapter_creation_failure_and_destroys_partial_handles() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let mut api = fake_api();
        api.stream_create = Some(create_failure_without_handle);
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::Adapter(-8),
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);

        CALLS.store(0, Ordering::SeqCst);
        let mut api = fake_api();
        api.stream_create = Some(create_failure_with_handle);
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::Adapter(-9),
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rejects_successful_creation_without_a_stream_handle() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut api = fake_api();
        api.stream_create = Some(create_success_without_handle);
        assert_open_error(
            &api,
            &radio(),
            &device(),
            1024,
            Some(receive),
            Some(transmit),
            StreamError::InvalidConfiguration,
        );
    }

    #[test]
    fn returns_start_and_stop_failures_and_stop_is_idempotent() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut api = fake_api();
        api.stream_start = Some(start_failure);
        let audio_workers = workers();
        let mut stream =
            NativeAudioStream::open(&api, &radio(), &device(), 1024, audio_workers).unwrap();
        assert_eq!(stream.start(), Err(StreamError::Adapter(-10)));
        drop(stream);

        let mut api = fake_api();
        api.stream_stop = Some(stop_failure);
        let audio_workers = workers();
        let mut stream =
            NativeAudioStream::open(&api, &radio(), &device(), 1024, audio_workers).unwrap();
        assert_eq!(stream.stop(), Ok(()));
        stream.start().unwrap();
        assert_eq!(stream.stop(), Err(StreamError::Adapter(-11)));
        drop(stream);
    }

    #[test]
    fn rejects_double_start_without_calling_adapter_twice() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let api = fake_api();
        let radio = radio();
        let device = device();
        let mut receive_context = ();
        let mut transmit_context = ();
        let workers = AudioWorkers::new(
            Some(receive),
            &mut receive_context,
            Some(transmit),
            &mut transmit_context,
        );
        let mut stream = NativeAudioStream::open(&api, &radio, &device, 1024, workers).unwrap();

        stream.start().unwrap();
        assert_eq!(stream.start(), Err(StreamError::AlreadyStarted));
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stop_quiesces_callbacks_and_allows_a_later_restart() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let api = fake_api();
        let radio = radio();
        let device = device();
        let mut receive_context = ();
        let mut transmit_context = ();
        let workers = AudioWorkers::new(
            Some(receive),
            &mut receive_context,
            Some(transmit),
            &mut transmit_context,
        );
        let mut stream = NativeAudioStream::open(&api, &radio, &device, 1024, workers).unwrap();

        stream.start().unwrap();
        stream.stop().unwrap();
        stream.start().unwrap();
        drop(stream);

        assert_eq!(CALLS.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn stream_owns_callback_contexts_until_after_adapter_destruction() {
        struct DropProbe;
        impl Drop for DropProbe {
            fn drop(&mut self) {
                WORKER_CONTEXTS_DROPPED.fetch_add(1, Ordering::SeqCst);
            }
        }

        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        WORKER_CONTEXTS_DROPPED.store(0, Ordering::SeqCst);
        DESTROY_SAW_LIVE_CONTEXTS.store(0, Ordering::SeqCst);
        let api = fake_api();
        let workers = AudioWorkers::owned(Some(receive), DropProbe, Some(transmit), DropProbe);
        let mut stream = NativeAudioStream::open(&api, &radio(), &device(), 1024, workers).unwrap();

        stream.start().unwrap();
        assert_eq!(WORKER_CONTEXTS_DROPPED.load(Ordering::SeqCst), 0);
        drop(stream);

        assert_eq!(WORKER_CONTEXTS_DROPPED.load(Ordering::SeqCst), 2);
        assert_eq!(DESTROY_SAW_LIVE_CONTEXTS.load(Ordering::SeqCst), 1);
        assert_eq!(CALLS.load(Ordering::SeqCst), 3);
    }

    fn workers() -> AudioWorkers<'static> {
        AudioWorkers::owned(Some(receive), (), Some(transmit), ())
    }

    fn assert_open_error(
        api: &abi::rptadv_audio_adapter_descriptor,
        radio: &ResolvedRadioNode,
        device: &ResolvedCm119Device,
        maximum_frames: u32,
        receive_worker: abi::rptadv_audio_receive_worker,
        transmit_worker: abi::rptadv_audio_transmit_worker,
        expected: StreamError,
    ) {
        let mut receive_context = ();
        let mut transmit_context = ();
        let workers = AudioWorkers::new(
            receive_worker,
            &mut receive_context,
            transmit_worker,
            &mut transmit_context,
        );
        assert_eq!(
            NativeAudioStream::open(api, radio, device, maximum_frames, workers).err(),
            Some(expected)
        );
    }

    unsafe extern "C" fn create_failure_without_handle(
        _: *const abi::rptadv_audio_stream_config,
        _: *mut *mut abi::rptadv_audio_stream,
    ) -> i32 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        -8
    }

    unsafe extern "C" fn create_failure_with_handle(
        _: *const abi::rptadv_audio_stream_config,
        stream: *mut *mut abi::rptadv_audio_stream,
    ) -> i32 {
        unsafe { *stream = std::ptr::dangling_mut() };
        CALLS.fetch_add(1, Ordering::SeqCst);
        -9
    }

    unsafe extern "C" fn create_success_without_handle(
        _: *const abi::rptadv_audio_stream_config,
        _: *mut *mut abi::rptadv_audio_stream,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn start_failure(_: *mut abi::rptadv_audio_stream) -> i32 {
        -10
    }

    unsafe extern "C" fn stop_failure(_: *mut abi::rptadv_audio_stream) -> i32 {
        -11
    }

    unsafe extern "C" fn receive(_: *mut std::ffi::c_void, _: *const f32, _: u32) -> i32 {
        0
    }
    unsafe extern "C" fn transmit(_: *mut std::ffi::c_void, _: *mut f32, _: u32) -> i32 {
        0
    }

    fn fake_api() -> abi::rptadv_audio_adapter_descriptor {
        unsafe extern "C" fn create(
            config: *const abi::rptadv_audio_stream_config,
            stream: *mut *mut abi::rptadv_audio_stream,
        ) -> i32 {
            let config = unsafe { &*config };
            assert_eq!(config.native_sample_rate_hz, 48_000);
            assert_eq!(config.input_device_index, 6);
            assert_eq!(config.output_device_index, 7);
            assert_eq!(config.maximum_receive_frame_count, 1024);
            assert_eq!(config.input_device_channels, 1);
            assert_eq!(config.output_device_channels, 1);
            assert_eq!(config.extra_input_buffer_milliseconds, 40);
            assert_eq!(config.extra_output_buffer_milliseconds, 100);
            assert!(config.receive_worker.is_some());
            assert!(config.transmit_worker.is_some());
            CALLS.fetch_add(1, Ordering::SeqCst);
            unsafe { *stream = std::ptr::dangling_mut() };
            0
        }
        unsafe extern "C" fn start(_: *mut abi::rptadv_audio_stream) -> i32 {
            CALLS.fetch_add(1, Ordering::SeqCst);
            0
        }
        unsafe extern "C" fn stop(_: *mut abi::rptadv_audio_stream) -> i32 {
            CALLS.fetch_add(1, Ordering::SeqCst);
            0
        }
        unsafe extern "C" fn destroy(_: *mut abi::rptadv_audio_stream) {
            DESTROY_SAW_LIVE_CONTEXTS.store(
                usize::from(WORKER_CONTEXTS_DROPPED.load(Ordering::SeqCst) == 0),
                Ordering::SeqCst,
            );
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        abi::rptadv_audio_adapter_descriptor {
            stream_create: Some(create),
            stream_start: Some(start),
            stream_stop: Some(stop),
            stream_destroy: Some(destroy),
            ..unsafe { std::mem::zeroed() }
        }
    }

    fn radio() -> ResolvedRadioNode {
        crate::resolve_radio_nodes(
            &rpt_advanced_core::config::ConfigDocument::parse(
                "[radio]\ninput_extra_buffer_ms=40\noutput_extra_buffer_ms=100\n[1000]\n",
            )
            .unwrap(),
        )
        .unwrap()
        .value
        .remove(0)
    }

    fn device() -> ResolvedCm119Device {
        ResolvedCm119Device {
            usb_interface_path: "3-1:1.0".into(),
            usb_serial: Some("SERIAL-A".into()),
            alsa_card_index: 4,
            input_device_index: 6,
            output_device_index: 7,
            gpio_vendor_id: 0x0d8c,
            gpio_product_id: 0x013c,
        }
    }
}
