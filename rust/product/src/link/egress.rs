//! Persistent native-to-codec conversion through the released shared adapter.
use crate::abi as ffi;
use std::{
    ffi::CStr,
    mem::offset_of,
    ptr::{self, NonNull},
};

/// Invalid capability, block size, or conversion failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressError;

/// One channel owner's persistent converter and bounded workspaces.
pub struct Egress {
    api: ffi::rptadv_samplerate_adapter_descriptor,
    converter: Option<NonNull<ffi::rptadv_samplerate_converter>>,
    rate: u32,
    maximum: usize,
    pending: Vec<f32>,
    used: usize,
    output: Vec<f32>,
    delay: usize,
    skip: usize,
    input_frames: u64,
    output_frames: u64,
    finishing: bool,
}
// SAFETY: the noncloneable owner exclusively calls the converter; transfer does not
// overlap any operation. The descriptor is process-lifetime immutable storage.
unsafe impl Send for Egress {}
impl Egress {
    /// Prepare bounded mono storage outside the audio/channel callback.
    pub fn new(rate: u32, maximum: usize) -> Result<Self, EgressError> {
        // SAFETY: the linked provider retains its immutable descriptor for process lifetime.
        unsafe { Self::from_descriptor(rate, maximum, ffi::rptadv_samplerate_adapter_descriptor()) }
    }
    // The provider retains the descriptor and callback code until the converter is dropped.
    unsafe fn from_descriptor(
        rate: u32,
        maximum: usize,
        pointer: *const ffi::rptadv_samplerate_adapter_descriptor,
    ) -> Result<Self, EgressError> {
        if !(8000..=48000).contains(&rate) || maximum == 0 || maximum > 48000 {
            return Err(EgressError);
        }
        // SAFETY: the installed shared library exports an immutable descriptor header.
        unsafe {
            let prefix = offset_of!(
                ffi::rptadv_samplerate_adapter_descriptor,
                converter_input_delay
            );
            if pointer.is_null()
                || ptr::addr_of!((*pointer).struct_size).read() < prefix as u32
                || ptr::addr_of!((*pointer).abi_version).read() != 2
            {
                return Err(EgressError);
            }
            // Ordinary conversion does not require live-position or pacing tails.
            // Copy its complete prefix without borrowing a larger Rust record.
            let mut api: ffi::rptadv_samplerate_adapter_descriptor = std::mem::zeroed();
            ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                ptr::from_mut(&mut api).cast::<u8>(),
                prefix,
            );
            if api.capability_name.is_null()
                || CStr::from_ptr(api.capability_name) != c"rptadv.samplerate"
                || api.create.is_none()
                || api.process.is_none()
                || api.destroy.is_none()
                || api.reset.is_none()
                || api.queued_input.is_none()
                || api.converter_output_delay.is_none()
            {
                return Err(EgressError);
            }
            // Native-rate egress is an exact copy, not asynchronous clock
            // recovery. Validate the provider contract, but prepare no unused
            // converter/filter history or pending-input workspace for that path.
            let mut value = Self {
                api,
                converter: None,
                rate,
                maximum,
                pending: Vec::new(),
                used: 0,
                output: vec![0.0; maximum],
                delay: 0,
                skip: 0,
                input_frames: 0,
                output_frames: 0,
                finishing: false,
            };
            if rate == 48000 {
                return Ok(value);
            }
            let mut converter = ptr::null_mut();
            if api.create.unwrap()(
                48000,
                rate,
                (maximum * 2) as u32,
                (maximum * 2 + 64) as u32,
                &mut converter,
            ) != 0
            {
                if !converter.is_null() {
                    api.destroy.unwrap()(converter);
                }
                return Err(EgressError);
            }
            let converter = NonNull::new(converter).ok_or(EgressError)?;
            value.converter = Some(converter);
            value.pending.resize(maximum * 2, 0.0);
            value.output.resize(maximum * 2 + 64, 0.0);
            let mut delay = 0;
            if api.converter_output_delay.unwrap()(converter.as_ptr(), &mut delay) != 0 {
                return Err(EgressError);
            }
            value.delay = delay as usize;
            value.skip = value.delay;
            Ok(value)
        }
    }
    /// Convert a continuing native block, retaining only unaccepted input.
    /// The adapter retains accepted FIR input even when it produces no output;
    /// it must not be submitted a second time. Empty input drains available
    /// output without flushing the continuing stream's FIR tail.
    /// The returned slice contains only real generated samples, never padded frames.
    pub fn process(&mut self, input: &[f32]) -> Result<&[f32], EgressError> {
        if self.finishing && !input.is_empty() {
            self.reset()?;
        }
        if input.len() > self.maximum
            || (self.converter.is_some() && self.used + input.len() > self.pending.len())
        {
            return Err(EgressError);
        }
        self.input_frames = self
            .input_frames
            .checked_add(input.len() as u64)
            .ok_or(EgressError)?;
        if self.rate == 48000 {
            self.output[..input.len()].copy_from_slice(input);
            self.output_frames += input.len() as u64;
            return Ok(&self.output[..input.len()]);
        }
        self.pending[self.used..self.used + input.len()].copy_from_slice(input);
        self.used += input.len();
        self.convert(usize::MAX)
    }
    fn convert(&mut self, limit: usize) -> Result<&[f32], EgressError> {
        let (mut consumed, mut generated) = (0, 0);
        // SAFETY: disjoint fixed buffers and unique converter; lengths fit u32.
        let result = unsafe {
            self.api.process.unwrap()(
                self.converter.map_or(ptr::null_mut(), NonNull::as_ptr),
                self.pending.as_ptr(),
                self.used as u32,
                self.output.as_mut_ptr(),
                self.output.len() as u32,
                f64::from(self.rate) / 48000.0,
                &mut consumed,
                &mut generated,
            )
        };
        if result != 0 || consumed as usize > self.used || generated as usize > self.output.len() {
            return Err(EgressError);
        }
        self.pending.copy_within(consumed as usize..self.used, 0);
        self.used -= consumed as usize;
        let skip = self.skip.min(generated as usize);
        self.skip -= skip;
        let count = (generated as usize - skip).min(limit);
        self.output.copy_within(skip..skip + count, 0);
        self.output_frames += count as u64;
        Ok(&self.output[..count])
    }
    /// Drain the finite source, trimming startup history and zero padding. One
    /// bounded step returns real source-rate output and whether its budget ended.
    /// Subsequent real input starts a fresh burst; no shared adapter ABI change.
    pub fn finish(&mut self) -> Result<(&[f32], bool), EgressError> {
        self.finishing = true;
        let budget = (u128::from(self.input_frames) * u128::from(self.rate)).div_ceil(48000);
        let remaining = usize::try_from(budget.saturating_sub(u128::from(self.output_frames)))
            .map_err(|_| EgressError)?;
        if remaining == 0 {
            return Ok((&self.output[..0], true));
        }
        let padding = self.maximum.min(self.pending.len() - self.used);
        self.pending[self.used..self.used + padding].fill(0.0);
        self.used += padding;
        let count = self.convert(remaining)?.len();
        Ok((&self.output[..count], count == remaining))
    }
    /// Borrow the previous process result until its encoder has consumed it.
    /// `frames` is exactly the length returned by that process call.
    pub(super) fn retained_output(&self, frames: usize) -> &[f32] {
        &self.output[..frames]
    }
    /// Discard hidden FIR history after a discontinuity; never splice old PCM into a new span.
    pub(crate) fn reset(&mut self) -> Result<(), EgressError> {
        self.used = 0;
        self.input_frames = 0;
        self.output_frames = 0;
        self.skip = self.delay;
        self.finishing = false;
        if let Some(converter) = self.converter {
            // SAFETY: this owner exclusively holds the validated converter.
            if unsafe { self.api.reset.unwrap()(converter.as_ptr()) } != 0 {
                return Err(EgressError);
            }
        }
        Ok(())
    }
}
impl Drop for Egress {
    fn drop(&mut self) {
        if let Some(converter) = self.converter {
            // SAFETY: exclusive ownership, no operation remains in flight.
            unsafe {
                self.api.destroy.unwrap()(converter.as_ptr());
            }
        }
    }
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
