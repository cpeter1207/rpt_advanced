//! Native radio-session ownership for one resolved standalone radio.

use crate::abi;
use std::{marker::PhantomData, ptr::NonNull};

type DestroyCallback = unsafe extern "C" fn(*mut abi::rptadv_radio_session);
type ReceiveCallback = unsafe extern "C" fn(
    *mut abi::rptadv_radio_session,
    *const f32,
    *mut f32,
    u32,
    *const abi::rptadv_radio_receive_input,
    *mut abi::rptadv_radio_receive_result,
) -> abi::rptadv_radio_result;
type TransmitCallback = unsafe extern "C" fn(
    *mut abi::rptadv_radio_session,
    *mut f32,
    u32,
    *const abi::rptadv_radio_transmit_input,
    *mut abi::rptadv_radio_transmit_result,
) -> abi::rptadv_radio_result;

/// A native session operation failed before audio callbacks were started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioSessionError {
    /// The radio ABI lacks a required lifecycle operation.
    IncompleteDescriptor,
    /// The radio core rejected session creation.
    Create(i32),
    /// The radio core returned success without a session handle.
    MissingHandle,
    /// The radio core could not warm its prepared DSP.
    Warm(i32),
    /// A real-time RX or TX operation is missing from the loaded ABI.
    IncompleteCallbacks,
    /// The supplied stereo frame block is empty, malformed, or too large.
    InvalidFrame,
    /// The radio core rejected one real-time operation.
    Process(i32),
}

impl std::fmt::Display for RadioSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteDescriptor => formatter.write_str("radio session ABI is incomplete"),
            Self::Create(code) => write!(formatter, "radio session creation failed ({code})"),
            Self::MissingHandle => formatter.write_str("radio session returned no handle"),
            Self::Warm(code) => write!(formatter, "radio DSP warmup failed ({code})"),
            Self::IncompleteCallbacks => {
                formatter.write_str("radio session callbacks are incomplete")
            }
            Self::InvalidFrame => {
                formatter.write_str("radio session received an invalid stereo frame")
            }
            Self::Process(code) => write!(formatter, "radio session callback failed ({code})"),
        }
    }
}

impl std::error::Error for RadioSessionError {}

/// One warmed native radio generation; borrowed processor contexts outlive this object.
pub struct NativeRadioSession<'a> {
    api: &'a abi::rptadv_radio_descriptor,
    destroy: DestroyCallback,
    handle: NonNull<abi::rptadv_radio_session>,
    maximum_receive_frames: u32,
    maximum_transmit_frames: u32,
    _ports: PhantomData<&'a abi::rptadv_radio_session_ports>,
}

// SAFETY: the released session ABI permits one serialized RX owner and one serialized TX owner
// to call concurrently. Safe access is exposed only through non-cloneable endpoints whose
// operations require `&mut self`; the session cannot be destroyed while those borrows exist.
unsafe impl Sync for NativeRadioSession<'_> {}

impl<'a> NativeRadioSession<'a> {
    /// Create and warm a session before starting the PortAudio stream.
    ///
    /// Processor contexts referenced by `ports` must stay alive until this session is dropped.
    pub fn prepare(
        api: &'a abi::rptadv_radio_descriptor,
        config: &abi::rptadv_radio_session_config,
        ports: &'a abi::rptadv_radio_session_ports,
    ) -> Result<Self, RadioSessionError> {
        let (Some(create), Some(warm), Some(destroy)) =
            (api.session_create, api.session_warm, api.session_destroy)
        else {
            return Err(RadioSessionError::IncompleteDescriptor);
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: the config and ports are valid for the synchronous create call; the borrowed
        // processor contexts remain alive for the returned session's lifetime.
        let result = unsafe { create(config, ports, &mut handle) };
        if result != 0 {
            if !handle.is_null() {
                // SAFETY: a nonnull handle returned alongside failure is still owned by this call.
                unsafe { destroy(handle) };
            }
            return Err(RadioSessionError::Create(result));
        }
        let handle = NonNull::new(handle).ok_or(RadioSessionError::MissingHandle)?;
        let session = Self {
            api,
            destroy,
            handle,
            maximum_receive_frames: config.maximum_receive_frame_count,
            maximum_transmit_frames: config.maximum_transmit_frame_count,
            _ports: PhantomData,
        };
        // SAFETY: the created session and every borrowed processor context are live.
        let result = unsafe { warm(session.handle.as_ptr()) };
        if result != 0 {
            drop(session);
            return Err(RadioSessionError::Warm(result));
        }
        Ok(session)
    }

    /// Split the warmed generation into one serial receive owner and one serial transmit owner.
    pub fn split(
        &mut self,
    ) -> Result<(ReceiveEndpoint<'_, 'a>, TransmitEndpoint<'_, 'a>), RadioSessionError> {
        let Some(receive) = self.api.session_receive else {
            return Err(RadioSessionError::IncompleteCallbacks);
        };
        let Some(transmit) = self.api.session_transmit else {
            return Err(RadioSessionError::IncompleteCallbacks);
        };
        Ok((
            ReceiveEndpoint {
                session: self,
                callback: receive,
            },
            TransmitEndpoint {
                session: self,
                callback: transmit,
            },
        ))
    }
}

/// Serial input-callback endpoint for one warmed radio session.
pub struct ReceiveEndpoint<'s, 'a> {
    session: &'s NativeRadioSession<'a>,
    callback: ReceiveCallback,
}

impl ReceiveEndpoint<'_, '_> {
    /// Process all interleaved stereo samples and return the native detector snapshot.
    pub fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        controls: &abi::rptadv_radio_receive_input,
    ) -> Result<abi::rptadv_radio_receive_result, RadioSessionError> {
        let Some(frames) = valid_frames(
            input.len(),
            output.len(),
            self.session.maximum_receive_frames,
        ) else {
            output.fill(0.0);
            return Err(RadioSessionError::InvalidFrame);
        };
        let mut result = unsafe { std::mem::zeroed::<abi::rptadv_radio_receive_result>() };
        // SAFETY: split() validates the callback exists; this endpoint serializes RX access, and
        // the session and adapter descriptor outlive its borrow. Both slices contain 2*frames.
        let code = unsafe {
            (self.callback)(
                self.session.handle.as_ptr(),
                input.as_ptr(),
                output.as_mut_ptr(),
                frames,
                controls,
                &mut result,
            )
        };
        if code == 0 {
            Ok(result)
        } else {
            output.fill(0.0);
            Err(RadioSessionError::Process(code))
        }
    }
}

/// Serial DAC-callback endpoint for one warmed radio session.
pub struct TransmitEndpoint<'s, 'a> {
    session: &'s NativeRadioSession<'a>,
    callback: TransmitCallback,
}

impl TransmitEndpoint<'_, '_> {
    /// Produce all interleaved stereo samples and return the native transmitter snapshot.
    pub fn process(
        &mut self,
        output: &mut [f32],
        controls: &abi::rptadv_radio_transmit_input,
    ) -> Result<abi::rptadv_radio_transmit_result, RadioSessionError> {
        let Some(frames) = valid_frames(
            output.len(),
            output.len(),
            self.session.maximum_transmit_frames,
        ) else {
            output.fill(0.0);
            return Err(RadioSessionError::InvalidFrame);
        };
        let mut result = unsafe { std::mem::zeroed::<abi::rptadv_radio_transmit_result>() };
        // SAFETY: split() validates the callback exists; this endpoint serializes TX access, and
        // the session and adapter descriptor outlive its borrow. Output contains 2*frames.
        let code = unsafe {
            (self.callback)(
                self.session.handle.as_ptr(),
                output.as_mut_ptr(),
                frames,
                controls,
                &mut result,
            )
        };
        if code == 0 {
            Ok(result)
        } else {
            output.fill(0.0);
            Err(RadioSessionError::Process(code))
        }
    }
}

fn valid_frames(input_samples: usize, output_samples: usize, maximum: u32) -> Option<u32> {
    if input_samples == 0 || input_samples != output_samples || input_samples % 2 != 0 {
        return None;
    }
    let frames = input_samples / 2;
    (frames <= maximum as usize).then_some(frames as u32)
}

impl Drop for NativeRadioSession<'_> {
    fn drop(&mut self) {
        // SAFETY: prepare requires destroy and the handle has one owner.
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeRadioSession, RadioSessionError};
    use crate::abi;
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<[u32; 3]> = const { Cell::new([0; 3]) };
        static WARM_RESULT: Cell<i32> = const { Cell::new(0) };
        static CREATE_RESULT: Cell<i32> = const { Cell::new(0) };
        static RETURN_HANDLE: Cell<bool> = const { Cell::new(true) };
    }

    unsafe extern "C" fn create(
        config: *const abi::rptadv_radio_session_config,
        ports: *const abi::rptadv_radio_session_ports,
        output: *mut *mut abi::rptadv_radio_session,
    ) -> abi::rptadv_radio_result {
        assert_eq!(unsafe { (*config).abi_version }, 4);
        assert_eq!(
            unsafe { (*ports).struct_size },
            std::mem::size_of::<abi::rptadv_radio_session_ports>() as u32
        );
        CALLS.with(|calls| calls.set([calls.get()[0] + 1, calls.get()[1], calls.get()[2]]));
        if RETURN_HANDLE.with(Cell::get) {
            unsafe { output.write(1_usize as *mut abi::rptadv_radio_session) };
        }
        CREATE_RESULT.with(Cell::get)
    }

    unsafe extern "C" fn warm(_: *mut abi::rptadv_radio_session) -> abi::rptadv_radio_result {
        CALLS.with(|calls| calls.set([calls.get()[0], calls.get()[1] + 1, calls.get()[2]]));
        WARM_RESULT.with(Cell::get)
    }

    unsafe extern "C" fn destroy(_: *mut abi::rptadv_radio_session) {
        CALLS.with(|calls| calls.set([calls.get()[0], calls.get()[1], calls.get()[2] + 1]));
    }

    fn descriptor() -> abi::rptadv_radio_descriptor {
        let mut descriptor: abi::rptadv_radio_descriptor = unsafe { std::mem::zeroed() };
        descriptor.session_create = Some(create);
        descriptor.session_warm = Some(warm);
        descriptor.session_receive = Some(receive);
        descriptor.session_transmit = Some(transmit);
        descriptor.session_destroy = Some(destroy);
        descriptor
    }

    unsafe extern "C" fn receive(
        _: *mut abi::rptadv_radio_session,
        input: *const f32,
        output: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_receive_input,
        result: *mut abi::rptadv_radio_receive_result,
    ) -> abi::rptadv_radio_result {
        let samples = unsafe { std::slice::from_raw_parts(input, frames as usize * 2) };
        unsafe { std::slice::from_raw_parts_mut(output, frames as usize * 2) }
            .copy_from_slice(samples);
        unsafe { (*result).frame_count = frames };
        0
    }

    unsafe extern "C" fn transmit(
        _: *mut abi::rptadv_radio_session,
        output: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_transmit_input,
        result: *mut abi::rptadv_radio_transmit_result,
    ) -> abi::rptadv_radio_result {
        unsafe { std::slice::from_raw_parts_mut(output, frames as usize * 2).fill(0.25) };
        unsafe {
            (*result).frame_count = frames;
            (*result).logical_ptt = 1;
        }
        0
    }

    unsafe extern "C" fn receive_failure(
        _: *mut abi::rptadv_radio_session,
        _: *const f32,
        output: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_receive_input,
        _: *mut abi::rptadv_radio_receive_result,
    ) -> abi::rptadv_radio_result {
        unsafe { std::slice::from_raw_parts_mut(output, frames as usize * 2).fill(1.0) };
        -2
    }

    unsafe extern "C" fn transmit_failure(
        _: *mut abi::rptadv_radio_session,
        output: *mut f32,
        frames: u32,
        _: *const abi::rptadv_radio_transmit_input,
        _: *mut abi::rptadv_radio_transmit_result,
    ) -> abi::rptadv_radio_result {
        unsafe { std::slice::from_raw_parts_mut(output, frames as usize * 2).fill(1.0) };
        -2
    }

    fn inputs() -> (
        abi::rptadv_radio_session_config,
        abi::rptadv_radio_session_ports,
    ) {
        let mut config: abi::rptadv_radio_session_config = unsafe { std::mem::zeroed() };
        config.struct_size = std::mem::size_of::<abi::rptadv_radio_session_config>() as u32;
        config.abi_version = 4;
        let mut ports: abi::rptadv_radio_session_ports = unsafe { std::mem::zeroed() };
        ports.struct_size = std::mem::size_of::<abi::rptadv_radio_session_ports>() as u32;
        (config, ports)
    }

    #[test]
    fn prepares_warms_and_destroys_a_native_session() {
        CALLS.with(|calls| calls.set([0; 3]));
        WARM_RESULT.with(|result| result.set(0));
        CREATE_RESULT.with(|result| result.set(0));
        RETURN_HANDLE.with(|present| present.set(true));
        let descriptor = descriptor();
        let (config, ports) = inputs();
        let session = NativeRadioSession::prepare(&descriptor, &config, &ports).unwrap();
        assert_eq!(CALLS.with(Cell::get), [1, 1, 0]);
        drop(session);
        assert_eq!(CALLS.with(Cell::get), [1, 1, 1]);
    }

    #[test]
    fn radio_session_errors_have_stable_user_facing_messages() {
        for (error, message) in [
            (
                RadioSessionError::IncompleteDescriptor,
                "radio session ABI is incomplete",
            ),
            (
                RadioSessionError::Create(-1),
                "radio session creation failed (-1)",
            ),
            (
                RadioSessionError::MissingHandle,
                "radio session returned no handle",
            ),
            (RadioSessionError::Warm(-2), "radio DSP warmup failed (-2)"),
            (
                RadioSessionError::IncompleteCallbacks,
                "radio session callbacks are incomplete",
            ),
            (
                RadioSessionError::InvalidFrame,
                "radio session received an invalid stereo frame",
            ),
            (
                RadioSessionError::Process(-3),
                "radio session callback failed (-3)",
            ),
        ] {
            assert_eq!(error.to_string(), message);
        }
    }

    #[test]
    fn destroys_a_session_when_warmup_fails() {
        CALLS.with(|calls| calls.set([0; 3]));
        WARM_RESULT.with(|result| result.set(-3));
        CREATE_RESULT.with(|result| result.set(0));
        RETURN_HANDLE.with(|present| present.set(true));
        let descriptor = descriptor();
        let (config, ports) = inputs();
        assert_eq!(
            NativeRadioSession::prepare(&descriptor, &config, &ports).err(),
            Some(RadioSessionError::Warm(-3))
        );
        assert_eq!(CALLS.with(Cell::get), [1, 1, 1]);
        WARM_RESULT.with(|result| result.set(0));
    }

    #[test]
    fn rejects_creation_failure_and_cleans_up_any_returned_handle() {
        CALLS.with(|calls| calls.set([0; 3]));
        CREATE_RESULT.with(|result| result.set(-2));
        RETURN_HANDLE.with(|present| present.set(true));
        let descriptor = descriptor();
        let (config, ports) = inputs();
        assert_eq!(
            NativeRadioSession::prepare(&descriptor, &config, &ports).err(),
            Some(RadioSessionError::Create(-2))
        );
        assert_eq!(CALLS.with(Cell::get), [1, 0, 1]);

        CALLS.with(|calls| calls.set([0; 3]));
        RETURN_HANDLE.with(|present| present.set(false));
        assert_eq!(
            NativeRadioSession::prepare(&descriptor, &config, &ports).err(),
            Some(RadioSessionError::Create(-2))
        );
        assert_eq!(CALLS.with(Cell::get), [1, 0, 0]);
        CREATE_RESULT.with(|result| result.set(0));
        RETURN_HANDLE.with(|present| present.set(true));
    }

    #[test]
    fn rejects_success_without_a_session_handle() {
        CALLS.with(|calls| calls.set([0; 3]));
        CREATE_RESULT.with(|result| result.set(0));
        RETURN_HANDLE.with(|present| present.set(false));
        let descriptor = descriptor();
        let (config, ports) = inputs();
        assert_eq!(
            NativeRadioSession::prepare(&descriptor, &config, &ports).err(),
            Some(RadioSessionError::MissingHandle)
        );
        assert_eq!(CALLS.with(Cell::get), [1, 0, 0]);
        RETURN_HANDLE.with(|present| present.set(true));
    }

    #[test]
    fn rejects_a_descriptor_without_session_lifecycle_functions() {
        let descriptor: abi::rptadv_radio_descriptor = unsafe { std::mem::zeroed() };
        let (config, ports) = inputs();
        assert_eq!(
            NativeRadioSession::prepare(&descriptor, &config, &ports).err(),
            Some(RadioSessionError::IncompleteDescriptor)
        );
    }

    #[test]
    fn split_rejects_each_missing_audio_callback() {
        for missing_receive in [true, false] {
            CALLS.with(|calls| calls.set([0; 3]));
            WARM_RESULT.with(|result| result.set(0));
            CREATE_RESULT.with(|result| result.set(0));
            RETURN_HANDLE.with(|present| present.set(true));
            let mut descriptor = descriptor();
            if missing_receive {
                descriptor.session_receive = None;
            } else {
                descriptor.session_transmit = None;
            }
            let (config, ports) = inputs();
            let mut session = NativeRadioSession::prepare(&descriptor, &config, &ports).unwrap();
            assert_eq!(
                session.split().err(),
                Some(RadioSessionError::IncompleteCallbacks)
            );
        }
    }

    #[test]
    fn split_endpoints_call_native_receive_and_transmit_for_the_full_frame_count() {
        CALLS.with(|calls| calls.set([0; 3]));
        WARM_RESULT.with(|result| result.set(0));
        CREATE_RESULT.with(|result| result.set(0));
        RETURN_HANDLE.with(|present| present.set(true));
        let descriptor = descriptor();
        let (mut config, ports) = inputs();
        config.maximum_receive_frame_count = 16;
        config.maximum_transmit_frame_count = 16;
        let mut session = NativeRadioSession::prepare(&descriptor, &config, &ports).unwrap();

        let (mut receive, mut transmit) = session.split().unwrap();
        let input = [0.1_f32; 12];
        let mut received = [0.0_f32; 12];
        let receive_input: abi::rptadv_radio_receive_input = unsafe { std::mem::zeroed() };
        let receive_result = receive
            .process(&input, &mut received, &receive_input)
            .unwrap();
        assert_eq!(received, input);
        assert_eq!(receive_result.frame_count, 6);

        let mut output = [0.0_f32; 12];
        let transmit_input: abi::rptadv_radio_transmit_input = unsafe { std::mem::zeroed() };
        let transmit_result = transmit.process(&mut output, &transmit_input).unwrap();
        assert_eq!(output, [0.25; 12]);
        assert_eq!(transmit_result.frame_count, 6);
        assert_eq!(transmit_result.logical_ptt, 1);
    }

    #[test]
    fn endpoint_rejects_oversized_or_malformed_frames_without_calling_native_session() {
        CALLS.with(|calls| calls.set([0; 3]));
        WARM_RESULT.with(|result| result.set(0));
        CREATE_RESULT.with(|result| result.set(0));
        RETURN_HANDLE.with(|present| present.set(true));
        let descriptor = descriptor();
        let (mut config, ports) = inputs();
        config.maximum_receive_frame_count = 2;
        config.maximum_transmit_frame_count = 2;
        let mut session = NativeRadioSession::prepare(&descriptor, &config, &ports).unwrap();
        let (mut receive, mut transmit) = session.split().unwrap();
        let receive_input: abi::rptadv_radio_receive_input = unsafe { std::mem::zeroed() };
        let mut output = [1.0_f32; 6];
        assert!(matches!(
            receive.process(&[], &mut output, &receive_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        assert!(matches!(
            receive.process(&[0.0; 4], &mut output, &receive_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        assert!(matches!(
            receive.process(&[0.0; 6], &mut output, &receive_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        assert!(matches!(
            receive.process(&[0.0; 5], &mut output, &receive_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        let transmit_input: abi::rptadv_radio_transmit_input = unsafe { std::mem::zeroed() };
        assert!(matches!(
            transmit.process(&mut output, &transmit_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        assert!(matches!(
            transmit.process(&mut output[..6], &transmit_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        assert!(matches!(
            transmit.process(&mut output[..3], &transmit_input),
            Err(RadioSessionError::InvalidFrame)
        ));
        assert_eq!(output, [0.0; 6]);
        assert_eq!(CALLS.with(Cell::get), [1, 1, 0]);
    }

    #[test]
    fn split_rejects_a_descriptor_missing_either_realtime_callback() {
        for missing_receive in [true, false] {
            let mut descriptor = descriptor();
            if missing_receive {
                descriptor.session_receive = None;
            } else {
                descriptor.session_transmit = None;
            }
            let (config, ports) = inputs();
            let mut session = NativeRadioSession::prepare(&descriptor, &config, &ports).unwrap();

            assert!(matches!(
                session.split(),
                Err(RadioSessionError::IncompleteCallbacks)
            ));
        }
    }

    #[test]
    fn native_callback_errors_silence_the_complete_output_span() {
        let mut descriptor = descriptor();
        descriptor.session_receive = Some(receive_failure);
        descriptor.session_transmit = Some(transmit_failure);
        let (mut config, ports) = inputs();
        config.maximum_receive_frame_count = 2;
        config.maximum_transmit_frame_count = 2;
        let mut session = NativeRadioSession::prepare(&descriptor, &config, &ports).unwrap();
        let (mut receive, mut transmit) = session.split().unwrap();
        let controls: abi::rptadv_radio_receive_input = unsafe { std::mem::zeroed() };
        let mut output = [0.5_f32; 4];

        assert!(matches!(
            receive.process(&[0.0; 4], &mut output, &controls),
            Err(RadioSessionError::Process(-2))
        ));
        assert_eq!(output, [0.0; 4]);
        let controls: abi::rptadv_radio_transmit_input = unsafe { std::mem::zeroed() };
        output.fill(0.5);
        assert!(matches!(
            transmit.process(&mut output, &controls),
            Err(RadioSessionError::Process(-2))
        ));
        assert_eq!(output, [0.0; 4]);
    }
}
