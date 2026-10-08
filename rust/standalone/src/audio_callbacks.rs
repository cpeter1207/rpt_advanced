//! Preallocated PortAudio-to-product workers around one native radio generation.

use crate::{
    abi,
    program_audio::ProgramAudio,
    radio_session::{ReceiveEndpoint, TransmitEndpoint},
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

/// Lock-free values exchanged between radio callbacks and the GPIO service thread.
#[derive(Default)]
pub struct StandaloneRadioState {
    hardware_carrier: AtomicU32,
    hardware_subaudible: AtomicU32,
    subaudible_override: AtomicU32,
    physical_ptt: AtomicU32,
    logical_ptt: AtomicU32,
    ctcss_enabled: AtomicU32,
}

impl StandaloneRadioState {
    /// Publish current CM119 or parallel-port receive signal inputs.
    pub fn set_receive_inputs(&self, carrier: bool, subaudible: bool, override_enabled: bool) {
        self.hardware_carrier
            .store(u32::from(carrier), Ordering::Release);
        self.hardware_subaudible
            .store(u32::from(subaudible), Ordering::Release);
        self.subaudible_override
            .store(u32::from(override_enabled), Ordering::Release);
    }

    /// Publish the physical PTT feedback observed by the GPIO service.
    pub fn set_physical_ptt(&self, applied: bool) {
        self.physical_ptt
            .store(u32::from(applied), Ordering::Release);
    }

    /// Return the latest lock-free receiver signal snapshot.
    pub fn receive_inputs(&self) -> (bool, bool, bool) {
        (
            self.hardware_carrier.load(Ordering::Acquire) != 0,
            self.hardware_subaudible.load(Ordering::Acquire) != 0,
            self.subaudible_override.load(Ordering::Acquire) != 0,
        )
    }

    /// Return the latest PTT state accepted by the GPIO output adapter.
    pub fn physical_ptt(&self) -> bool {
        self.physical_ptt.load(Ordering::Acquire) != 0
    }

    /// Read logical PTT intent for the GPIO service to apply off the audio callback.
    pub fn logical_ptt(&self) -> bool {
        self.logical_ptt.load(Ordering::Acquire) != 0
    }

    #[cfg(test)]
    pub(crate) fn set_logical_ptt_for_test(&self, requested: bool) {
        self.logical_ptt
            .store(u32::from(requested), Ordering::Release);
    }

    /// Read whether the active transmit span requires CTCSS for the GPIO service.
    pub fn ctcss_enabled(&self) -> bool {
        self.ctcss_enabled.load(Ordering::Acquire) != 0
    }
}

/// An audio callback bridge could not process its exact bounded span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioCallbackError {
    /// Configuration or supplied frame span is invalid.
    InvalidFrame,
    /// The product callback failed.
    ProductCallback,
    /// The native radio session failed.
    Radio(crate::radio_session::RadioSessionError),
    /// The product program source failed.
    Program(crate::program_audio::ProgramAudioError),
}

impl std::fmt::Display for AudioCallbackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFrame => formatter.write_str("invalid audio callback frame"),
            Self::ProductCallback => formatter.write_str("product audio callback failed"),
            Self::Radio(error) => error.fmt(formatter),
            Self::Program(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AudioCallbackError {}

/// Input-clocked RX path: hardware stereo through radio DSP, then qualified mono to product.
pub struct NativeReceiveWorker<'s, 'a> {
    endpoint: ReceiveEndpoint<'s, 'a>,
    product: abi::rptadv_radio_receive_v2,
    product_context: *mut c_void,
    maximum_frames: usize,
    channel: usize,
    input_channels: usize,
    input_stereo: Vec<f32>,
    stereo: Vec<f32>,
    mono: Vec<f32>,
    state: Arc<StandaloneRadioState>,
}

impl<'s, 'a> NativeReceiveWorker<'s, 'a> {
    /// Prepare callback scratch before the PortAudio stream starts.
    pub fn new(
        endpoint: ReceiveEndpoint<'s, 'a>,
        product: abi::rptadv_radio_receive_v2,
        product_context: *mut c_void,
        maximum_frames: u32,
        channel: u32,
        input_channels: u32,
        state: Arc<StandaloneRadioState>,
    ) -> Result<Self, AudioCallbackError> {
        if product.is_none()
            || maximum_frames == 0
            || channel > 1
            || !(1..=2).contains(&input_channels)
        {
            return Err(AudioCallbackError::InvalidFrame);
        }
        let frames = maximum_frames as usize;
        Ok(Self {
            endpoint,
            product,
            product_context,
            maximum_frames: frames,
            channel: channel as usize,
            input_channels: input_channels as usize,
            input_stereo: vec![0.0; frames * 2],
            stereo: vec![0.0; frames * 2],
            mono: vec![0.0; frames],
            state,
        })
    }

    /// Process one exact native-rate stereo block and deliver its selected mono receive channel.
    pub fn process(
        &mut self,
        input: &[f32],
        frames: u32,
    ) -> Result<abi::rptadv_radio_receive_result, AudioCallbackError> {
        let frames_usize = frames as usize;
        if frames == 0
            || frames_usize > self.maximum_frames
            || input.len() != frames_usize * self.input_channels
        {
            return Err(AudioCallbackError::InvalidFrame);
        }
        if self.input_channels == 1 {
            for (sample, stereo) in input
                .iter()
                .zip(self.input_stereo[..frames_usize * 2].chunks_exact_mut(2))
            {
                stereo.fill(*sample);
            }
        } else {
            self.input_stereo[..frames_usize * 2].copy_from_slice(input);
        }
        let controls = abi::rptadv_radio_receive_input {
            hardware_carrier: self.state.hardware_carrier.load(Ordering::Acquire),
            parallel_carrier: 0,
            hardware_subaudible: self.state.hardware_subaudible.load(Ordering::Acquire),
            parallel_subaudible: 0,
            subaudible_override: self.state.subaudible_override.load(Ordering::Acquire),
        };
        let result = self
            .endpoint
            .process(
                &self.input_stereo[..frames_usize * 2],
                &mut self.stereo[..frames_usize * 2],
                &controls,
            )
            .map_err(AudioCallbackError::Radio)?;
        if result.frame_count != frames {
            self.mono[..frames_usize].fill(0.0);
            return Err(AudioCallbackError::InvalidFrame);
        }
        for (mono, stereo) in self.mono[..frames_usize]
            .iter_mut()
            .zip(self.stereo[..frames_usize * 2].chunks_exact(2))
        {
            *mono = stereo[self.channel];
        }
        // SAFETY: constructor validated this callback and its context remains borrowed through worker use.
        let callback = unsafe { self.product.unwrap_unchecked() };
        // SAFETY: mono contains exactly `frames` preallocated writable samples.
        let code = unsafe {
            callback(
                self.product_context,
                result.receiver_keyed,
                self.mono.as_mut_ptr(),
                frames,
            )
        };
        if code != 0 {
            self.mono[..frames_usize].fill(0.0);
            return Err(AudioCallbackError::ProductCallback);
        }
        Ok(result)
    }

    /// Return the PortAudio capture-worker entry point.
    pub fn callback() -> abi::rptadv_audio_receive_worker {
        Some(receive_callback)
    }
}

/// Output-clocked TX path: product mono program through radio DSP/signaling to stereo hardware.
pub struct NativeTransmitWorker<'s, 'a> {
    endpoint: TransmitEndpoint<'s, 'a>,
    program: Pin<&'s mut ProgramAudio>,
    maximum_frames: usize,
    output_channels: usize,
    stereo: Vec<f32>,
    state: Arc<StandaloneRadioState>,
}

impl<'s, 'a> NativeTransmitWorker<'s, 'a> {
    /// Retain the pinned product source whose context is installed in the radio session.
    pub fn new(
        endpoint: TransmitEndpoint<'s, 'a>,
        program: Pin<&'s mut ProgramAudio>,
        maximum_frames: u32,
        output_channels: u32,
        state: Arc<StandaloneRadioState>,
    ) -> Result<Self, AudioCallbackError> {
        if maximum_frames == 0
            || maximum_frames > program.as_ref().maximum_frames()
            || !(1..=2).contains(&output_channels)
        {
            return Err(AudioCallbackError::InvalidFrame);
        }
        Ok(Self {
            endpoint,
            program,
            maximum_frames: maximum_frames as usize,
            output_channels: output_channels as usize,
            stereo: vec![0.0; maximum_frames as usize * 2],
            state,
        })
    }

    /// Render one exact stereo output block without allocating or waiting.
    pub fn process(
        &mut self,
        output: &mut [f32],
        frames: u32,
    ) -> Result<abi::rptadv_radio_transmit_result, AudioCallbackError> {
        let frames_usize = frames as usize;
        if frames == 0
            || frames_usize > self.maximum_frames
            || output.len() != frames_usize * self.output_channels
        {
            output.fill(0.0);
            self.clear_transmit_state();
            return Err(AudioCallbackError::InvalidFrame);
        }
        let (keyed, ctcss_enabled) = match self.program.as_mut().prepare(frames) {
            Ok(state) => state,
            Err(error) => {
                output.fill(0.0);
                self.clear_transmit_state();
                return Err(AudioCallbackError::Program(error));
            }
        };
        let controls = abi::rptadv_radio_transmit_input {
            external_ptt_request: keyed,
            physical_ptt_applied: self.state.physical_ptt.load(Ordering::Acquire),
            render_admitted: 1,
            ctcss_inhibit: u32::from(ctcss_enabled == 0),
            calibrated_test_tone: 0,
            forced_ctcss_tenths_hz: 0,
        };
        match self
            .endpoint
            .process(&mut self.stereo[..frames_usize * 2], &controls)
        {
            Ok(result) if result.frame_count == frames => {
                if self.output_channels == 1 {
                    for (sample, stereo) in output
                        .iter_mut()
                        .zip(self.stereo[..frames_usize * 2].chunks_exact(2))
                    {
                        *sample = (stereo[0] + stereo[1]) * 0.5;
                    }
                } else {
                    output.copy_from_slice(&self.stereo[..frames_usize * 2]);
                }
                self.state
                    .logical_ptt
                    .store(result.logical_ptt, Ordering::Release);
                self.state.ctcss_enabled.store(
                    u32::from(result.selected_ctcss_tenths_hz != 0),
                    Ordering::Release,
                );
                Ok(result)
            }
            Ok(_) => {
                output.fill(0.0);
                self.clear_transmit_state();
                Err(AudioCallbackError::InvalidFrame)
            }
            Err(error) => {
                output.fill(0.0);
                self.clear_transmit_state();
                Err(AudioCallbackError::Radio(error))
            }
        }
    }

    /// Return the PortAudio playback-worker entry point.
    pub fn callback() -> abi::rptadv_audio_transmit_worker {
        Some(transmit_callback)
    }

    fn clear_transmit_state(&self) {
        self.state.logical_ptt.store(0, Ordering::Release);
        self.state.ctcss_enabled.store(0, Ordering::Release);
    }
}

unsafe extern "C" fn receive_callback(context: *mut c_void, input: *const f32, frames: u32) -> i32 {
    let Some(worker) = (unsafe { context.cast::<NativeReceiveWorker<'_, '_>>().as_mut() }) else {
        return -1;
    };
    if input.is_null() || frames == 0 || frames as usize > worker.maximum_frames {
        return -1;
    }
    // SAFETY: the configured capture-channel count determines the exact input span.
    let input =
        unsafe { std::slice::from_raw_parts(input, frames as usize * worker.input_channels) };
    match catch_unwind(AssertUnwindSafe(|| worker.process(input, frames))) {
        Ok(Ok(_)) => 0,
        Ok(Err(_)) | Err(_) => -1,
    }
}

unsafe extern "C" fn transmit_callback(context: *mut c_void, output: *mut f32, frames: u32) -> i32 {
    let Some(worker) = (unsafe { context.cast::<NativeTransmitWorker<'_, '_>>().as_mut() }) else {
        return -1;
    };
    if output.is_null() || frames == 0 {
        worker.clear_transmit_state();
        return -1;
    }
    // SAFETY: the configured playback-channel count determines the exact output span.
    let output =
        unsafe { std::slice::from_raw_parts_mut(output, frames as usize * worker.output_channels) };
    if frames as usize > worker.maximum_frames {
        output.fill(0.0);
        worker.clear_transmit_state();
        return -1;
    }
    match catch_unwind(AssertUnwindSafe(|| worker.process(output, frames))) {
        Ok(Ok(_)) => 0,
        Ok(Err(_)) | Err(_) => {
            output.fill(0.0);
            worker.clear_transmit_state();
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeReceiveWorker, NativeTransmitWorker, StandaloneRadioState};
    use crate::{abi, program_audio::ProgramAudio, radio_session::NativeRadioSession};
    use std::{ffi::c_void, sync::Arc};

    struct FakeSession {
        program: abi::rptadv_radio_program_ring_port,
    }

    struct ReceiveProbe {
        samples: Vec<f32>,
        receiving: u32,
    }

    unsafe extern "C" fn create(
        _: *const abi::rptadv_radio_session_config,
        ports: *const abi::rptadv_radio_session_ports,
        output: *mut *mut abi::rptadv_radio_session,
    ) -> i32 {
        let session = Box::new(FakeSession {
            program: unsafe { (*ports).program_ring },
        });
        unsafe { output.write(Box::into_raw(session).cast()) };
        0
    }

    unsafe extern "C" fn warm(_: *mut abi::rptadv_radio_session) -> i32 {
        0
    }

    unsafe extern "C" fn destroy(session: *mut abi::rptadv_radio_session) {
        drop(unsafe { Box::from_raw(session.cast::<FakeSession>()) });
    }

    unsafe extern "C" fn receive(
        _: *mut abi::rptadv_radio_session,
        input: *const f32,
        output: *mut f32,
        frames: u32,
        controls: *const abi::rptadv_radio_receive_input,
        result: *mut abi::rptadv_radio_receive_result,
    ) -> i32 {
        let samples = unsafe { std::slice::from_raw_parts(input, frames as usize * 2) };
        unsafe { std::slice::from_raw_parts_mut(output, frames as usize * 2) }
            .copy_from_slice(samples);
        unsafe {
            (*result).frame_count = frames;
            (*result).receiver_keyed = (*controls).hardware_carrier;
        }
        0
    }

    unsafe extern "C" fn receive_wrong_frame_count(
        _: *mut abi::rptadv_radio_session,
        _: *const f32,
        _: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_receive_input,
        result: *mut abi::rptadv_radio_receive_result,
    ) -> i32 {
        unsafe { (*result).frame_count = frames + 1 };
        0
    }

    unsafe extern "C" fn transmit(
        session: *mut abi::rptadv_radio_session,
        output: *mut f32,
        frames: u32,
        controls: *const abi::rptadv_radio_transmit_input,
        result: *mut abi::rptadv_radio_transmit_result,
    ) -> i32 {
        let fake = unsafe { &*session.cast::<FakeSession>() };
        let mut mono = vec![0.0; frames as usize];
        let mut ring_result: abi::rptadv_radio_program_ring_result = unsafe { std::mem::zeroed() };
        let status = unsafe {
            fake.program.render_f32.unwrap()(
                fake.program.context,
                mono.as_mut_ptr(),
                frames,
                &mut ring_result,
            )
        };
        if status != 0 {
            return status;
        }
        for (index, sample) in mono.into_iter().enumerate() {
            unsafe {
                *output.add(index * 2) = sample;
                *output.add(index * 2 + 1) = sample;
            }
        }
        unsafe {
            (*result).frame_count = frames;
            (*result).logical_ptt = (*controls).external_ptt_request;
            (*result).selected_ctcss_tenths_hz = if (*controls).ctcss_inhibit == 0 {
                1000
            } else {
                0
            };
        }
        0
    }

    unsafe extern "C" fn transmit_error(
        _: *mut abi::rptadv_radio_session,
        _: *mut f32,
        _: u32,
        _: *const abi::rptadv_radio_transmit_input,
        _: *mut abi::rptadv_radio_transmit_result,
    ) -> i32 {
        -1
    }

    unsafe extern "C" fn transmit_wrong_frame_count(
        _: *mut abi::rptadv_radio_session,
        _: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_transmit_input,
        result: *mut abi::rptadv_radio_transmit_result,
    ) -> i32 {
        unsafe { (*result).frame_count = frames + 1 };
        0
    }

    fn session_descriptor() -> abi::rptadv_radio_descriptor {
        let mut api = unsafe { std::mem::zeroed::<abi::rptadv_radio_descriptor>() };
        api.session_create = Some(create);
        api.session_warm = Some(warm);
        api.session_receive = Some(receive);
        api.session_transmit = Some(transmit);
        api.session_destroy = Some(destroy);
        api
    }

    fn session_with_descriptor(
        api: abi::rptadv_radio_descriptor,
        program: Option<abi::rptadv_radio_program_ring_port>,
    ) -> NativeRadioSession<'static> {
        let api = Box::leak(Box::new(api));
        let mut config = unsafe { std::mem::zeroed::<abi::rptadv_radio_session_config>() };
        config.maximum_receive_frame_count = 8;
        config.maximum_transmit_frame_count = 8;
        let mut ports = unsafe { std::mem::zeroed::<abi::rptadv_radio_session_ports>() };
        ports.program_ring = program.unwrap_or_else(|| unsafe { std::mem::zeroed() });
        let ports = Box::leak(Box::new(ports));
        NativeRadioSession::prepare(api, &config, ports).unwrap()
    }

    fn session(
        program: Option<abi::rptadv_radio_program_ring_port>,
    ) -> NativeRadioSession<'static> {
        session_with_descriptor(session_descriptor(), program)
    }

    unsafe extern "C" fn product_receive(
        context: *mut c_void,
        receiving: u32,
        samples: *mut f32,
        count: u32,
    ) -> i32 {
        let probe = unsafe { &mut *context.cast::<ReceiveProbe>() };
        probe.receiving = receiving;
        probe.samples = unsafe { std::slice::from_raw_parts(samples, count as usize) }.to_vec();
        0
    }

    unsafe extern "C" fn product_receive_error(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
        -1
    }

    unsafe extern "C" fn product_transmit(
        _: *mut c_void,
        samples: *mut f32,
        count: u32,
        keyed: *mut u32,
        ctcss_enabled: *mut u32,
    ) -> i32 {
        unsafe {
            std::slice::from_raw_parts_mut(samples, count as usize).fill(0.375);
            *keyed = 1;
            *ctcss_enabled = 1;
        }
        0
    }

    unsafe extern "C" fn product_transmit_error(
        _: *mut c_void,
        _: *mut f32,
        _: u32,
        _: *mut u32,
        _: *mut u32,
    ) -> i32 {
        -1
    }

    fn receive_worker_result(
        product: abi::rptadv_radio_receive_v2,
        maximum_frames: u32,
        channel: u32,
        input_channels: u32,
    ) -> Result<(), super::AudioCallbackError> {
        let mut session = session(None);
        let (receive, _) = session.split().unwrap();
        NativeReceiveWorker::new(
            receive,
            product,
            std::ptr::null_mut(),
            maximum_frames,
            channel,
            input_channels,
            Arc::new(StandaloneRadioState::default()),
        )
        .map(|_| ())
    }

    fn transmit_worker_result(
        program_frames: u32,
        maximum_frames: u32,
        output_channels: u32,
    ) -> Result<(), super::AudioCallbackError> {
        let mut program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), program_frames)
                .unwrap();
        let port = program.as_mut().port();
        let mut session = session(Some(port));
        let (_, transmit) = session.split().unwrap();
        NativeTransmitWorker::new(
            transmit,
            program.as_mut(),
            maximum_frames,
            output_channels,
            Arc::new(StandaloneRadioState::default()),
        )
        .map(|_| ())
    }

    #[test]
    fn radio_state_publishes_and_reads_receiver_and_physical_ptt_inputs() {
        let state = StandaloneRadioState::default();

        assert_eq!(state.receive_inputs(), (false, false, false));
        assert!(!state.physical_ptt());

        state.set_receive_inputs(true, true, false);
        state.set_physical_ptt(true);
        assert_eq!(state.receive_inputs(), (true, true, false));
        assert!(state.physical_ptt());

        state.set_receive_inputs(false, false, true);
        state.set_physical_ptt(false);
        assert_eq!(state.receive_inputs(), (false, false, true));
        assert!(!state.physical_ptt());
    }

    #[test]
    fn audio_callback_errors_display_wrapped_radio_and_program_failures() {
        assert_eq!(
            super::AudioCallbackError::InvalidFrame.to_string(),
            "invalid audio callback frame"
        );
        assert_eq!(
            super::AudioCallbackError::ProductCallback.to_string(),
            "product audio callback failed"
        );
        assert_eq!(
            super::AudioCallbackError::Radio(crate::radio_session::RadioSessionError::Process(-7))
                .to_string(),
            "radio session callback failed (-7)"
        );
        assert_eq!(
            super::AudioCallbackError::Program(
                crate::program_audio::ProgramAudioError::ProductCallback
            )
            .to_string(),
            "product transmit callback failed"
        );
    }

    #[test]
    fn receive_worker_rejects_each_invalid_callback_shape() {
        let valid_product: abi::rptadv_radio_receive_v2 = Some(product_receive);
        for (product, maximum_frames, channel, input_channels) in [
            (None, 8, 0, 1),
            (valid_product, 0, 0, 1),
            (valid_product, 8, 2, 1),
            (valid_product, 8, 0, 0),
            (valid_product, 8, 0, 3),
        ] {
            assert_eq!(
                receive_worker_result(product, maximum_frames, channel, input_channels),
                Err(super::AudioCallbackError::InvalidFrame)
            );
        }
    }

    #[test]
    fn receive_worker_rejects_empty_oversized_and_mismatched_spans() {
        let mut session = session(None);
        let (receive, _) = session.split().unwrap();
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive),
            std::ptr::null_mut(),
            4,
            0,
            1,
            Arc::new(StandaloneRadioState::default()),
        )
        .unwrap();

        assert_eq!(
            worker.process(&[], 0).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );
        assert_eq!(
            worker.process(&[0.0; 5], 5).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );
        assert_eq!(
            worker.process(&[0.0; 3], 2).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );
    }

    #[test]
    fn receive_worker_silences_core_frame_mismatch_and_product_failure() {
        let mut api = session_descriptor();
        api.session_receive = Some(receive_wrong_frame_count);
        let mut radio_session = session_with_descriptor(api, None);
        let (receive, _) = radio_session.split().unwrap();
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive),
            std::ptr::null_mut(),
            4,
            0,
            1,
            Arc::new(StandaloneRadioState::default()),
        )
        .unwrap();

        assert_eq!(
            worker.process(&[0.25], 1).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );

        let mut radio_session = session(None);
        let (receive, _) = radio_session.split().unwrap();
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive_error),
            std::ptr::null_mut(),
            4,
            0,
            1,
            Arc::new(StandaloneRadioState::default()),
        )
        .unwrap();
        assert_eq!(
            worker.process(&[0.25], 1).err(),
            Some(super::AudioCallbackError::ProductCallback)
        );
    }

    #[test]
    fn receive_callback_rejects_null_context_input_and_invalid_frame_count() {
        let input = [0.25, 0.5];
        assert_eq!(
            unsafe { super::receive_callback(std::ptr::null_mut(), input.as_ptr(), 1) },
            -1
        );

        let mut radio_session = session(None);
        let (receive, _) = radio_session.split().unwrap();
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive),
            std::ptr::null_mut(),
            1,
            0,
            1,
            Arc::new(StandaloneRadioState::default()),
        )
        .unwrap();
        let context = std::ptr::from_mut(&mut worker).cast();
        assert_eq!(
            unsafe { super::receive_callback(context, std::ptr::null(), 1) },
            -1
        );
        assert_eq!(
            unsafe { super::receive_callback(context, input.as_ptr(), 0) },
            -1
        );
        assert_eq!(
            unsafe { super::receive_callback(context, input.as_ptr(), 2) },
            -1
        );
        let mut session = session(None);
        let (receive, _) = session.split().unwrap();
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive_error),
            std::ptr::null_mut(),
            1,
            0,
            1,
            Arc::new(StandaloneRadioState::default()),
        )
        .unwrap();
        let context = std::ptr::from_mut(&mut worker).cast();
        assert_eq!(
            unsafe { super::receive_callback(context, input.as_ptr(), 1) },
            -1
        );
    }

    #[test]
    fn transmit_worker_rejects_each_invalid_callback_shape() {
        for (program_frames, maximum_frames, output_channels) in
            [(8, 0, 2), (4, 8, 2), (8, 8, 0), (8, 8, 3)]
        {
            assert_eq!(
                transmit_worker_result(program_frames, maximum_frames, output_channels),
                Err(super::AudioCallbackError::InvalidFrame)
            );
        }
    }

    #[test]
    fn transmit_worker_silences_invalid_spans_and_program_failures() {
        let mut program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 4).unwrap();
        let port = program.as_mut().port();
        let mut radio_session = session(Some(port));
        let (_, transmit) = radio_session.split().unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        let mut worker =
            NativeTransmitWorker::new(transmit, program.as_mut(), 4, 2, state.clone()).unwrap();
        let mut output = [1.0; 8];
        worker.process(&mut output, 4).unwrap();
        assert!(state.logical_ptt());

        assert_eq!(
            worker.process(&mut [], 0).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );
        assert!(!state.logical_ptt());
        assert_eq!(
            worker.process(&mut [1.0; 10], 5).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );
        assert_eq!(
            worker.process(&mut [1.0; 3], 2).err(),
            Some(super::AudioCallbackError::InvalidFrame)
        );

        let mut program =
            ProgramAudio::new(Some(product_transmit_error), std::ptr::null_mut(), 4).unwrap();
        let port = program.as_mut().port();
        let mut radio_session = session(Some(port));
        let (_, transmit) = radio_session.split().unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        let mut worker =
            NativeTransmitWorker::new(transmit, program.as_mut(), 4, 2, state.clone()).unwrap();
        state
            .logical_ptt
            .store(1, std::sync::atomic::Ordering::Release);
        state
            .ctcss_enabled
            .store(1, std::sync::atomic::Ordering::Release);
        let mut output = [1.0; 2];
        assert_eq!(
            worker.process(&mut output, 1).err(),
            Some(super::AudioCallbackError::Program(
                crate::program_audio::ProgramAudioError::ProductCallback
            ))
        );
        assert_eq!(output, [0.0; 2]);
        assert!(!state.logical_ptt());
        assert!(!state.ctcss_enabled());
    }

    #[test]
    fn transmit_worker_silences_core_mismatch_and_core_error() {
        for (operation, expected) in [
            (
                transmit_wrong_frame_count as unsafe extern "C" fn(_, _, _, _, _) -> i32,
                super::AudioCallbackError::InvalidFrame,
            ),
            (
                transmit_error as unsafe extern "C" fn(_, _, _, _, _) -> i32,
                super::AudioCallbackError::Radio(crate::radio_session::RadioSessionError::Process(
                    -1,
                )),
            ),
        ] {
            let mut api = session_descriptor();
            api.session_transmit = Some(operation);
            let mut program =
                ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 4).unwrap();
            let port = program.as_mut().port();
            let mut session = session_with_descriptor(api, Some(port));
            let (_, transmit) = session.split().unwrap();
            let state = Arc::new(StandaloneRadioState::default());
            let mut worker =
                NativeTransmitWorker::new(transmit, program.as_mut(), 4, 2, state.clone()).unwrap();
            state
                .logical_ptt
                .store(1, std::sync::atomic::Ordering::Release);
            state
                .ctcss_enabled
                .store(1, std::sync::atomic::Ordering::Release);
            let mut output = [1.0; 2];

            assert_eq!(worker.process(&mut output, 1).err(), Some(expected));
            assert_eq!(output, [0.0; 2]);
            assert!(!state.logical_ptt());
            assert!(!state.ctcss_enabled());
        }
    }

    #[test]
    fn transmit_callback_rejects_null_context_output_and_zero_frames() {
        let mut output = [1.0; 2];
        assert_eq!(
            unsafe { super::transmit_callback(std::ptr::null_mut(), output.as_mut_ptr(), 1) },
            -1
        );

        let mut program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 4).unwrap();
        let port = program.as_mut().port();
        let mut session = session(Some(port));
        let (_, transmit) = session.split().unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        let mut worker =
            NativeTransmitWorker::new(transmit, program.as_mut(), 4, 2, state.clone()).unwrap();
        state
            .logical_ptt
            .store(1, std::sync::atomic::Ordering::Release);
        state
            .ctcss_enabled
            .store(1, std::sync::atomic::Ordering::Release);
        let context = std::ptr::from_mut(&mut worker).cast();

        assert_eq!(
            unsafe { super::transmit_callback(context, std::ptr::null_mut(), 1) },
            -1
        );
        assert!(!state.logical_ptt());
        assert!(!state.ctcss_enabled());
        state
            .logical_ptt
            .store(1, std::sync::atomic::Ordering::Release);
        state
            .ctcss_enabled
            .store(1, std::sync::atomic::Ordering::Release);
        assert_eq!(
            unsafe { super::transmit_callback(context, output.as_mut_ptr(), 0) },
            -1
        );
        assert!(!state.logical_ptt());
        assert!(!state.ctcss_enabled());

        let mut api = session_descriptor();
        api.session_transmit = Some(transmit_error);
        let mut program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 4).unwrap();
        let port = program.as_mut().port();
        let mut session = session_with_descriptor(api, Some(port));
        let (_, transmit) = session.split().unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        let mut worker =
            NativeTransmitWorker::new(transmit, program.as_mut(), 4, 2, state.clone()).unwrap();
        let context = std::ptr::from_mut(&mut worker).cast();
        output.fill(1.0);
        assert_eq!(
            unsafe { super::transmit_callback(context, output.as_mut_ptr(), 1) },
            -1
        );
        assert_eq!(output, [0.0; 2]);
        assert!(!state.logical_ptt());
        assert!(!state.ctcss_enabled());
    }

    #[test]
    fn receive_path_processes_native_audio_then_delivers_qualified_mono_to_product() {
        let mut session = session(None);
        let (receive, _) = session.split().unwrap();
        let mut probe = ReceiveProbe {
            samples: Vec::new(),
            receiving: 0,
        };
        let state = Arc::new(StandaloneRadioState::default());
        state.set_receive_inputs(true, false, false);
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive),
            std::ptr::from_mut(&mut probe).cast(),
            8,
            1,
            2,
            state,
        )
        .unwrap();

        let input = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5];
        let context = std::ptr::from_mut(&mut worker).cast();
        assert_eq!(
            unsafe { NativeReceiveWorker::callback().unwrap()(context, input.as_ptr(), 3) },
            0
        );

        assert_eq!(probe.receiving, 1);
        assert_eq!(probe.samples, [0.1, 0.3, 0.5]);
    }

    #[test]
    fn mono_hardware_capture_is_presented_to_radio_core_as_dual_mono() {
        let mut session = session(None);
        let (receive, _) = session.split().unwrap();
        let mut probe = ReceiveProbe {
            samples: Vec::new(),
            receiving: 0,
        };
        let mut worker = NativeReceiveWorker::new(
            receive,
            Some(product_receive),
            std::ptr::from_mut(&mut probe).cast(),
            8,
            1,
            1,
            Arc::new(StandaloneRadioState::default()),
        )
        .unwrap();

        worker.process(&[0.2, -0.4], 2).unwrap();

        assert_eq!(probe.samples, [0.2, -0.4]);
    }

    #[test]
    fn transmit_path_feeds_product_program_and_signaling_through_native_tick() {
        let state = Arc::new(StandaloneRadioState::default());
        let mut pinned_program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 8).unwrap();
        let program_port = pinned_program.as_mut().port();
        let mut session = session(Some(program_port));
        let (_, transmit) = session.split().unwrap();
        let mut worker =
            NativeTransmitWorker::new(transmit, pinned_program.as_mut(), 8, 2, state.clone())
                .unwrap();
        let mut output = [0.0; 8];

        let context = std::ptr::from_mut(&mut worker).cast();
        assert_eq!(
            unsafe { NativeTransmitWorker::callback().unwrap()(context, output.as_mut_ptr(), 4) },
            0
        );

        assert_eq!(output, [0.375; 8]);
        assert!(state.logical_ptt());
        assert!(state.ctcss_enabled());
    }

    #[test]
    fn mono_hardware_playback_gets_the_downmix_of_native_stereo() {
        let mut program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 8).unwrap();
        let program_port = program.as_mut().port();
        let mut session = session(Some(program_port));
        let (_, transmit) = session.split().unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        let mut worker =
            NativeTransmitWorker::new(transmit, program.as_mut(), 8, 1, state).unwrap();
        let mut output = [0.0; 3];

        worker.process(&mut output, 3).unwrap();

        assert_eq!(output, [0.375; 3]);
    }

    #[test]
    fn transmit_worker_rejects_zero_capacity_before_startup() {
        let state = Arc::new(StandaloneRadioState::default());
        let mut pinned_program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 8).unwrap();
        let program_port = pinned_program.as_mut().port();
        let mut session = session(Some(program_port));
        let (_, transmit) = session.split().unwrap();

        assert!(matches!(
            NativeTransmitWorker::new(transmit, pinned_program.as_mut(), 0, 2, state),
            Err(super::AudioCallbackError::InvalidFrame)
        ));
    }

    #[test]
    fn transmit_worker_rejects_capacity_larger_than_the_program_buffer() {
        let state = Arc::new(StandaloneRadioState::default());
        let mut pinned_program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 4).unwrap();
        let program_port = pinned_program.as_mut().port();
        let mut session = session(Some(program_port));
        let (_, transmit) = session.split().unwrap();

        assert!(matches!(
            NativeTransmitWorker::new(transmit, pinned_program.as_mut(), 8, 2, state),
            Err(super::AudioCallbackError::InvalidFrame)
        ));
    }

    #[test]
    fn oversized_portaudio_output_is_silenced_before_the_callback_returns() {
        let state = Arc::new(StandaloneRadioState::default());
        state.set_physical_ptt(true);
        let mut pinned_program =
            ProgramAudio::new(Some(product_transmit), std::ptr::null_mut(), 4).unwrap();
        let program_port = pinned_program.as_mut().port();
        let mut session = session(Some(program_port));
        let (_, transmit) = session.split().unwrap();
        let mut worker =
            NativeTransmitWorker::new(transmit, pinned_program.as_mut(), 4, 2, state.clone())
                .unwrap();
        let mut output = [1.0; 10];
        let context = std::ptr::from_mut(&mut worker).cast();

        assert_eq!(
            unsafe { super::transmit_callback(context, output.as_mut_ptr(), 5) },
            -1
        );

        assert_eq!(output, [0.0; 10]);
        assert!(!state.logical_ptt());
        assert!(!state.ctcss_enabled());
    }
}
