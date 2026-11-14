//! Source-rate shared speech analysis; no programme storage or radio ownership.
use crate::{Error, abi};
use std::{
    ffi::CStr,
    mem::{offset_of, size_of},
    ptr::{self, NonNull},
};

type Handle = abi::rptadv_speech_analysis;
type Create = unsafe extern "C" fn(u32, u32, *mut *mut Handle) -> abi::rptadv_radio_result;
type Process = unsafe extern "C" fn(
    *mut Handle,
    *const f32,
    u32,
    u64,
    *mut abi::rptadv_speech_analysis_result,
) -> abi::rptadv_radio_result;
type Destroy = unsafe extern "C" fn(*mut Handle);

#[derive(Clone, Copy)]
struct Api {
    create: Create,
    process: Process,
    destroy: Destroy,
}
impl Api {
    unsafe fn from_descriptor(d: *const abi::rptadv_radio_descriptor) -> Result<Self, Error> {
        if d.is_null() {
            return Err(Error::UnsupportedFormat);
        }
        // SAFETY: the immutable provider descriptor promises a readable size prefix.
        let size = unsafe { d.cast::<u32>().read_unaligned() };
        let required = offset_of!(abi::rptadv_radio_descriptor, speech_analysis_destroy)
            + size_of::<Option<Destroy>>();
        if size < required as u32 {
            return Err(Error::UnsupportedFormat);
        }
        // SAFETY: the checked descriptor prefix covers each scalar and callback below.
        let version = unsafe { ptr::addr_of!((*d).abi_version).read_unaligned() };
        let capability = unsafe { ptr::addr_of!((*d).capability_name).read_unaligned() };
        if version != 5 || capability.is_null()
            // SAFETY: a nonnull provider capability is a process-lifetime C string.
            || unsafe { CStr::from_ptr(capability) }.to_bytes() != b"rptadv.radio-core"
        {
            return Err(Error::UnsupportedFormat);
        }
        // SAFETY: size validation covers the complete speech-analysis tail.
        let (create, process, reset, destroy) = unsafe {
            (
                ptr::addr_of!((*d).speech_analysis_create).read_unaligned(),
                ptr::addr_of!((*d).speech_analysis_process).read_unaligned(),
                ptr::addr_of!((*d).speech_analysis_reset).read_unaligned(),
                ptr::addr_of!((*d).speech_analysis_destroy).read_unaligned(),
            )
        };
        let _ = reset.ok_or(Error::UnsupportedFormat)?;
        Ok(Self {
            create: create.ok_or(Error::UnsupportedFormat)?,
            process: process.ok_or(Error::UnsupportedFormat)?,
            destroy: destroy.ok_or(Error::UnsupportedFormat)?,
        })
    }
}

/// One serialized released-provider classifier, prepared outside source callbacks.
/// Source discontinuity resets the provider's existing analysis history. Programme
/// retention and source-coordinate admission remain entirely in the existing ring.
pub(super) struct SpeechAnalysis {
    handle: NonNull<Handle>,
    api: Api,
}
// SAFETY: the uniquely owned handle moves with its serial source owner; no shared calls.
unsafe impl Send for SpeechAnalysis {}
impl SpeechAnalysis {
    pub(super) fn new(rate: u32, maximum: usize) -> Result<Self, Error> {
        // SAFETY: the directly linked released descriptor has immutable process lifetime.
        let api = unsafe { Api::from_descriptor(abi::rptadv_radio_descriptor()) }?;
        let maximum = u32::try_from(maximum).map_err(|_| Error::UnsupportedFormat)?;
        let mut handle = ptr::null_mut();
        // SAFETY: setup receives unique writable handle storage and bounded scalar options.
        let code = unsafe { (api.create)(rate, maximum, &mut handle) };
        if code != 0 {
            return Err(Error::UnsupportedFormat);
        }
        Ok(Self {
            handle: NonNull::new(handle).ok_or(Error::Allocation)?,
            api,
        })
    }
    /// Analyze only a newly accepted source prefix, preserving its original coordinate.
    pub(super) fn process(&mut self, start: u64, samples: &[f32]) -> Result<Option<u64>, Error> {
        let mut result: abi::rptadv_speech_analysis_result = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<abi::rptadv_speech_analysis_result>() as u32;
        let count = u32::try_from(samples.len()).map_err(|_| Error::InvalidFrame)?;
        // SAFETY: this serial owner retains its handle and disjoint input/result storage.
        let code = unsafe {
            (self.api.process)(
                self.handle.as_ptr(),
                samples.as_ptr(),
                count,
                start,
                &mut result,
            )
        };
        if code != 0 {
            return Err(Error::Translation);
        }
        Ok(
            (result.last_voice_source_sample != u64::MAX)
                .then_some(result.last_voice_source_sample),
        )
    }
}
impl Drop for SpeechAnalysis {
    fn drop(&mut self) {
        // SAFETY: the stopped source owner returns its unique provider handle exactly once.
        unsafe { (self.api.destroy)(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn released_phone_speech_analysis_keeps_source_positions_and_has_no_programme_buffer() {
        let speech: Vec<f32> = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm")
            .chunks_exact(2)
            .map(|p| f32::from(i16::from_le_bytes([p[0], p[1]])) / 32768.0)
            .collect();
        for rate in [8000, 48000] {
            let source: Vec<_> = speech.iter().step_by(48000 / rate).copied().collect();
            let mut analysis = SpeechAnalysis::new(rate as u32, 127).unwrap();
            let mut start = 1000;
            let mut last = None;
            for block in source.chunks(127) {
                let (result, allocations) =
                    crate::fixture::allocations(|| analysis.process(start, block));
                last = result.unwrap();
                assert_eq!(allocations, 0);
                start += block.len() as u64;
            }
            assert!(last.is_some_and(|last| (1000..start).contains(&last)));
            assert_eq!(analysis.process(0, &[0.0; 127]).unwrap(), None);
        }
    }
    #[test]
    fn released_phone_speech_analysis_rejects_invalid_setup_and_source_spans() {
        for (rate, frames) in [(0, 160), (8000, 0), (8000, 4097)] {
            assert!(SpeechAnalysis::new(rate, frames).is_err());
        }
        let mut analysis = SpeechAnalysis::new(8000, 160).unwrap();
        for samples in [&[][..], &[0.0; 161][..], &[f32::NAN][..]] {
            assert!(analysis.process(0, samples).is_err());
        }
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;
    #[test]
    fn released_phone_speech_descriptor_requires_the_complete_matching_tail() {
        // SAFETY: the linked current release supplies this complete immutable record.
        let original = unsafe { abi::rptadv_radio_descriptor().read() };
        // SAFETY: null is rejected before any prefix access.
        assert!(unsafe { Api::from_descriptor(ptr::null()) }.is_err());
        for case in 0..7 {
            let mut d = original;
            match case {
                0 => {
                    d.struct_size =
                        offset_of!(abi::rptadv_radio_descriptor, speech_analysis_destroy) as u32
                }
                1 => d.abi_version = 0,
                2 => d.capability_name = ptr::null(),
                3 => d.speech_analysis_create = None,
                4 => d.speech_analysis_process = None,
                5 => d.speech_analysis_reset = None,
                _ => d.speech_analysis_destroy = None,
            }
            // SAFETY: this local complete record remains live throughout prefix validation.
            assert!(unsafe { Api::from_descriptor(&d) }.is_err());
        }
    }
}
