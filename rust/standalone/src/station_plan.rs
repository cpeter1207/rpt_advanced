//! Own the exact provider preparation while the standalone control owner stages or opens it.
use crate::{abi, providers::PhysicalProviders};
use std::{
    ffi::{CStr, c_void},
    ptr::{self, NonNull},
};

/// Immutable physical station plan owned by the loaded product provider.
pub(crate) struct StationPlan {
    pub(crate) port: [u8; 16],
    pub(crate) candidate: u64,
    pub(crate) receive_policy: (u32, u32),
    pub(crate) prepared: NonNull<c_void>,
    pub(crate) physical: abi::UrpPreparationDescriptor,
    pub(crate) providers: PhysicalProviders,
    destroy: unsafe extern "C" fn(*mut c_void),
}
// SAFETY: the product's cloned preparation is immutable. Only the serialized control owner opens a radio.
unsafe impl Send for StationPlan {}
// SAFETY: immutable borrows may be shared; release occurs after the final reference.
unsafe impl Sync for StationPlan {}

impl Drop for StationPlan {
    fn drop(&mut self) {
        // SAFETY: this is the unique provider-owned copy and the plan retains its DSO during destruction.
        unsafe { (self.destroy)(self.prepared.as_ptr()) };
    }
}

impl StationPlan {
    /// Adopt an independent clone after matching both the producer identity and the sized operations.
    pub(crate) fn prepare(
        providers: PhysicalProviders,
        request: &abi::rptadv_station_request_v1,
    ) -> Result<Self, i32> {
        let set = providers.as_ref();
        let product = set.native_product_descriptor().map_err(|_| -2)?;
        if !ptr::eq(request.product, product)
            || request.prepared.is_null()
            || request.candidate_identity == 0
        {
            return Err(-1);
        }
        // SAFETY: each exact getter is resolved within the product DSO that owns this request.
        let ownership = unsafe {
            set.native_extension::<abi::UrpStationOwnershipDescriptor>(
                b"usbradioplus_station_ownership_descriptor\0",
            )
        }
        .ok_or(-2)?;
        // SAFETY: physical1 and the original owner belong to this same retained module.
        let physical = unsafe {
            set.native_extension::<abi::UrpPreparationDescriptor>(
                b"usbradioplus_physical_preparation_descriptor\0",
            )
        }
        .ok_or(-2)?;
        // SAFETY: both checked getters return an immutable table starting with a readable u32 size.
        let (ownership, physical) = unsafe { (complete(ownership)?, complete(physical)?) };
        if ownership.abi_version != 1
            || physical.abi_version != 1
            || ownership.capability_name.is_null()
            || physical.capability_name.is_null()
        {
            return Err(-2);
        }
        // SAFETY: complete sized SDK tables promise terminated process-lifetime strings.
        if unsafe { CStr::from_ptr(ownership.capability_name) } != c"usbradioplus.station-ownership1"
            // SAFETY: the physical descriptor is checked under the same contract.
            || unsafe { CStr::from_ptr(physical.capability_name) } != c"usbradioplus.physical-preparation1"
            || physical.prepare.is_none() || physical.equal.is_none() || physical.create.is_none() || physical.prepare_update.is_none() || physical.link_prepare.is_none()
        {
            return Err(-2);
        }
        let copy = ownership.copy_prepared.ok_or(-2)?;
        let destroy = product.native_configuration_destroy.ok_or(-2)?;
        let visit = product.native_configuration_visit.ok_or(-2)?;
        let mut receive_policy: Option<(u32, u32)> = None;
        // SAFETY: the diagnostic projection is copied synchronously and never replaces the opaque physical plan.
        let status = unsafe {
            visit(
                request.prepared.cast_mut(),
                Some(inspect),
                ptr::from_mut(&mut receive_policy).cast(),
            )
        };
        if status != 0 {
            return Err(status);
        }
        let receive_policy = receive_policy.ok_or(-1)?;
        let mut owned = ptr::null_mut();
        // SAFETY: this matched product retains the original until return and success transfers a fresh copy.
        let status = unsafe { copy(product, request.prepared, &mut owned) };
        if status != 0 {
            return Err(status);
        }
        let prepared = NonNull::new(owned).ok_or(-1)?;
        Ok(Self {
            port: request.port_uuid,
            candidate: request.candidate_identity,
            receive_policy,
            prepared,
            physical,
            providers,
            destroy,
        })
    }
}

/// Accept only a complete provider table, after reading its size prefix.
unsafe fn complete<T: Copy>(pointer: *const T) -> Result<T, i32> {
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(-2);
    }
    // SAFETY: the getter provides at least its readable size word.
    if (unsafe { pointer.cast::<u32>().read() } as usize) < size_of::<T>() {
        return Err(-2);
    }
    // SAFETY: the retained DSO owns this complete immutable table.
    Ok(unsafe { pointer.read() })
}
unsafe extern "C" fn inspect(
    context: *mut c_void,
    config: *const abi::UrpNativeStationConfig,
) -> i32 {
    // SAFETY: the synchronous visitor lends the native request and its radio prefix.
    let Some(config) = (unsafe { visit_record(config) }) else {
        return -1;
    };
    if config.abi_version != abi::URP_NATIVE_CONFIG_ABI_VERSION {
        return -1;
    }
    // SAFETY: the native request lends its nested radio template through this visit.
    let Some(radio) = (unsafe { visit_record(config.radio) }) else {
        return -1;
    };
    if radio.abi_version != 5 {
        return -1;
    }
    let mode = radio.receive.vad_mode;
    // SAFETY: the closure retains a unique receive-policy output slot through this call.
    unsafe {
        *context.cast::<Option<(u32, u32)>>() = Some((mode, if mode == 0 { 0 } else { 7_680 }))
    };
    0
}

unsafe fn visit_record<T: Copy>(record: *const T) -> Option<T> {
    if record.is_null() {
        return None;
    }
    // SAFETY: the non-null visitor record starts with its readable size prefix.
    if (unsafe { record.cast::<u32>().read_unaligned() } as usize) < size_of::<T>() {
        return None;
    }
    // SAFETY: the visitor lends the complete sized record for this call.
    Some(unsafe { record.read_unaligned() })
}
