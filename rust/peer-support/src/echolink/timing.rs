//! Optional complete timing pair following the v1 observation and egress pairs.
use super::{ApiError, abi, host::{Account, Error}};
use std::{mem::size_of, num::NonZeroU64, ptr, sync::Arc};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JitterTiming {
    pub minimum_ms: u32,
    pub initial_ms: u32,
    pub maximum_ms: u32,
}

#[repr(C)]
#[derive(Default)]
struct Receipt {
    size: u32, version: u32, attempt: u64, generation: u64, outcome: u32, reserved: u32,
}
type Stage = unsafe extern "C" fn(*mut abi::Account, *const JitterTiming) -> abi::Status;
type Swap = unsafe extern "C" fn(*mut abi::Account, *mut abi::Account, u64, *mut Receipt) -> abi::Status;
#[repr(C)]
struct Extended {
    legacy: abi::Descriptor,
    retire_egress: Option<unsafe extern "C" fn(*mut abi::Qso, *mut u64) -> abi::Status>,
    egress_observer: Option<unsafe extern "C" fn(*const abi::Qso, *mut std::ffi::c_void) -> abi::Status>,
    stage: Option<Stage>, exchange: Option<Swap>,
}
#[derive(Clone, Copy)]
pub(super) struct Operations { stage: Stage, exchange: Swap }
impl Operations {
    pub(super) unsafe fn open(p: *const abi::Descriptor) -> Result<Option<Self>, ApiError> {
        if (unsafe { ptr::addr_of!((*p).struct_size).read() } as usize) < size_of::<Extended>() {
            return Ok(None);
        }
        let p = p.cast::<Extended>();
        match unsafe { (ptr::addr_of!((*p).stage).read(), ptr::addr_of!((*p).exchange).read()) } {
            (Some(stage), Some(exchange)) => Ok(Some(Self { stage, exchange })),
            (None, None) => Ok(None),
            _ => Err(ApiError),
        }
    }
}

/// Outcome of an exact prepared exchange on a live direct account.
pub enum Exchange { Pending, Applied, Refused, Uncertain }
impl Account {
    /// Stage on a never-activated direct candidate. The provider copies the tuple.
    pub fn stage_jitter(&self, timing: JitterTiming) -> Result<(), Error> {
        let ops = self.provider.api.1.ok_or(Error::Contract)?;
        super::host::completed(unsafe { (ops.stage)(self.handle.as_ptr(), &timing) })
    }

    /// Begin or poll the exact target/candidate pair. A refused attempt has no live effect.
    pub fn exchange(&self, other: &Self, id: NonZeroU64, generation: NonZeroU64) -> Exchange {
        let Some(ops) = self.provider.api.1 else { return Exchange::Refused };
        if !Arc::ptr_eq(&self.provider, &other.provider) || self.local != other.local ||
            ptr::eq(self, other) { return Exchange::Refused; }
        let mut r = Receipt { size: size_of::<Receipt>() as u32, version: 1, ..Default::default() };
        let status = unsafe { (ops.exchange)(self.handle.as_ptr(), other.handle.as_ptr(), id.get(), &mut r) };
        if r.size != size_of::<Receipt>() as u32 || r.version != 1 || r.reserved != 0 {
            return Exchange::Uncertain;
        }
        if r.attempt == 0 && r.generation == 0 && r.outcome == 0 {
            return match status {
                abi::Status::Full => Exchange::Pending,
                abi::Status::Invalid | abi::Status::Unsupported | abi::Status::Closed => Exchange::Refused,
                _ => Exchange::Uncertain,
            };
        }
        if r.attempt != id.get() || r.generation != generation.get() { return Exchange::Uncertain; }
        match (status, r.outcome) {
            (abi::Status::Pending, 1) => Exchange::Pending,
            (abi::Status::Ok, 2) => Exchange::Applied,
            (abi::Status::Failed, 3) => Exchange::Refused,
            _ => Exchange::Uncertain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn stage(_: *mut abi::Account, _: *const JitterTiming) -> abi::Status { abi::Status::Ok }
    unsafe extern "C" fn swap(_: *mut abi::Account, _: *mut abi::Account, id: u64, out: *mut Receipt) -> abi::Status {
        unsafe { (*out).attempt = id; }
        abi::Status::Pending
    }
    #[test]
    fn complete_timing_pair_starts_after_both_egress_slots() {
        let mut full: Extended = unsafe { std::mem::zeroed() };
        full.stage = Some(stage);
        full.exchange = Some(swap);
        let prefix = size_of::<abi::Descriptor>();
        let slot = size_of::<Option<Stage>>();
        assert_eq!(std::mem::offset_of!(Extended, stage), prefix + 2 * slot);
        assert_eq!(std::mem::offset_of!(Extended, exchange), prefix + 3 * slot);
        let end = size_of::<Extended>();
        for available in prefix..=end {
            full.legacy.struct_size = available as u32;
            assert_eq!(unsafe { Operations::open(&full.legacy) }.unwrap().is_some(), available == end);
        }
        let ops = unsafe { Operations::open(&full.legacy) }.unwrap().unwrap();
        let mut receipt = Receipt::default();
        assert_eq!(unsafe { (ops.stage)(ptr::null_mut(), ptr::null()) }, abi::Status::Ok);
        assert_eq!(unsafe { (ops.exchange)(ptr::null_mut(), ptr::null_mut(), 19, &mut receipt) }, abi::Status::Pending);
        assert_eq!(receipt.attempt, 19);
        full.stage = None;
        assert!(unsafe { Operations::open(&full.legacy) }.is_err());
        full.stage = Some(stage);
        full.exchange = None;
        assert!(unsafe { Operations::open(&full.legacy) }.is_err());
    }
}
