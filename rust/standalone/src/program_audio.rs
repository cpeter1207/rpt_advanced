//! Exact-frame bridge from the product transmit callback to the native radio core.

use crate::abi;
use std::{ffi::c_void, marker::PhantomPinned, pin::Pin};

type ProductTransmit = unsafe extern "C" fn(*mut c_void, *mut f32, u32, *mut u32, *mut u32) -> i32;

/// Program source rejected an invalid callback span or its product callback failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramAudioError {
    /// The product has no transmit callback or the frame count is outside the prepared bound.
    InvalidFrame,
    /// The product callback could not produce a program span.
    ProductCallback,
}

impl std::fmt::Display for ProgramAudioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFrame => formatter.write_str("invalid program audio frame"),
            Self::ProductCallback => formatter.write_str("product transmit callback failed"),
        }
    }
}

impl std::error::Error for ProgramAudioError {}

/// Preallocated exact-frame product output exposed as the radio core's program port.
pub struct ProgramAudio {
    callback: ProductTransmit,
    context: *mut c_void,
    samples: Vec<f32>,
    frames: u32,
    keyed: u32,
    ctcss_enabled: u32,
    _pinned: PhantomPinned,
}

impl ProgramAudio {
    /// Prepare one bounded bridge before audio callbacks start.
    pub fn new(
        callback: abi::rptadv_radio_transmit_v3,
        context: *mut c_void,
        maximum_frames: u32,
    ) -> Result<Pin<Box<Self>>, ProgramAudioError> {
        let callback = callback.ok_or(ProgramAudioError::InvalidFrame)?;
        if maximum_frames == 0 {
            return Err(ProgramAudioError::InvalidFrame);
        }
        Ok(Box::pin(Self {
            callback,
            context,
            samples: vec![0.0; maximum_frames as usize],
            frames: 0,
            keyed: 0,
            ctcss_enabled: 0,
            _pinned: PhantomPinned,
        }))
    }

    /// Request one exact mono block from the product, before the radio-core transmit tick.
    pub fn prepare(self: Pin<&mut Self>, frames: u32) -> Result<(u32, u32), ProgramAudioError> {
        // SAFETY: pinning protects the callback context address; the fields are not moved.
        let source = unsafe { self.get_unchecked_mut() };
        if frames == 0 || frames as usize > source.samples.len() {
            source.samples.fill(0.0);
            source.frames = 0;
            source.keyed = 0;
            source.ctcss_enabled = 0;
            return Err(ProgramAudioError::InvalidFrame);
        }
        let samples = &mut source.samples[..frames as usize];
        samples.fill(0.0);
        source.keyed = 0;
        source.ctcss_enabled = 0;
        // SAFETY: the product callback and context are retained by the caller through this
        // bridge's lifetime; the callback receives an exact writable mono span.
        let result = unsafe {
            (source.callback)(
                source.context,
                samples.as_mut_ptr(),
                frames,
                &mut source.keyed,
                &mut source.ctcss_enabled,
            )
        };
        if result != 0 {
            samples.fill(0.0);
            source.keyed = 0;
            source.ctcss_enabled = 0;
            source.frames = 0;
            return Err(ProgramAudioError::ProductCallback);
        }
        source.frames = frames;
        Ok((source.keyed, source.ctcss_enabled))
    }

    /// Borrow the exact-frame program source for the radio-core session.
    pub fn port(self: Pin<&mut Self>) -> abi::rptadv_radio_program_ring_port {
        // SAFETY: this object is pinned for the full lifetime of the returned raw context.
        let source = unsafe { self.get_unchecked_mut() };
        abi::rptadv_radio_program_ring_port {
            context: std::ptr::from_mut(source).cast(),
            render_f32: Some(render),
            warm: Some(warm),
        }
    }

    /// Return the preallocated maximum mono frame count.
    pub fn maximum_frames(self: Pin<&Self>) -> u32 {
        self.get_ref().samples.len() as u32
    }
}

unsafe extern "C" fn render(
    context: *mut c_void,
    output: *mut f32,
    frame_count: u32,
    result: *mut abi::rptadv_radio_program_ring_result,
) -> i32 {
    if output.is_null() || result.is_null() {
        return -1;
    }
    // SAFETY: radio-core ABI provides writable spans for frame_count and one result record.
    let output = unsafe { std::slice::from_raw_parts_mut(output, frame_count as usize) };
    let Some(source) = (unsafe { context.cast::<ProgramAudio>().as_ref() }) else {
        output.fill(0.0);
        return -1;
    };
    if frame_count == 0 || frame_count != source.frames {
        output.fill(0.0);
        return -1;
    }
    output.copy_from_slice(&source.samples[..frame_count as usize]);
    let mut metadata = unsafe { std::mem::zeroed::<abi::rptadv_radio_program_ring_result>() };
    metadata.ctcss_decoded_index = -1;
    unsafe { result.write(metadata) };
    0
}

unsafe extern "C" fn warm(context: *mut c_void, frame_count: u32) -> i32 {
    let Some(source) = (unsafe { context.cast::<ProgramAudio>().as_mut() }) else {
        return -1;
    };
    if frame_count == 0 || frame_count as usize > source.samples.len() {
        return -1;
    }
    source.samples[..frame_count as usize].fill(0.0);
    0
}

#[cfg(test)]
mod tests {
    use super::{ProgramAudio, ProgramAudioError};
    use crate::abi;
    use std::ffi::c_void;

    unsafe extern "C" fn product_tx(
        _: *mut c_void,
        samples: *mut f32,
        count: u32,
        keyed: *mut u32,
        ctcss: *mut u32,
    ) -> i32 {
        unsafe {
            std::slice::from_raw_parts_mut(samples, count as usize).fill(0.25);
            *keyed = 1;
            *ctcss = 1;
        }
        0
    }

    unsafe extern "C" fn failed_product_tx(
        _: *mut c_void,
        samples: *mut f32,
        count: u32,
        keyed: *mut u32,
        ctcss: *mut u32,
    ) -> i32 {
        unsafe {
            std::slice::from_raw_parts_mut(samples, count as usize).fill(1.0);
            *keyed = 1;
            *ctcss = 1;
        }
        -1
    }

    fn render(port: abi::rptadv_radio_program_ring_port, count: u32) -> (i32, Vec<f32>) {
        let mut samples = vec![0.0; count as usize];
        let mut result: abi::rptadv_radio_program_ring_result = unsafe { std::mem::zeroed() };
        let code = unsafe {
            port.render_f32.unwrap()(port.context, samples.as_mut_ptr(), count, &mut result)
        };
        (code, samples)
    }

    #[test]
    fn product_audio_and_signaling_are_available_to_the_radio_program_port() {
        let mut source = ProgramAudio::new(Some(product_tx), std::ptr::null_mut(), 8).unwrap();
        assert_eq!(source.as_mut().prepare(4), Ok((1, 1)));
        let port = source.as_mut().port();

        assert_eq!(render(port, 4), (0, vec![0.25; 4]));
        assert_eq!(render(port, 3), (-1, vec![0.0; 3]));
    }

    #[test]
    fn invalid_frames_and_product_failure_silence_the_source() {
        assert_eq!(
            ProgramAudio::new(None, std::ptr::null_mut(), 8).err(),
            Some(ProgramAudioError::InvalidFrame)
        );
        let mut source =
            ProgramAudio::new(Some(failed_product_tx), std::ptr::null_mut(), 8).unwrap();
        assert_eq!(
            source.as_mut().prepare(4),
            Err(ProgramAudioError::ProductCallback)
        );
        assert_eq!(render(source.as_mut().port(), 4), (-1, vec![0.0; 4]));
        assert_eq!(
            source.as_mut().prepare(9),
            Err(ProgramAudioError::InvalidFrame)
        );
    }

    #[test]
    fn reports_its_preallocated_frame_capacity() {
        let source = ProgramAudio::new(Some(product_tx), std::ptr::null_mut(), 8).unwrap();

        assert_eq!(source.as_ref().maximum_frames(), 8);
    }

    #[test]
    fn rejects_zero_capacity_and_zero_frames() {
        assert_eq!(
            ProgramAudio::new(Some(product_tx), std::ptr::null_mut(), 0).err(),
            Some(ProgramAudioError::InvalidFrame)
        );
        let mut source = ProgramAudio::new(Some(product_tx), std::ptr::null_mut(), 8).unwrap();

        assert_eq!(
            source.as_mut().prepare(0),
            Err(ProgramAudioError::InvalidFrame)
        );
        assert_eq!(render(source.as_mut().port(), 0), (-1, Vec::new()));
    }

    #[test]
    fn callback_ports_reject_null_contexts_and_buffers() {
        let mut source = ProgramAudio::new(Some(product_tx), std::ptr::null_mut(), 8).unwrap();
        source.as_mut().prepare(4).unwrap();
        let port = source.as_mut().port();
        let mut output = [1.0; 4];
        let mut result: abi::rptadv_radio_program_ring_result = unsafe { std::mem::zeroed() };
        let render = port.render_f32.unwrap();
        let warm = port.warm.unwrap();

        assert_eq!(
            unsafe { render(std::ptr::null_mut(), output.as_mut_ptr(), 4, &mut result) },
            -1
        );
        assert_eq!(output, [0.0; 4]);
        assert_eq!(
            unsafe { render(port.context, std::ptr::null_mut(), 4, &mut result) },
            -1
        );
        assert_eq!(
            unsafe { render(port.context, output.as_mut_ptr(), 4, std::ptr::null_mut()) },
            -1
        );
        assert_eq!(unsafe { warm(std::ptr::null_mut(), 4) }, -1);
        assert_eq!(unsafe { warm(port.context, 0) }, -1);
        assert_eq!(unsafe { warm(port.context, 9) }, -1);
        assert_eq!(unsafe { warm(port.context, 4) }, 0);
    }

    #[test]
    fn successful_port_render_sets_the_native_result_sentinel() {
        let mut source = ProgramAudio::new(Some(product_tx), std::ptr::null_mut(), 8).unwrap();
        source.as_mut().prepare(4).unwrap();
        let port = source.as_mut().port();
        let mut output = [0.0; 4];
        let mut result: abi::rptadv_radio_program_ring_result = unsafe { std::mem::zeroed() };

        assert_eq!(
            unsafe { port.render_f32.unwrap()(port.context, output.as_mut_ptr(), 4, &mut result,) },
            0
        );
        assert_eq!(output, [0.25; 4]);
        assert_eq!(result.ctcss_decoded_index, -1);
    }

    #[test]
    fn program_audio_errors_have_stable_messages() {
        assert_eq!(
            ProgramAudioError::InvalidFrame.to_string(),
            "invalid program audio frame"
        );
        assert_eq!(
            ProgramAudioError::ProductCallback.to_string(),
            "product transmit callback failed"
        );
    }
}
