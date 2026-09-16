//! Persistent native-to-codec conversion through the released shared adapter.
use crate::abi as ffi;
use std::{
    ffi::CStr,
    mem::size_of,
    ptr::{self, NonNull},
};

/// Invalid capability, block size, or conversion failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressError;

/// One channel owner's persistent converter and bounded workspaces.
pub struct Egress {
    api: &'static ffi::rptadv_samplerate_adapter_descriptor,
    converter: NonNull<ffi::rptadv_samplerate_converter>,
    rate: u32,
    maximum: usize,
    pending: Vec<f32>,
    used: usize,
    output: Vec<f32>,
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
            if pointer.is_null()
                || ptr::addr_of!((*pointer).struct_size).read()
                    < size_of::<ffi::rptadv_samplerate_adapter_descriptor>() as u32
                || ptr::addr_of!((*pointer).abi_version).read() != 1
            {
                return Err(EgressError);
            }
            let api = &*pointer;
            if api.capability_name.is_null()
                || CStr::from_ptr(api.capability_name) != c"rptadv.samplerate"
                || api.create.is_none()
                || api.process.is_none()
                || api.destroy.is_none()
            {
                return Err(EgressError);
            }
            let mut converter = ptr::null_mut();
            if api.create.unwrap()(2, 1, &mut converter) != 0 {
                return Err(EgressError);
            }
            Ok(Self {
                api,
                converter: NonNull::new(converter).ok_or(EgressError)?,
                rate,
                maximum,
                pending: vec![0.0; maximum * 2],
                used: 0,
                output: vec![0.0; maximum * 2 + 64],
            })
        }
    }
    /// Convert a continuing native block, retaining any unconsumed input tail.
    /// The returned slice contains only real generated samples, never padded frames.
    pub fn process(&mut self, input: &[f32]) -> Result<&[f32], EgressError> {
        if input.len() > self.maximum || self.used + input.len() > self.pending.len() {
            return Err(EgressError);
        }
        if self.rate == 48000 {
            self.output[..input.len()].copy_from_slice(input);
            return Ok(&self.output[..input.len()]);
        }
        self.pending[self.used..self.used + input.len()].copy_from_slice(input);
        self.used += input.len();
        let (mut consumed, mut generated) = (0, 0);
        // SAFETY: disjoint fixed buffers and unique converter; lengths fit u32.
        let result = unsafe {
            self.api.process.unwrap()(
                self.converter.as_ptr(),
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
        Ok(&self.output[..generated as usize])
    }
}
impl Drop for Egress {
    fn drop(&mut self) {
        // SAFETY: exclusive ownership, no operation remains in flight.
        unsafe {
            self.api.destroy.unwrap()(self.converter.as_ptr());
        }
    }
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
