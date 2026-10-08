//! Prepared FFmpeg processor graphs for the standalone radio session.

use crate::abi;
use std::{
    ffi::{CString, c_void},
    marker::PhantomPinned,
    pin::Pin,
    ptr::NonNull,
};

/// An FFmpeg graph could not be prepared for real-time radio processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessingError {
    /// The adapter does not implement exact-block processing.
    IncompleteAdapter,
    /// The graph, rate, or callback frame bound is invalid.
    InvalidConfiguration,
    /// FFmpeg rejected graph creation or warmup.
    Adapter(i32),
    /// The adapter returned success without a graph.
    MissingGraph,
}

impl std::fmt::Display for ProcessingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteAdapter => {
                formatter.write_str("FFmpeg exact-block adapter is incomplete")
            }
            Self::InvalidConfiguration => formatter.write_str("invalid FFmpeg graph settings"),
            Self::Adapter(code) => write!(formatter, "FFmpeg graph operation failed ({code})"),
            Self::MissingGraph => formatter.write_str("FFmpeg returned no graph"),
        }
    }
}

impl std::error::Error for ProcessingError {}

/// Pinned persistent mono graph that can be borrowed by a radio-core processor port.
pub struct FfmpegProcessor<'a> {
    _api: &'a abi::rptadv_ffmpeg_adapter_descriptor,
    process: unsafe extern "C" fn(*mut abi::rptadv_ffmpeg_graph, *const f32, u32, *mut f32) -> i32,
    destroy: unsafe extern "C" fn(*mut abi::rptadv_ffmpeg_graph),
    graph: NonNull<abi::rptadv_ffmpeg_graph>,
    maximum_frames: u32,
    warm_input: Vec<f32>,
    warm_output: Vec<f32>,
    _pinned: PhantomPinned,
}

impl<'a> FfmpegProcessor<'a> {
    /// Create a 48 kHz graph and preallocate its warmup buffers on the control plane.
    pub fn create(
        api: &'a abi::rptadv_ffmpeg_adapter_descriptor,
        description: &str,
        maximum_frames: u32,
    ) -> Result<Pin<Box<Self>>, ProcessingError> {
        let (Some(create), Some(destroy), Some(process)) =
            (api.create, api.destroy, api.process_block)
        else {
            return Err(ProcessingError::IncompleteAdapter);
        };
        if api.abi_version != 1 || maximum_frames == 0 {
            return Err(ProcessingError::InvalidConfiguration);
        }
        let description =
            CString::new(description).map_err(|_| ProcessingError::InvalidConfiguration)?;
        let config = abi::rptadv_ffmpeg_graph_config {
            struct_size: std::mem::size_of::<abi::rptadv_ffmpeg_graph_config>() as u32,
            abi_version: 1,
            sample_rate_hz: 48_000,
            maximum_frame_count: maximum_frames,
            filter_description: description.as_ptr(),
        };
        let mut graph = std::ptr::null_mut();
        // SAFETY: the adapter copies graph settings during create; the descriptor outlives this
        // object and remains loaded until the graph is destroyed.
        let result = unsafe { create(&config, &mut graph) };
        if result != 0 {
            if !graph.is_null() {
                // SAFETY: failed creation still transfers any returned non-null handle to caller.
                unsafe { destroy(graph) };
            }
            return Err(ProcessingError::Adapter(result));
        }
        let graph = NonNull::new(graph).ok_or(ProcessingError::MissingGraph)?;
        Ok(Box::pin(Self {
            _api: api,
            process,
            destroy,
            graph,
            maximum_frames,
            warm_input: vec![0.0; maximum_frames as usize],
            warm_output: vec![0.0; maximum_frames as usize],
            _pinned: PhantomPinned,
        }))
    }

    /// Borrow this stable graph as the radio core's exact-frame processor port.
    ///
    /// The returned raw context is valid only while this pinned processor remains alive and
    /// callbacks are quiesced before it is dropped.
    pub fn port(self: Pin<&mut Self>) -> abi::rptadv_radio_processor_port {
        let context = unsafe { self.get_unchecked_mut() };
        abi::rptadv_radio_processor_port {
            context: std::ptr::from_mut(context).cast::<c_void>(),
            process_f32: Some(process_f32),
            bypass: Some(bypass),
            warm: Some(warm),
        }
    }

    fn run(&mut self, frames: u32) -> i32 {
        if frames == 0 || frames > self.maximum_frames {
            return abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
        }
        // SAFETY: both scratch buffers were allocated to the validated maximum at creation.
        unsafe {
            (self.process)(
                self.graph.as_ptr(),
                self.warm_input.as_ptr(),
                frames,
                self.warm_output.as_mut_ptr(),
            )
        }
    }
}

impl Drop for FfmpegProcessor<'_> {
    fn drop(&mut self) {
        // SAFETY: creation validated this callback and callers stop before destruction.
        unsafe { (self.destroy)(self.graph.as_ptr()) };
    }
}

/// Persistent receive and transmit FFmpeg graphs for one standalone radio generation.
pub struct FfmpegPorts<'a> {
    receive: Pin<Box<FfmpegProcessor<'a>>>,
    transmit: Pin<Box<FfmpegProcessor<'a>>>,
}

impl<'a> FfmpegPorts<'a> {
    /// Prepare configured graphs before the audio stream starts.
    pub fn create(
        api: &'a abi::rptadv_ffmpeg_adapter_descriptor,
        radio: &crate::ResolvedRadioNode,
        maximum_frames: u32,
    ) -> Result<Self, ProcessingError> {
        Ok(Self {
            receive: FfmpegProcessor::create(api, &radio.settings.receive_graph, maximum_frames)?,
            transmit: FfmpegProcessor::create(api, &radio.settings.transmit_graph, maximum_frames)?,
        })
    }

    /// Return radio-core ports; retain this owner until the session is destroyed.
    pub fn ports(&mut self) -> abi::rptadv_radio_session_ports {
        self.ports_with_program(None)
    }

    /// Return radio-core ports with an optional externally owned exact-frame program source.
    pub fn ports_with_program(
        &mut self,
        program: Option<abi::rptadv_radio_program_ring_port>,
    ) -> abi::rptadv_radio_session_ports {
        let mut ports = unsafe { std::mem::zeroed::<abi::rptadv_radio_session_ports>() };
        ports.struct_size = std::mem::size_of_val(&ports) as u32;
        ports.receive_filter = self.receive.as_mut().port();
        ports.transmit_program = self.transmit.as_mut().port();
        if let Some(program) = program {
            ports.program_ring = program;
        }
        ports
    }
}

unsafe extern "C" fn process_f32(
    context: *mut c_void,
    input: *const f32,
    output: *mut f32,
    frame_count: u32,
) -> i32 {
    if output.is_null() || frame_count == 0 {
        return abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
    }
    // SAFETY: the ABI caller supplies an output span of frame_count samples.
    let output = unsafe { std::slice::from_raw_parts_mut(output, frame_count as usize) };
    let Some(processor) = (unsafe { context.cast::<FfmpegProcessor<'_>>().as_mut() }) else {
        output.fill(0.0);
        return abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
    };
    if input.is_null() || frame_count > processor.maximum_frames {
        output.fill(0.0);
        return abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
    }
    // SAFETY: this processor port has one serial owner and the ABI provides exact mono spans.
    let result = unsafe {
        (processor.process)(
            processor.graph.as_ptr(),
            input,
            frame_count,
            output.as_mut_ptr(),
        )
    };
    if result != abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_OK {
        output.fill(0.0);
    }
    result
}

unsafe extern "C" fn warm(context: *mut c_void, frame_count: u32) -> i32 {
    let Some(processor) = (unsafe { context.cast::<FfmpegProcessor<'_>>().as_mut() }) else {
        return abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
    };
    processor.run(frame_count)
}

unsafe extern "C" fn bypass(context: *mut c_void, frame_count: u32) -> i32 {
    let Some(processor) = (unsafe { context.cast::<FfmpegProcessor<'_>>().as_mut() }) else {
        return abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
    };
    processor.warm_input[..processor.maximum_frames as usize].fill(0.0);
    processor.run(frame_count)
}

#[cfg(test)]
mod tests {
    use super::FfmpegProcessor;
    use crate::abi;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    };

    static DESTROYED: AtomicUsize = AtomicUsize::new(0);
    static CREATE_RESULT: AtomicI32 = AtomicI32::new(0);
    static CREATE_GRAPH: AtomicBool = AtomicBool::new(true);
    static PROCESSED: AtomicUsize = AtomicUsize::new(0);
    static FAIL_PROCESS: AtomicBool = AtomicBool::new(false);
    static VALID_CONFIG: AtomicBool = AtomicBool::new(false);
    static GRAPH_PORTS_CONNECTED: AtomicBool = AtomicBool::new(false);
    static PROGRAM_PORT_CONNECTED: AtomicBool = AtomicBool::new(false);
    static SESSION_DESTROYED: AtomicUsize = AtomicUsize::new(0);
    static SESSION_CREATE_RESULT: AtomicI32 = AtomicI32::new(0);
    static SESSION_WARM_RESULT: AtomicI32 = AtomicI32::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn create(
        config: *const abi::rptadv_ffmpeg_graph_config,
        output: *mut *mut abi::rptadv_ffmpeg_graph,
    ) -> i32 {
        let config = unsafe { &*config };
        let description = unsafe { std::ffi::CStr::from_ptr(config.filter_description) }.to_bytes();
        if config.sample_rate_hz != 48_000
            || config.maximum_frame_count != 8
            || !matches!(description, b"volume=2" | b"anull")
        {
            return -1;
        }
        VALID_CONFIG.store(true, Ordering::Relaxed);
        if CREATE_GRAPH.load(Ordering::Relaxed) {
            unsafe { *output = 1_usize as *mut abi::rptadv_ffmpeg_graph };
        }
        CREATE_RESULT.load(Ordering::Relaxed)
    }

    unsafe extern "C" fn process_block(
        _: *mut abi::rptadv_ffmpeg_graph,
        input: *const f32,
        count: u32,
        output: *mut f32,
    ) -> i32 {
        PROCESSED.fetch_add(1, Ordering::Relaxed);
        if FAIL_PROCESS.load(Ordering::Relaxed) {
            return -2;
        }
        for index in 0..count as usize {
            unsafe { *output.add(index) = *input.add(index) * 2.0 };
        }
        0
    }

    unsafe extern "C" fn destroy(_: *mut abi::rptadv_ffmpeg_graph) {
        DESTROYED.fetch_add(1, Ordering::Relaxed);
    }

    fn adapter() -> abi::rptadv_ffmpeg_adapter_descriptor {
        abi::rptadv_ffmpeg_adapter_descriptor {
            struct_size: std::mem::size_of::<abi::rptadv_ffmpeg_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.ffmpeg".as_ptr(),
            create: Some(create),
            process: None,
            destroy: Some(destroy),
            process_block: Some(process_block),
        }
    }

    #[test]
    fn exact_block_port_creates_warms_processes_and_destroys_graph() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        DESTROYED.store(0, Ordering::Relaxed);
        CREATE_RESULT.store(0, Ordering::Relaxed);
        CREATE_GRAPH.store(true, Ordering::Relaxed);
        PROCESSED.store(0, Ordering::Relaxed);
        FAIL_PROCESS.store(false, Ordering::Relaxed);
        VALID_CONFIG.store(false, Ordering::Relaxed);
        let api = adapter();
        let mut graph = FfmpegProcessor::create(&api, "volume=2", 8).unwrap();
        let port = graph.as_mut().port();
        assert!(VALID_CONFIG.load(Ordering::Relaxed));
        assert_eq!(unsafe { port.warm.unwrap()(port.context, 4) }, 0);
        let mut output = [0.0; 4];
        let process = port.process_f32.unwrap();
        assert_eq!(
            unsafe {
                process(
                    port.context,
                    [0.25, -0.5, 0.0, 1.0].as_ptr(),
                    output.as_mut_ptr(),
                    4,
                )
            },
            0
        );
        assert_eq!(output, [0.5, -1.0, 0.0, 2.0]);
        assert!(
            PROCESSED.load(Ordering::Relaxed) >= 2,
            "session warmup and audio process"
        );
        drop(graph);
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn creation_rejects_incomplete_and_invalid_adapters_or_graph_settings() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let api = adapter();
        let mut incomplete = api;
        incomplete.create = None;
        assert_eq!(
            FfmpegProcessor::create(&incomplete, "anull", 8)
                .err()
                .unwrap(),
            super::ProcessingError::IncompleteAdapter
        );
        let mut incomplete = api;
        incomplete.destroy = None;
        assert_eq!(
            FfmpegProcessor::create(&incomplete, "anull", 8)
                .err()
                .unwrap(),
            super::ProcessingError::IncompleteAdapter
        );
        let mut incomplete = api;
        incomplete.process_block = None;
        assert_eq!(
            FfmpegProcessor::create(&incomplete, "anull", 8)
                .err()
                .unwrap(),
            super::ProcessingError::IncompleteAdapter
        );

        let mut invalid = api;
        invalid.abi_version = 2;
        assert_eq!(
            FfmpegProcessor::create(&invalid, "anull", 8).err().unwrap(),
            super::ProcessingError::InvalidConfiguration
        );
        assert_eq!(
            FfmpegProcessor::create(&api, "anull", 0).err().unwrap(),
            super::ProcessingError::InvalidConfiguration
        );
        assert_eq!(
            FfmpegProcessor::create(&api, "bad\0filter", 8)
                .err()
                .unwrap(),
            super::ProcessingError::InvalidConfiguration
        );
    }

    #[test]
    fn creation_reports_adapter_errors_and_missing_graphs() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        DESTROYED.store(0, Ordering::Relaxed);
        let api = adapter();
        CREATE_RESULT.store(-4, Ordering::Relaxed);
        CREATE_GRAPH.store(true, Ordering::Relaxed);
        assert_eq!(
            FfmpegProcessor::create(&api, "anull", 8).err().unwrap(),
            super::ProcessingError::Adapter(-4)
        );
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 1);

        CREATE_GRAPH.store(false, Ordering::Relaxed);
        assert_eq!(
            FfmpegProcessor::create(&api, "anull", 8).err().unwrap(),
            super::ProcessingError::Adapter(-4)
        );
        CREATE_RESULT.store(0, Ordering::Relaxed);
        assert_eq!(
            FfmpegProcessor::create(&api, "anull", 8).err().unwrap(),
            super::ProcessingError::MissingGraph
        );
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 1);
        CREATE_GRAPH.store(true, Ordering::Relaxed);
    }

    #[test]
    fn error_display_is_specific_without_exposing_adapter_data() {
        assert_eq!(
            super::ProcessingError::IncompleteAdapter.to_string(),
            "FFmpeg exact-block adapter is incomplete"
        );
        assert_eq!(
            super::ProcessingError::InvalidConfiguration.to_string(),
            "invalid FFmpeg graph settings"
        );
        assert_eq!(
            super::ProcessingError::Adapter(-5).to_string(),
            "FFmpeg graph operation failed (-5)"
        );
        assert_eq!(
            super::ProcessingError::MissingGraph.to_string(),
            "FFmpeg returned no graph"
        );
    }

    #[test]
    fn callback_boundaries_reject_invalid_spans_and_contexts() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let api = adapter();
        let mut graph = FfmpegProcessor::create(&api, "anull", 8).unwrap();
        let port = graph.as_mut().port();
        let process = port.process_f32.unwrap();
        let warm = port.warm.unwrap();
        let bypass = port.bypass.unwrap();
        let mut output = [1.0; 9];
        let invalid = abi::rptadv_ffmpeg_adapter_result_RPTADV_FFMPEG_ADAPTER_INVALID_ARGUMENT;
        assert_eq!(
            unsafe { process(port.context, [0.0; 1].as_ptr(), std::ptr::null_mut(), 1) },
            invalid
        );
        assert_eq!(
            unsafe { process(port.context, [0.0; 1].as_ptr(), output.as_mut_ptr(), 0) },
            invalid
        );
        assert_eq!(
            unsafe {
                process(
                    std::ptr::null_mut(),
                    [0.0; 1].as_ptr(),
                    output.as_mut_ptr(),
                    1,
                )
            },
            invalid
        );
        assert_eq!(&output[..1], [0.0]);
        output.fill(1.0);
        assert_eq!(
            unsafe { process(port.context, std::ptr::null(), output.as_mut_ptr(), 1) },
            invalid
        );
        assert_eq!(&output[..1], [0.0]);
        output.fill(1.0);
        assert_eq!(
            unsafe { process(port.context, [0.0; 9].as_ptr(), output.as_mut_ptr(), 9) },
            invalid
        );
        assert_eq!(output, [0.0; 9]);
        assert_eq!(unsafe { warm(std::ptr::null_mut(), 1) }, invalid);
        assert_eq!(unsafe { bypass(std::ptr::null_mut(), 1) }, invalid);
        assert_eq!(unsafe { warm(port.context, 0) }, invalid);
        assert_eq!(unsafe { bypass(port.context, 9) }, invalid);
        assert_eq!(unsafe { bypass(port.context, 4) }, 0);
        assert_eq!(PROCESSED.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn graph_failure_mutes_the_full_output_and_bad_frames_are_rejected() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        FAIL_PROCESS.store(false, Ordering::Relaxed);
        VALID_CONFIG.store(false, Ordering::Relaxed);
        let api = adapter();
        let mut graph = FfmpegProcessor::create(&api, "anull", 8).unwrap();
        let port = graph.as_mut().port();
        let process = port.process_f32.unwrap();
        let mut output = [1.0; 9];
        assert_ne!(
            unsafe { process(port.context, [0.0; 9].as_ptr(), output.as_mut_ptr(), 9) },
            0
        );
        assert_eq!(output, [0.0; 9]);
        output.fill(1.0);
        FAIL_PROCESS.store(true, Ordering::Relaxed);
        assert_ne!(
            unsafe { process(port.context, [0.0; 3].as_ptr(), output.as_mut_ptr(), 3) },
            0
        );
        assert_eq!(&output[..3], [0.0; 3]);
        assert_eq!(&output[3..], [1.0; 6]);
        FAIL_PROCESS.store(false, Ordering::Relaxed);
    }

    #[test]
    fn configured_graphs_are_retained_in_the_radio_filter_and_program_ports() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        FAIL_PROCESS.store(false, Ordering::Relaxed);
        let api = adapter();
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\nreceive_graph=volume=2\ntransmit_graph=anull\n[1000]\n",
        )
        .unwrap();
        let radio = crate::resolve_radio_nodes(&document)
            .unwrap()
            .value
            .remove(0);
        let mut graphs = super::FfmpegPorts::create(&api, &radio, 8).unwrap();
        let ports = graphs.ports();

        assert!(ports.receive_filter.process_f32.is_some());
        assert!(ports.transmit_program.process_f32.is_some());
        assert!(ports.receive_deemphasis.process_f32.is_none());
        assert!(ports.receive_noise_reduction.process_f32.is_none());
        assert!(ports.receive_dynamics.process_f32.is_none());
        assert!(ports.transmit_dcs_normal_filter.process_f32.is_none());
    }

    #[test]
    fn prepared_radio_generation_owns_graph_ports_through_session_destroy() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        FAIL_PROCESS.store(false, Ordering::Relaxed);
        GRAPH_PORTS_CONNECTED.store(false, Ordering::Relaxed);
        SESSION_DESTROYED.store(0, Ordering::Relaxed);
        SESSION_CREATE_RESULT.store(0, Ordering::Relaxed);
        SESSION_WARM_RESULT.store(0, Ordering::Relaxed);
        let ffmpeg = adapter();
        let radio_api = radio_adapter();
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\nreceive_graph=volume=2\ntransmit_graph=anull\n[1000]\n",
        )
        .unwrap();
        let radio = crate::resolve_radio_nodes(&document)
            .unwrap()
            .value
            .remove(0);
        let mut generation = crate::radio_generation::PreparedRadioGeneration::prepare(
            &radio_api, &ffmpeg, &radio, 1, 8, 50,
        )
        .unwrap();

        assert!(GRAPH_PORTS_CONNECTED.load(Ordering::Relaxed));
        let (receive, transmit, program) = generation.split().unwrap();
        assert!(program.is_none());
        let _ = (receive, transmit);
        drop(generation);
        assert_eq!(SESSION_DESTROYED.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn generation_reports_an_incomplete_ffmpeg_adapter() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let ffmpeg: abi::rptadv_ffmpeg_adapter_descriptor = unsafe { std::mem::zeroed() };
        let radio_api = radio_adapter();
        let radio = resolved_radio("receive_graph=anull\ntransmit_graph=anull\n");

        assert!(matches!(
            crate::radio_generation::PreparedRadioGeneration::prepare(
                &radio_api, &ffmpeg, &radio, 1, 8, 50
            ),
            Err(crate::radio_generation::RadioGenerationError::Processing(
                crate::processing::ProcessingError::IncompleteAdapter
            ))
        ));
    }

    #[test]
    fn prepared_generation_connects_the_external_program_source() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        FAIL_PROCESS.store(false, Ordering::Relaxed);
        PROGRAM_PORT_CONNECTED.store(false, Ordering::Relaxed);
        SESSION_CREATE_RESULT.store(0, Ordering::Relaxed);
        SESSION_WARM_RESULT.store(0, Ordering::Relaxed);
        let ffmpeg = adapter();
        let radio_api = radio_adapter();
        let radio = resolved_radio("receive_graph=anull\ntransmit_graph=anull\n");
        let audio = crate::program_audio::ProgramAudio::new(
            Some(dummy_product_tx),
            std::ptr::null_mut(),
            8,
        )
        .unwrap();
        let mut generation =
            crate::radio_generation::PreparedRadioGeneration::prepare_with_program(
                &radio_api, &ffmpeg, &radio, 1, 8, 50, audio,
            )
            .unwrap();

        assert!(PROGRAM_PORT_CONNECTED.load(Ordering::Relaxed));
        {
            let (receive, transmit, program) = generation.split().unwrap();
            assert!(program.is_some());
            let _ = (receive, transmit, program);
        }
        drop(generation);
    }

    #[test]
    fn prepared_generation_rejects_invalid_bounds_before_graph_creation() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let ffmpeg = adapter();
        let radio_api = radio_adapter();
        let radio = resolved_radio("receive_graph=anull\ntransmit_graph=anull\n");

        let error = crate::radio_generation::PreparedRadioGeneration::prepare(
            &radio_api, &ffmpeg, &radio, 0, 8, 50,
        )
        .err()
        .unwrap();

        assert_eq!(
            error,
            crate::radio_generation::RadioGenerationError::Configuration(
                crate::radio_config::RadioConfigError::InvalidBounds
            )
        );
    }

    #[test]
    fn prepared_generation_destroys_partial_session_after_warmup_failure() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        SESSION_DESTROYED.store(0, Ordering::Relaxed);
        SESSION_CREATE_RESULT.store(0, Ordering::Relaxed);
        SESSION_WARM_RESULT.store(-8, Ordering::Relaxed);
        let ffmpeg = adapter();
        let radio_api = radio_adapter();
        let radio = resolved_radio("receive_graph=volume=2\ntransmit_graph=anull\n");

        let error = crate::radio_generation::PreparedRadioGeneration::prepare(
            &radio_api, &ffmpeg, &radio, 1, 8, 50,
        )
        .err()
        .unwrap();

        assert_eq!(
            error,
            crate::radio_generation::RadioGenerationError::Session(
                crate::radio_session::RadioSessionError::Warm(-8)
            )
        );
        assert_eq!(SESSION_DESTROYED.load(Ordering::Relaxed), 1);
        SESSION_WARM_RESULT.store(0, Ordering::Relaxed);
    }

    fn resolved_radio(overrides: &str) -> crate::ResolvedRadioNode {
        let document = rpt_advanced_core::config::ConfigDocument::parse(&format!(
            "[radio]\n{overrides}[1000]\n"
        ))
        .unwrap();
        crate::resolve_radio_nodes(&document)
            .unwrap()
            .value
            .remove(0)
    }

    unsafe extern "C" fn radio_create(
        _: *const abi::rptadv_radio_session_config,
        ports: *const abi::rptadv_radio_session_ports,
        output: *mut *mut abi::rptadv_radio_session,
    ) -> abi::rptadv_radio_result {
        let ports = unsafe { &*ports };
        GRAPH_PORTS_CONNECTED.store(
            ports.receive_filter.process_f32.is_some()
                && ports.transmit_program.process_f32.is_some(),
            Ordering::Relaxed,
        );
        PROGRAM_PORT_CONNECTED.store(ports.program_ring.render_f32.is_some(), Ordering::Relaxed);
        unsafe { *output = 1_usize as *mut abi::rptadv_radio_session };
        SESSION_CREATE_RESULT.load(Ordering::Relaxed)
    }

    unsafe extern "C" fn radio_warm(_: *mut abi::rptadv_radio_session) -> abi::rptadv_radio_result {
        SESSION_WARM_RESULT.load(Ordering::Relaxed)
    }

    unsafe extern "C" fn radio_destroy(_: *mut abi::rptadv_radio_session) {
        SESSION_DESTROYED.fetch_add(1, Ordering::Relaxed);
    }

    fn radio_adapter() -> abi::rptadv_radio_descriptor {
        let mut descriptor: abi::rptadv_radio_descriptor = unsafe { std::mem::zeroed() };
        descriptor.session_create = Some(radio_create);
        descriptor.session_warm = Some(radio_warm);
        descriptor.session_receive = Some(radio_receive);
        descriptor.session_transmit = Some(radio_transmit);
        descriptor.session_destroy = Some(radio_destroy);
        descriptor
    }

    unsafe extern "C" fn radio_receive(
        _: *mut abi::rptadv_radio_session,
        input: *const f32,
        output: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_receive_input,
        result: *mut abi::rptadv_radio_receive_result,
    ) -> abi::rptadv_radio_result {
        unsafe {
            std::slice::from_raw_parts_mut(output, frames as usize * 2)
                .copy_from_slice(std::slice::from_raw_parts(input, frames as usize * 2));
            (*result).frame_count = frames;
        }
        0
    }

    unsafe extern "C" fn radio_transmit(
        _: *mut abi::rptadv_radio_session,
        output: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_transmit_input,
        result: *mut abi::rptadv_radio_transmit_result,
    ) -> abi::rptadv_radio_result {
        unsafe {
            std::slice::from_raw_parts_mut(output, frames as usize * 2).fill(0.0);
            (*result).frame_count = frames;
        }
        0
    }

    unsafe extern "C" fn dummy_product_tx(
        _: *mut std::ffi::c_void,
        _: *mut f32,
        _: u32,
        _: *mut u32,
        _: *mut u32,
    ) -> i32 {
        0
    }
}
