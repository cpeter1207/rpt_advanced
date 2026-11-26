//! Native source-rate speech classification on the private call's serialized media owner.
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
type Reset = unsafe extern "C" fn(*mut Handle) -> abi::rptadv_radio_result;
type Destroy = unsafe extern "C" fn(*mut Handle);

#[derive(Clone, Copy)]
struct Api {
    create: Create,
    process: Process,
    reset: Reset,
    destroy: Destroy,
}
impl Api {
    unsafe fn open(raw: *const abi::rptadv_radio_descriptor) -> Result<Self, Error> {
        if raw.is_null()
            || unsafe { ptr::addr_of!((*raw).struct_size).read_unaligned() }
                < (offset_of!(abi::rptadv_radio_descriptor, speech_analysis_destroy)
                    + size_of::<Option<Destroy>>()) as u32
        {
            return Err(Error::UnsupportedFormat);
        }
        // Read only the checked prefix. A future descriptor can contain further optional entries.
        let version = unsafe { ptr::addr_of!((*raw).abi_version).read_unaligned() };
        let code = unsafe { ptr::addr_of!((*raw).capability_name).read_unaligned() };
        if version != 5
            || code.is_null()
            || unsafe { CStr::from_ptr(code) }.to_bytes() != b"rptadv.radio-core"
        {
            return Err(Error::UnsupportedFormat);
        }
        Ok(Self {
            create: unsafe { ptr::addr_of!((*raw).speech_analysis_create).read_unaligned() }
                .ok_or(Error::UnsupportedFormat)?,
            process: unsafe { ptr::addr_of!((*raw).speech_analysis_process).read_unaligned() }
                .ok_or(Error::UnsupportedFormat)?,
            reset: unsafe { ptr::addr_of!((*raw).speech_analysis_reset).read_unaligned() }
                .ok_or(Error::UnsupportedFormat)?,
            destroy: unsafe { ptr::addr_of!((*raw).speech_analysis_destroy).read_unaligned() }
                .ok_or(Error::UnsupportedFormat)?,
        })
    }
}
pub(super) struct SpeechAnalysis {
    api: Api,
    handle: NonNull<Handle>,
    rate: u32,
    // The classifier can report a source position from an earlier or partial
    // frame. Retain actual credited births for two maximum-sized source turns.
    births: Box<[Option<std::num::NonZeroU64>]>,
    first: u64,
    end: u64,
}
// One serialized source conductor owns all classifier calls; it can migrate with the peer.
unsafe impl Send for SpeechAnalysis {}
impl SpeechAnalysis {
    pub(super) fn new(rate: u32, maximum: u32) -> Result<Self, Error> {
        let api = unsafe { Api::open(abi::rptadv_radio_descriptor()) }?;
        let mut handle = ptr::null_mut();
        if unsafe { (api.create)(rate, maximum, &mut handle) } != 0 {
            return Err(Error::UnsupportedFormat);
        }
        Ok(Self {
            api,
            handle: NonNull::new(handle).ok_or(Error::Allocation)?,
            rate,
            births: vec![None; 2 * maximum as usize].into_boxed_slice(),
            first: 0,
            end: 0,
        })
    }
    pub(super) fn process(&mut self, start: u64, pcm: &[f32]) -> Result<Option<u64>, Error> {
        let count = u32::try_from(pcm.len()).map_err(|_| Error::InvalidFrame)?;
        let mut observed: abi::rptadv_speech_analysis_result = unsafe { std::mem::zeroed() };
        observed.struct_size = size_of::<abi::rptadv_speech_analysis_result>() as u32;
        if unsafe {
            (self.api.process)(
                self.handle.as_ptr(),
                pcm.as_ptr(),
                count,
                start,
                &mut observed,
            )
        } != 0
        {
            return Err(Error::Translation);
        }
        Ok((observed.last_voice_source_sample != u64::MAX)
            .then_some(observed.last_voice_source_sample))
    }
    pub(super) fn revoke(&mut self) {
        self.first = self.end;
    }

    /// Analyze the original PCM. Only a real, admitted source interval containing
    /// signal can supply the birth time of a positive observation for RF.
    pub(super) fn admitted_voice(
        &mut self,
        start: u64,
        pcm: &[f32],
        born: Option<u64>,
    ) -> Result<Option<u64>, Error> {
        let last = self.process(start, pcm)?;
        let end = start
            .checked_add(pcm.len() as u64)
            .ok_or(Error::InvalidFrame)?;
        if start != self.end {
            self.first = start;
        }
        let capacity = self.births.len() as u64;
        self.first = self.first.max(end.saturating_sub(capacity));
        self.end = end;
        let admitted = born.filter(|_| pcm.iter().any(|sample| *sample != 0.0));
        for (offset, _) in pcm.iter().enumerate() {
            let position = start + offset as u64;
            let at = admitted.and_then(|base| {
                base.checked_add(offset as u64 * 1_000_000_000 / u64::from(self.rate))
            });
            self.births[(position % capacity) as usize] = at
                .and_then(|ns| ns.checked_add(1))
                .and_then(std::num::NonZeroU64::new);
        }
        Ok(last
            .filter(|p| (self.first..self.end).contains(p))
            .and_then(|p| self.births[(p % capacity) as usize].map(|ns| ns.get() - 1)))
    }
    pub(super) fn reset(&mut self) -> Result<(), Error> {
        self.revoke();
        if unsafe { (self.api.reset)(self.handle.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(Error::Translation)
        }
    }
}
impl Drop for SpeechAnalysis {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.handle.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_native_classifier_owns_source_positions_and_resets_only_its_analysis() {
        assert!(unsafe { Api::open(ptr::null()) }.is_err());
        let mut speech = SpeechAnalysis::new(8000, 160).unwrap();
        let mut at = 1200;
        for _ in 0..8 {
            assert_eq!(speech.process(at, &[0.; 160]).unwrap(), None);
            at += 160;
        }
        assert!(speech.process(at, &[0.; 161]).is_err());
        speech.reset().unwrap();
        assert_eq!(speech.process(0, &[0.; 160]).unwrap(), None);
    }
}
