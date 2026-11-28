//! Side-effect-free host station plans and their serialized open/update operations.
use super::{HostServices, Radio};
use crate::{
    Error, abi,
    configuration_radio::{PreparedStation, RadioPreparationApi},
};
use std::{
    ffi::{CStr, c_void},
    num::NonZeroU64,
    ptr::{self, NonNull},
};

/// One independent host-owned physical plan associated with the prepared product DSO.
pub(crate) struct StationBinding {
    host: HostServices,
    ops: &'static abi::rptadv_station_binding_v1,
    handle: NonNull<c_void>,
    _provider: RadioPreparationApi,
}
// SAFETY: the host retains immutable plan contents and permits control-owner migration.
unsafe impl Send for StationBinding {}
// SAFETY: this client only borrows immutable plan data; the actual radio is uniquely owned.
unsafe impl Sync for StationBinding {}

/// A staged change, an untouched handoff request, or a continuing operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StationStage {
    Staged,
    Handoff,
    Pending,
}
/// Outcome of one poll for the staged transition's commit or rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StationFinish {
    Complete,
    Pending,
}

impl HostServices {
    fn station_ops(&self) -> Result<&'static abi::rptadv_station_binding_v1, Error> {
        let raw = self.0.station_binding;
        if raw.is_null() || !raw.is_aligned() || self.0.radio_update.is_none() {
            return Err(Error::Admission);
        }
        // SAFETY: the host publishes a readable size word for its immutable process-lifetime table.
        let size = unsafe { ptr::addr_of!((*raw).struct_size).read() } as usize;
        if size < size_of::<abi::rptadv_station_binding_v1>() {
            return Err(Error::Admission);
        }
        // SAFETY: the size covers every field; host code and table remain live through product stop.
        let ops = unsafe { &*raw };
        if ops.abi_version != 1 || ops.capability_name.is_null() {
            return Err(Error::Admission);
        }
        // SAFETY: the host capability is a terminated immutable C string under the table contract.
        let name = unsafe { CStr::from_ptr(ops.capability_name) };
        if name != c"rptadv.station-binding1"
            || ops.prepare.is_none()
            || ops.open_prepared.is_none()
            || ops.stage_update.is_none()
            || ops.release_plan.is_none()
        {
            return Err(Error::Admission);
        }
        Ok(ops)
    }

    /// Synchronously bind the exact provider preparation to one hardware-free host plan.
    pub(crate) fn prepare_station_binding(
        &self,
        station: &PreparedStation,
        candidate_identity: NonZeroU64,
    ) -> Result<StationBinding, Error> {
        let ops = self.station_ops()?;
        let (request, provider) = station.binding_request(candidate_identity);
        let mut handle = ptr::null_mut();
        // SAFETY: all request contents are borrowed for this call; success transfers one plan.
        let result =
            unsafe { (ops.prepare.expect("validated"))(self.0.context, &request, &mut handle) };
        if result != 0 {
            return Err(Error::Operation);
        }
        Ok(StationBinding {
            host: *self,
            ops,
            handle: NonNull::new(handle).ok_or(Error::Operation)?,
            _provider: provider,
        })
    }
}

impl StationBinding {
    /// Open the existing host radio owner from this immutable plan after publication.
    pub(crate) fn open_radio(&self, maximum_frames: usize) -> Result<Radio, Error> {
        if maximum_frames == 0 {
            return Err(Error::Admission);
        }
        let mut handle = ptr::null_mut();
        // SAFETY: the live plan is borrowed synchronously; the host independently retains its radio data.
        let code = unsafe {
            (self.ops.open_prepared.expect("validated"))(
                self.host.0.context,
                self.handle.as_ptr(),
                maximum_frames,
                &mut handle,
            )
        };
        if code != 0 {
            return Err(Error::Operation);
        }
        Ok(Radio {
            services: self.host,
            maximum_frames,
            handle: NonNull::new(handle).ok_or(Error::Operation)?,
        })
    }
}
impl Drop for StationBinding {
    fn drop(&mut self) {
        // SAFETY: the plan is unique to this owner, with its host and matched DSO still retained.
        unsafe {
            (self.ops.release_plan.expect("validated"))(self.host.0.context, self.handle.as_ptr())
        };
    }
}

impl Radio {
    /// Stage or poll a typed edit on this radio's own host.
    pub(crate) fn stage_station_binding(
        &mut self,
        plan: &StationBinding,
    ) -> Result<StationStage, i32> {
        if self.services.0.context != plan.host.0.context
            || self.services.0.station_binding != plan.host.0.station_binding
        {
            return Err(-1);
        }
        // SAFETY: both handles belong to this host and the unique radio owner serializes changes.
        match unsafe {
            (plan.ops.stage_update.expect("validated"))(
                self.services.0.context,
                self.handle.as_ptr(),
                plan.handle.as_ptr(),
            )
        } {
            0 => Ok(StationStage::Staged),
            1 => Ok(StationStage::Handoff),
            2 => Ok(StationStage::Pending),
            code => Err(code),
        }
    }
    /// Poll commitment or reversal without acknowledging an unknown disposition.
    pub(crate) fn finish_station_binding(&mut self, commit: bool) -> Result<StationFinish, i32> {
        let finish = self.services.0.radio_update.ok_or(-7)?;
        // SAFETY: the unique radio remains alive across pending finish attempts.
        match unsafe {
            finish(
                self.services.0.context,
                self.handle.as_ptr(),
                if commit { 1 } else { 2 },
            )
        } {
            0 => Ok(StationFinish::Complete),
            2 => Ok(StationFinish::Pending),
            code => Err(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn prepare(
        _: *mut c_void,
        _: *const abi::rptadv_station_request_v1,
        _: *mut *mut c_void,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn open(
        _: *mut c_void,
        _: *const c_void,
        _: usize,
        _: *mut *mut c_void,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn stage(_: *mut c_void, _: *mut c_void, _: *const c_void) -> i32 {
        -1
    }
    unsafe extern "C" fn release(_: *mut c_void, _: *mut c_void) {}
    unsafe extern "C" fn finish(_: *mut c_void, _: *mut c_void, _: u32) -> i32 {
        -1
    }
    #[test]
    fn validates_every_station_table_boundary_before_prepare() {
        // SAFETY: the complete static fixture table remains installed through all synchronous admissions.
        let original = unsafe { *crate::fixture::host_descriptor() };
        for failure in 0..10 {
            let mut ops = abi::rptadv_station_binding_v1 {
                struct_size: size_of::<abi::rptadv_station_binding_v1>() as u32,
                abi_version: 1,
                capability_name: c"rptadv.station-binding1".as_ptr(),
                prepare: Some(prepare),
                open_prepared: Some(open),
                stage_update: Some(stage),
                release_plan: Some(release),
            };
            let mut table = original;
            table.radio_update = Some(finish);
            match failure {
                0 => ops.struct_size -= 1,
                1 => ops.abi_version += 1,
                2 => ops.capability_name = ptr::null(),
                3 => ops.capability_name = c"other.station-binding1".as_ptr(),
                4 => ops.prepare = None,
                5 => ops.open_prepared = None,
                6 => ops.stage_update = None,
                7 => ops.release_plan = None,
                8 => table.radio_update = None,
                _ => {}
            }
            table.station_binding = Box::leak(Box::new(ops));
            // SAFETY: table and all referenced pointers are installed for the duration required by HostServices.
            let host = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
            assert_eq!(
                host.station_ops().is_ok(),
                failure == 9,
                "station table case {failure}"
            );
        }
        let mut previous = original;
        previous.abi_version = 10;
        previous.capability = *b"rptadv.hst10\0\0\0\0";
        previous.struct_size = 400;
        assert!(matches!(
            unsafe { HostServices::open(&previous) },
            Err(Error::Admission)
        ));
        let mut absent_binding = original;
        absent_binding.station_binding = ptr::null();
        let host = unsafe { HostServices::open(&absent_binding) }.unwrap();
        assert!(host.station_ops().is_err());
    }
}
