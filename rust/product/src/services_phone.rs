//! Checked, process-lifetime private account provider. Ordinary hosts can omit this group.
use super::{Error, HostServices, abi};
use std::mem::size_of;

impl HostServices {
    pub(crate) fn echo_provider(&self) -> Result<&abi::rptadv_managed_echo_provider_v1, Error> {
        let p = self.0.managed_echo_provider;
        if p.is_null() || !p.is_aligned() { return Err(Error::Admission); }
        let [extent, version] = unsafe { p.cast::<[u32; 2]>().read() };
        if version != 1 || (extent as usize) < size_of::<abi::rptadv_managed_echo_provider_v1>() {
            return Err(Error::Admission);
        }
        let ops = unsafe { &*p };
        if ops.begin.is_none() || ops.row.is_none() || ops.step.is_none() || ops.shutdown.is_none()
            || !self.peer_step_available() || self.0.peer_prepare_audio.is_none() {
            return Err(Error::Admission);
        }
        Ok(ops)
    }

    /// Read the ordinary IAX group only for an explicit selected authority.
    pub(crate) fn asl_provider(&self) -> Result<&abi::rptadv_asl_provider_v1, Error> {
        let pointer = self.0.asl_provider;
        if pointer.is_null() || !pointer.is_aligned() { return Err(Error::Admission); }
        let [extent, version] = unsafe { pointer.cast::<[u32; 2]>().read() };
        if version != 1 || (extent as usize) < size_of::<abi::rptadv_asl_provider_v1>() {
            return Err(Error::Admission);
        }
        let group = unsafe { &*pointer };
        if group.prepare.is_none() || group.step.is_none() || group.observe.is_none() || group.originate.is_none()
            || !self.peer_step_available() || self.0.peer_prepare_audio.is_none() {
            return Err(Error::Admission);
        }
        Ok(group)
    }
    pub(crate) fn context(&self) -> *mut std::ffi::c_void {
        self.0.context
    }

    /// Validate the complete lower transport contract before the first account
    /// prepare, or before accepting a peer that advertises private source media.
    pub(crate) fn phone_provider(&self) -> Result<&abi::rptadv_phone_provider_v1, Error> {
        let pointer = self.0.phone_provider;
        if pointer.is_null() {
            return Err(Error::Admission);
        }
        // The immutable host table owns this nested pointer through final stop.
        // Check the fixed header before forming a reference to the full record.
        let [extent, version] = unsafe { pointer.cast::<[u32; 2]>().read_unaligned() };
        if version != 1 || (extent as usize) < size_of::<abi::rptadv_phone_provider_v1>() {
            return Err(Error::Admission);
        }
        let group = unsafe { &*pointer };
        if group.prepare.is_none()
            || group.apply.is_none()
            || group.restore.is_none()
            || group.release.is_none()
            || group.observe.is_none()
            || group.originate.is_none()
            || group.send_prepared_timed.is_none()
            || group.media_clock_reader.is_none()
            || self.0.peer_read_source.is_none()
            || !self.peer_step_available()
            || self.0.peer_prepare_audio.is_none()
            || self.0.peer_reset_encoder.is_none()
            || self.0.peer_elapsed_ms.is_none()
        {
            return Err(Error::Admission);
        }
        Ok(group)
    }
}
