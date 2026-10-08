//! One owned CM119/radio-core/PortAudio activation.

use crate::{
    ResolvedRadioNode,
    audio_callbacks::{NativeReceiveWorker, NativeTransmitWorker, StandaloneRadioState},
    audio_stream::{AudioWorkers, NativeAudioStream},
    gpio_device::{Cm119GpioDevice, GpioService},
    program_audio::ProgramAudio,
    providers::{ProviderSet, RadioRuntimeDescriptors},
    radio_generation::PreparedRadioGeneration,
};
use std::sync::Arc;

/// One radio could not be activated.
#[derive(Debug)]
pub enum RadioActivationError {
    /// Required versioned runtime providers are not available.
    Provider(crate::providers::ProviderError),
    /// The audio and GPIO providers could not agree on a CM119 interface.
    Device(crate::providers::Cm119DeviceError),
    /// The selected GPIO device could not be opened or serviced.
    Gpio(crate::gpio_device::GpioError),
    /// Program-audio bridge setup failed.
    Program(crate::program_audio::ProgramAudioError),
    /// Radio core and processing graphs could not be prepared.
    Generation(crate::radio_generation::RadioGenerationError),
    /// Callback worker setup failed.
    Callback(crate::audio_callbacks::AudioCallbackError),
    /// PortAudio stream setup or start failed.
    Stream(crate::audio_stream::StreamError),
}

impl std::fmt::Display for RadioActivationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::Device(error) => error.fmt(formatter),
            Self::Gpio(error) => error.fmt(formatter),
            Self::Program(error) => error.fmt(formatter),
            Self::Generation(error) => error.fmt(formatter),
            Self::Callback(error) => error.fmt(formatter),
            Self::Stream(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RadioActivationError {}

/// Active radio whose callback contexts and providers are retained through stream teardown.
pub struct ActiveRadio {
    gpio: Option<GpioService>,
    stream: Option<NativeAudioStream<'static>>,
    generation: Option<Box<PreparedRadioGeneration<'static>>>,
}

/// Product callbacks and contexts retained for one active radio.
pub struct ProductRadioCallbacks {
    /// Product receive callback.
    pub receive: crate::abi::rptadv_radio_receive_v2,
    /// Product receive callback context.
    pub receive_context: *mut std::ffi::c_void,
    /// Product transmit callback.
    pub transmit: crate::abi::rptadv_radio_transmit_v3,
    /// Product transmit callback context.
    pub transmit_context: *mut std::ffi::c_void,
}

impl ActiveRadio {
    /// Prepare DSP, open GPIO/audio, then start the native callbacks.
    ///
    /// The provider set must remain alive until this value is dropped. The returned callback
    /// workers borrow the boxed generation; `Drop` quiesces their stream before that box moves
    /// or is released.
    pub fn open(
        providers: &'static ProviderSet,
        radio: &ResolvedRadioNode,
        generation_id: u64,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<Self, RadioActivationError> {
        validate_callbacks(&callbacks, maximum_frames)?;
        let descriptors = providers
            .radio_runtime_descriptors()
            .map_err(RadioActivationError::Provider)?;
        Self::open_with_descriptors(
            providers,
            descriptors,
            radio,
            generation_id,
            maximum_frames,
            callbacks,
        )
    }

    fn open_with_descriptors(
        providers: &'static ProviderSet,
        descriptors: RadioRuntimeDescriptors,
        radio: &ResolvedRadioNode,
        generation_id: u64,
        maximum_frames: u32,
        callbacks: ProductRadioCallbacks,
    ) -> Result<Self, RadioActivationError> {
        let device = providers
            .resolve_cm119_device(radio)
            .map_err(RadioActivationError::Device)?;
        let gpio_device = Cm119GpioDevice::open(descriptors.gpio, radio, &device)
            .map_err(RadioActivationError::Gpio)?;
        let state = Arc::new(StandaloneRadioState::default());
        let gpio = GpioService::start(gpio_device, Arc::clone(&state))
            .map_err(RadioActivationError::Gpio)?;

        let program = ProgramAudio::new(
            callbacks.transmit,
            callbacks.transmit_context,
            maximum_frames,
        )
        .map_err(RadioActivationError::Program)?;
        let mut generation = Box::new(
            PreparedRadioGeneration::prepare_with_program(
                descriptors.radio,
                descriptors.ffmpeg,
                radio,
                generation_id,
                maximum_frames,
                50,
                program,
            )
            .map_err(RadioActivationError::Generation)?,
        );
        let (receive_endpoint, transmit_endpoint, program) = generation
            .split()
            .map_err(crate::radio_generation::RadioGenerationError::Session)
            .map_err(RadioActivationError::Generation)?;
        let program = program.ok_or(RadioActivationError::Callback(
            crate::audio_callbacks::AudioCallbackError::InvalidFrame,
        ))?;
        let receive_worker = NativeReceiveWorker::new(
            receive_endpoint,
            callbacks.receive,
            callbacks.receive_context,
            maximum_frames,
            0,
            radio.settings.input_device_channels,
            Arc::clone(&state),
        )
        .map_err(RadioActivationError::Callback)?;
        let transmit_worker = NativeTransmitWorker::new(
            transmit_endpoint,
            program,
            maximum_frames,
            radio.settings.output_device_channels,
            state,
        )
        .map_err(RadioActivationError::Callback)?;

        // SAFETY: the generation is heap-stable and owned by `ActiveRadio`. Its PortAudio stream
        // owns these callback contexts and is destroyed first in `Drop`, synchronously quiescing
        // both callbacks before the generation is dropped. ProviderSet is required to be static.
        let receive_worker: NativeReceiveWorker<'static, 'static> =
            unsafe { std::mem::transmute(receive_worker) };
        // SAFETY: same retained-generation and stream-quiescence guarantee as the RX worker.
        let transmit_worker: NativeTransmitWorker<'static, 'static> =
            unsafe { std::mem::transmute(transmit_worker) };
        let workers = AudioWorkers::owned(
            NativeReceiveWorker::callback(),
            receive_worker,
            NativeTransmitWorker::callback(),
            transmit_worker,
        );
        let mut stream =
            NativeAudioStream::open(descriptors.audio, radio, &device, maximum_frames, workers)
                .map_err(RadioActivationError::Stream)?;
        stream.start().map_err(RadioActivationError::Stream)?;

        Ok(Self {
            gpio: Some(gpio),
            stream: Some(stream),
            generation: Some(generation),
        })
    }
}

fn validate_callbacks(
    callbacks: &ProductRadioCallbacks,
    maximum_frames: u32,
) -> Result<(), RadioActivationError> {
    if callbacks.receive.is_none() || callbacks.transmit.is_none() || maximum_frames == 0 {
        return Err(RadioActivationError::Callback(
            crate::audio_callbacks::AudioCallbackError::InvalidFrame,
        ));
    }
    Ok(())
}

impl Drop for ActiveRadio {
    fn drop(&mut self) {
        // Stop PTT/signal polling first, then synchronously destroy callbacks, then release DSP.
        self.gpio.take();
        self.stream.take();
        self.generation.take();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{ProductRadioCallbacks, validate_callbacks};
    use crate::{
        audio_callbacks::AudioCallbackError,
        audio_stream::StreamError,
        gpio_device::GpioError,
        processing::ProcessingError,
        program_audio::ProgramAudioError,
        providers::{Cm119DeviceError, ProviderError},
        radio_generation::RadioGenerationError,
        radio_session::RadioSessionError,
    };
    use std::{
        ffi::{CStr, c_void},
        sync::{
            OnceLock,
            atomic::{AtomicI32, AtomicUsize, Ordering},
        },
    };

    static PROVIDERS: OnceLock<usize> = OnceLock::new();
    static STREAM_CREATE_RESULT: AtomicI32 = AtomicI32::new(0);
    static STREAM_START_RESULT: AtomicI32 = AtomicI32::new(0);
    static GPIO_OPEN_RESULT: AtomicI32 = AtomicI32::new(0);
    static FFMPEG_CREATE_RESULT: AtomicI32 = AtomicI32::new(0);
    static STREAM_STOPS: AtomicUsize = AtomicUsize::new(0);
    static STREAM_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static GPIO_CLOSES: AtomicUsize = AtomicUsize::new(0);
    static SESSION_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static GRAPH_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn empty_active_radio() -> super::ActiveRadio {
        super::ActiveRadio {
            gpio: None,
            stream: None,
            generation: None,
        }
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

    fn callbacks(
        receive: crate::abi::rptadv_radio_receive_v2,
        transmit: crate::abi::rptadv_radio_transmit_v3,
    ) -> ProductRadioCallbacks {
        ProductRadioCallbacks {
            receive,
            receive_context: std::ptr::null_mut(),
            transmit,
            transmit_context: std::ptr::null_mut(),
        }
    }

    fn providers() -> &'static crate::providers::ProviderSet {
        let pointer = *PROVIDERS.get_or_init(|| {
            let radio = Box::leak(Box::new(radio_api()));
            let audio = Box::leak(Box::new(audio_api()));
            let gpio = Box::leak(Box::new(gpio_api()));
            let ffmpeg = Box::leak(Box::new(ffmpeg_api()));
            Box::into_raw(Box::new(
                crate::providers::ProviderSet::for_radio_activation_tests(
                    radio, audio, gpio, ffmpeg,
                ),
            )) as usize
        });
        // SAFETY: the test-only owner and descriptors are intentionally leaked so their static
        // borrows match ActiveRadio's provider lifetime for every serialized fixture test.
        unsafe { &*(pointer as *const crate::providers::ProviderSet) }
    }

    fn radio_node() -> crate::ResolvedRadioNode {
        let config = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\ndevice_identifier=3-1\nusb_serial=SERIAL-A\nreceive_graph=anull\ntransmit_graph=anull\n[1000]\n",
        )
        .unwrap();
        crate::resolve_radio_nodes(&config).unwrap().value.remove(0)
    }

    fn audio_api() -> crate::abi::rptadv_audio_adapter_descriptor {
        crate::abi::rptadv_audio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_adapter_descriptor>() as u32,
            abi_version: 2,
            capability_name: c"rptadv.portaudio-alsa-audio".as_ptr(),
            usb_device_select: Some(select_device),
            stream_create: Some(stream_create),
            stream_start: Some(stream_start),
            stream_stop: Some(stream_stop),
            stream_destroy: Some(stream_destroy),
            ..unsafe { std::mem::zeroed() }
        }
    }

    unsafe extern "C" fn select_device(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> i32 {
        let selector = unsafe { &*selector };
        if selector.device_identifier.is_null()
            || unsafe { CStr::from_ptr(selector.device_identifier) }.to_bytes() != b"3-1"
            || matched.is_null()
        {
            return -1;
        }
        let matched = unsafe { &mut *matched };
        matched.struct_size = std::mem::size_of_val(matched) as u32;
        matched.abi_version = 2;
        for (slot, value) in matched.usb_interface_path.iter_mut().zip(b"3-1:1.0\0") {
            *slot = *value as std::ffi::c_char;
        }
        for (slot, value) in matched.usb_serial.iter_mut().zip(b"SERIAL-A\0") {
            *slot = *value as std::ffi::c_char;
        }
        matched.selection = crate::abi::rptadv_audio_usb_device_selection {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_usb_device_selection>()
                as u32,
            abi_version: 2,
            alsa_card_index: 0,
            input_device_index: 6,
            output_device_index: 7,
        };
        0
    }

    unsafe extern "C" fn stream_create(
        _: *const crate::abi::rptadv_audio_stream_config,
        stream: *mut *mut crate::abi::rptadv_audio_stream,
    ) -> i32 {
        let result = STREAM_CREATE_RESULT.load(Ordering::SeqCst);
        if !stream.is_null() {
            unsafe { *stream = 1_usize as *mut crate::abi::rptadv_audio_stream };
        }
        result
    }

    unsafe extern "C" fn stream_start(_: *mut crate::abi::rptadv_audio_stream) -> i32 {
        STREAM_START_RESULT.load(Ordering::SeqCst)
    }

    unsafe extern "C" fn stream_stop(_: *mut crate::abi::rptadv_audio_stream) -> i32 {
        STREAM_STOPS.fetch_add(1, Ordering::SeqCst);
        0
    }

    unsafe extern "C" fn stream_destroy(_: *mut crate::abi::rptadv_audio_stream) {
        STREAM_DESTROYS.fetch_add(1, Ordering::SeqCst);
    }

    fn gpio_api() -> crate::abi::rptadv_gpio_adapter_descriptor {
        crate::abi::rptadv_gpio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.cm119-hid-gpio".as_ptr(),
            device_probe: Some(gpio_probe),
            device_open: Some(gpio_open),
            device_publish_outputs: Some(gpio_publish),
            device_service: Some(gpio_service),
            device_get_inputs: Some(gpio_inputs),
            device_close: Some(gpio_close),
            ..unsafe { std::mem::zeroed() }
        }
    }

    unsafe extern "C" fn gpio_probe(
        _: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> i32 {
        let info = unsafe { &mut *info };
        info.struct_size = std::mem::size_of_val(info) as u32;
        info.abi_version = 1;
        info.present = 1;
        info.vendor_id = 0x0d8c;
        info.product_id = 0x013c;
        for (slot, value) in info.serial.iter_mut().zip(b"SERIAL-A\0") {
            *slot = *value as std::ffi::c_char;
        }
        0
    }

    unsafe extern "C" fn gpio_open(
        _: *const crate::abi::rptadv_gpio_device_config,
        device: *mut *mut crate::abi::rptadv_gpio_device,
    ) -> i32 {
        let result = GPIO_OPEN_RESULT.load(Ordering::SeqCst);
        if !device.is_null() {
            unsafe { *device = 1_usize as *mut crate::abi::rptadv_gpio_device };
        }
        result
    }

    unsafe extern "C" fn gpio_publish(
        _: *mut crate::abi::rptadv_gpio_device,
        _: *const crate::abi::rptadv_gpio_output_action,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn gpio_service(_: *mut crate::abi::rptadv_gpio_device) -> i32 {
        0
    }

    unsafe extern "C" fn gpio_inputs(
        _: *const crate::abi::rptadv_gpio_device,
        snapshot: *mut crate::abi::rptadv_gpio_input_snapshot,
    ) -> i32 {
        let snapshot = unsafe { &mut *snapshot };
        snapshot.struct_size = std::mem::size_of_val(snapshot) as u32;
        snapshot.abi_version = 1;
        snapshot.online = 1;
        0
    }

    unsafe extern "C" fn gpio_close(_: *mut crate::abi::rptadv_gpio_device) {
        GPIO_CLOSES.fetch_add(1, Ordering::SeqCst);
    }

    fn ffmpeg_api() -> crate::abi::rptadv_ffmpeg_adapter_descriptor {
        crate::abi::rptadv_ffmpeg_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_ffmpeg_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.ffmpeg".as_ptr(),
            create: Some(graph_create),
            destroy: Some(graph_destroy),
            process_block: Some(graph_process),
            ..unsafe { std::mem::zeroed() }
        }
    }

    unsafe extern "C" fn graph_create(
        _: *const crate::abi::rptadv_ffmpeg_graph_config,
        graph: *mut *mut crate::abi::rptadv_ffmpeg_graph,
    ) -> i32 {
        let result = FFMPEG_CREATE_RESULT.load(Ordering::SeqCst);
        if result == 0 && !graph.is_null() {
            unsafe { *graph = 1_usize as *mut crate::abi::rptadv_ffmpeg_graph };
        }
        result
    }

    unsafe extern "C" fn graph_destroy(_: *mut crate::abi::rptadv_ffmpeg_graph) {
        GRAPH_DESTROYS.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn graph_process(
        _: *mut crate::abi::rptadv_ffmpeg_graph,
        input: *const f32,
        frames: u32,
        output: *mut f32,
    ) -> i32 {
        unsafe {
            std::ptr::copy_nonoverlapping(input, output, frames as usize);
        }
        0
    }

    fn radio_api() -> crate::abi::rptadv_radio_descriptor {
        crate::abi::rptadv_radio_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_radio_descriptor>() as u32,
            abi_version: 4,
            capability_name: c"rptadv.radio-core".as_ptr(),
            session_create: Some(session_create),
            session_warm: Some(session_warm),
            session_receive: Some(session_receive),
            session_transmit: Some(session_transmit),
            session_destroy: Some(session_destroy),
            ..unsafe { std::mem::zeroed() }
        }
    }

    unsafe extern "C" fn session_create(
        _: *const crate::abi::rptadv_radio_session_config,
        _: *const crate::abi::rptadv_radio_session_ports,
        session: *mut *mut crate::abi::rptadv_radio_session,
    ) -> i32 {
        unsafe { *session = 1_usize as *mut crate::abi::rptadv_radio_session };
        0
    }

    unsafe extern "C" fn session_warm(_: *mut crate::abi::rptadv_radio_session) -> i32 {
        0
    }

    unsafe extern "C" fn session_receive(
        _: *mut crate::abi::rptadv_radio_session,
        _: *const f32,
        output: *mut f32,
        frames: u32,
        _: *const crate::abi::rptadv_radio_receive_input,
        result: *mut crate::abi::rptadv_radio_receive_result,
    ) -> i32 {
        unsafe {
            std::slice::from_raw_parts_mut(output, frames as usize * 2).fill(0.0);
            (*result).frame_count = frames;
        }
        0
    }

    unsafe extern "C" fn session_transmit(
        _: *mut crate::abi::rptadv_radio_session,
        output: *mut f32,
        frames: u32,
        _: *const crate::abi::rptadv_radio_transmit_input,
        result: *mut crate::abi::rptadv_radio_transmit_result,
    ) -> i32 {
        unsafe {
            std::slice::from_raw_parts_mut(output, frames as usize * 2).fill(0.0);
            (*result).frame_count = frames;
        }
        0
    }

    unsafe extern "C" fn session_destroy(_: *mut crate::abi::rptadv_radio_session) {
        SESSION_DESTROYS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn activates_and_tears_down_the_complete_radio_stack_without_hardware() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        reset_results();
        let active = super::ActiveRadio::open(
            providers(),
            &radio_node(),
            1,
            1024,
            callbacks(Some(receive), Some(transmit)),
        )
        .unwrap();
        drop(active);

        assert_eq!(GPIO_CLOSES.load(Ordering::SeqCst), 1);
        assert_eq!(STREAM_STOPS.load(Ordering::SeqCst), 0);
        assert_eq!(STREAM_DESTROYS.load(Ordering::SeqCst), 1);
        assert_eq!(SESSION_DESTROYS.load(Ordering::SeqCst), 1);
        assert_eq!(GRAPH_DESTROYS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn activation_failures_release_every_resource_already_acquired() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        reset_results();
        GPIO_OPEN_RESULT.store(-7, Ordering::SeqCst);
        assert!(matches!(
            super::ActiveRadio::open(
                providers(),
                &radio_node(),
                1,
                1024,
                callbacks(Some(receive), Some(transmit)),
            ),
            Err(super::RadioActivationError::Gpio(GpioError::Adapter(-7)))
        ));
        assert_eq!(GPIO_CLOSES.load(Ordering::SeqCst), 1);

        reset_results();
        FFMPEG_CREATE_RESULT.store(-9, Ordering::SeqCst);
        assert!(matches!(
            super::ActiveRadio::open(
                providers(),
                &radio_node(),
                1,
                1024,
                callbacks(Some(receive), Some(transmit)),
            ),
            Err(super::RadioActivationError::Generation(
                crate::radio_generation::RadioGenerationError::Processing(
                    ProcessingError::Adapter(-9)
                )
            ))
        ));
        assert_eq!(GPIO_CLOSES.load(Ordering::SeqCst), 1);
        assert_eq!(GRAPH_DESTROYS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stream_open_and_start_failures_release_gpio_and_generation() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        reset_results();
        STREAM_CREATE_RESULT.store(-4, Ordering::SeqCst);
        assert!(matches!(
            super::ActiveRadio::open(
                providers(),
                &radio_node(),
                1,
                1024,
                callbacks(Some(receive), Some(transmit)),
            ),
            Err(super::RadioActivationError::Stream(StreamError::Adapter(
                -4
            )))
        ));
        assert_eq!(STREAM_DESTROYS.load(Ordering::SeqCst), 1);
        assert_eq!(GPIO_CLOSES.load(Ordering::SeqCst), 1);
        assert_eq!(SESSION_DESTROYS.load(Ordering::SeqCst), 1);

        reset_results();
        STREAM_START_RESULT.store(-5, Ordering::SeqCst);
        assert!(matches!(
            super::ActiveRadio::open(
                providers(),
                &radio_node(),
                1,
                1024,
                callbacks(Some(receive), Some(transmit)),
            ),
            Err(super::RadioActivationError::Stream(StreamError::Adapter(
                -5
            )))
        ));
        assert_eq!(STREAM_DESTROYS.load(Ordering::SeqCst), 1);
        assert_eq!(GPIO_CLOSES.load(Ordering::SeqCst), 1);
        assert_eq!(SESSION_DESTROYS.load(Ordering::SeqCst), 1);
    }

    fn reset_results() {
        STREAM_CREATE_RESULT.store(0, Ordering::SeqCst);
        STREAM_START_RESULT.store(0, Ordering::SeqCst);
        GPIO_OPEN_RESULT.store(0, Ordering::SeqCst);
        FFMPEG_CREATE_RESULT.store(0, Ordering::SeqCst);
        STREAM_STOPS.store(0, Ordering::SeqCst);
        STREAM_DESTROYS.store(0, Ordering::SeqCst);
        GPIO_CLOSES.store(0, Ordering::SeqCst);
        SESSION_DESTROYS.store(0, Ordering::SeqCst);
        GRAPH_DESTROYS.store(0, Ordering::SeqCst);
    }

    #[test]
    fn activation_requires_both_callbacks_and_nonzero_frames() {
        let missing_receive =
            validate_callbacks(&callbacks(None, Some(transmit)), 960).unwrap_err();
        assert!(matches!(
            missing_receive,
            super::RadioActivationError::Callback(AudioCallbackError::InvalidFrame)
        ));

        let missing_transmit =
            validate_callbacks(&callbacks(Some(receive), None), 960).unwrap_err();
        assert!(matches!(
            missing_transmit,
            super::RadioActivationError::Callback(AudioCallbackError::InvalidFrame)
        ));

        let missing_frames =
            validate_callbacks(&callbacks(Some(receive), Some(transmit)), 0).unwrap_err();
        assert!(matches!(
            missing_frames,
            super::RadioActivationError::Callback(AudioCallbackError::InvalidFrame)
        ));
    }

    #[test]
    fn activation_accepts_valid_callback_pair_and_frame_bound() {
        assert!(validate_callbacks(&callbacks(Some(receive), Some(transmit)), 960).is_ok());
    }

    #[test]
    fn activation_errors_preserve_each_underlying_failure_message() {
        let errors = [
            (
                super::RadioActivationError::Provider(ProviderError::Load("provider")),
                "cannot load required provider provider",
            ),
            (
                super::RadioActivationError::Device(Cm119DeviceError::IncompleteProvider),
                "CM119 audio or GPIO provider is incomplete",
            ),
            (
                super::RadioActivationError::Gpio(GpioError::ThreadStart),
                "cannot start CM119 GPIO service thread",
            ),
            (
                super::RadioActivationError::Program(ProgramAudioError::ProductCallback),
                "product transmit callback failed",
            ),
            (
                super::RadioActivationError::Generation(RadioGenerationError::Session(
                    RadioSessionError::IncompleteDescriptor,
                )),
                "radio session ABI is incomplete",
            ),
            (
                super::RadioActivationError::Generation(RadioGenerationError::Processing(
                    ProcessingError::IncompleteAdapter,
                )),
                "FFmpeg exact-block adapter is incomplete",
            ),
            (
                super::RadioActivationError::Callback(AudioCallbackError::ProductCallback),
                "product audio callback failed",
            ),
            (
                super::RadioActivationError::Stream(StreamError::AlreadyStarted),
                "audio stream is already started",
            ),
        ];

        for (error, expected) in errors {
            assert_eq!(error.to_string(), expected);
        }
    }
}
