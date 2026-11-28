//! Route typed preparation, public channel requests and updates to the mounted RadioPlusAdvanced driver.
use super::{Connection, ReservedRadio, boundary, ffi};
use libloading::Library;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr::{self, NonNull},
    sync::Arc,
};

pub(super) struct Table(pub(super) ffi::rptadv_station_binding_v1);
// SAFETY: the table contains only immutable process-lifetime strings and function pointers.
unsafe impl Sync for Table {}
pub(super) static OPS: Table = Table(ffi::rptadv_station_binding_v1 {
    struct_size: size_of::<ffi::rptadv_station_binding_v1>() as u32,
    abi_version: 1,
    capability_name: c"rptadv.station-binding1".as_ptr(),
    prepare: Some(prepare),
    open_prepared: Some(open),
    stage_update: Some(stage),
    release_plan: Some(release),
});

/// The uniquely owned provider handle is released before the adapter library.
pub(super) struct Plan {
    host: usize,
    raw: NonNull<c_void>,
    ops: &'static ffi::UrpAstStationBindingDescriptor,
    _library: Library,
}
// SAFETY: immutable prepared data and Asterisk module references may cross serialized control queues.
unsafe impl Send for Plan {}
// SAFETY: the provider retains a separate per-call owner until synchronous completion.
unsafe impl Sync for Plan {}
impl Drop for Plan {
    fn drop(&mut self) {
        // SAFETY: the library and immutable callback table are retained until after release returns.
        unsafe { (self.ops.release.expect("validated"))(self.raw.as_ptr()) };
    }
}
fn prepare_plan(
    context: *mut c_void,
    request: &ffi::rptadv_station_request_v1,
) -> Result<Arc<Plan>, i32> {
    // SAFETY: the SONAME and getter form the co-packaged public adapter contract. Preparation
    // then verifies the registered technology and the exact product descriptor before copying.
    let library = unsafe { Library::new("libusbradioplus_asterisk.so.1") }.map_err(|_| -2)?;
    let pointer = unsafe {
        library
            .get::<unsafe extern "C" fn() -> *const ffi::UrpAstStationBindingDescriptor>(
                b"usbradioplus_asterisk_station_binding_descriptor\0",
            )
            .map_err(|_| -2)?()
    };
    if pointer.is_null()
        || !pointer.is_aligned()
        // SAFETY: this aligned, non-null ABI pointer supplies at least the leading size word.
        || (unsafe { pointer.cast::<u32>().read() } as usize)
            < size_of::<ffi::UrpAstStationBindingDescriptor>()
    {
        return Err(-2);
    }
    // SAFETY: the complete table was checked and this Plan retains the library.
    let ops = unsafe { &*pointer };
    if ops.abi_version != 1
        || ops.capability_name.is_null()
        || unsafe { CStr::from_ptr(ops.capability_name) } != c"usbradioplus.asterisk-station1"
        || ops.with_request.is_none()
        || ops.stage.is_none()
        || ops.finish.is_none()
        || ops.release.is_none()
    {
        return Err(-2);
    }
    let transfer = ffi::UrpBoundStationRequest {
        struct_size: size_of::<ffi::UrpBoundStationRequest>() as u32,
        abi_version: 1,
        port_uuid: request.port_uuid,
        candidate_identity: request.candidate_identity,
        product: request.product,
        prepared: request.prepared,
    };
    let mut raw = ptr::null_mut();
    // SAFETY: the matched provider pins the registered module and deep-copies the preparation before return.
    let code = unsafe { (ops.prepare.ok_or(-2)?)(&transfer, &mut raw) };
    if code != 0 {
        return Err(code);
    }
    Ok(Arc::new(Plan {
        host: context as usize,
        raw: NonNull::new(raw).ok_or(-1)?,
        ops,
        _library: library,
    }))
}
unsafe extern "C" fn prepare(
    context: *mut c_void,
    request: *const ffi::rptadv_station_request_v1,
    out: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        // SAFETY: the output is uniquely writable; non-null request supplies at least its size prefix.
        let Some(out) = (unsafe { out.as_mut() }) else {
            return -1;
        };
        *out = ptr::null_mut();
        if request.is_null()
            || !request.is_aligned()
            // SAFETY: this aligned, non-null ABI pointer supplies at least the leading size word.
            || (unsafe { request.cast::<u32>().read() } as usize)
                < size_of::<ffi::rptadv_station_request_v1>()
        {
            return -1;
        }
        // SAFETY: all request fields are readable following the prefix check.
        let request = unsafe { &*request };
        if request.abi_version != 1
            || request.candidate_identity == 0
            || request.product.is_null()
            || request.prepared.is_null()
        {
            return -1;
        }
        match prepare_plan(context, request) {
            Ok(plan) => {
                *out = Box::into_raw(Box::new(plan)).cast();
                0
            }
            Err(code) => code,
        }
    })
}
struct Open {
    maximum: usize,
    result: Option<(super::Radio, CString)>,
}
unsafe extern "C" fn request_channel(context: *mut c_void, name: *const c_char) -> i32 {
    boundary(-1, || {
        // SAFETY: with_request borrows our Open context and a terminated one-shot address.
        let Some(open) = (unsafe { context.cast::<Open>().as_mut() }) else {
            return -1;
        };
        if name.is_null() || open.result.is_some() {
            return -1;
        }
        // SAFETY: the binding callback supplies its terminated request address for this call.
        let name = unsafe { CStr::from_ptr(name) };
        match Connection::open(name).and_then(|connection| connection.into_radio(open.maximum)) {
            Ok(radio) => {
                open.result = Some((radio, name.to_owned()));
                0
            }
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn open(
    context: *mut c_void,
    raw: *const c_void,
    maximum: usize,
    out: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        // SAFETY: product retains its dormant plan and provides writable output.
        let Some(out) = (unsafe { out.as_mut() }) else {
            return -1;
        };
        *out = ptr::null_mut();
        // SAFETY: the product retains the original plan wrapper until this synchronous call returns.
        let Some(plan) = (unsafe { raw.cast::<Arc<Plan>>().as_ref() }) else {
            return -1;
        };
        if plan.host != context as usize || !(1..=4096).contains(&maximum) {
            return -1;
        }
        let mut open = Open {
            maximum,
            result: None,
        };
        // SAFETY: the callback and Open context are synchronously borrowed; the driver cancels unused addresses before return.
        let code = unsafe {
            (plan.ops.with_request.expect("validated"))(
                plan.raw.as_ptr(),
                ptr::from_mut(&mut open).cast(),
                Some(request_channel),
            )
        };
        if code != 0 {
            return code;
        }
        let Some((radio, name)) = open.result else {
            return -1;
        };
        *out = Box::into_raw(Box::new(ReservedRadio {
            radio: Some(radio),
            name,
            metadata: None,
            binding: Some(plan.clone()),
        }))
        .cast();
        0
    })
}
unsafe extern "C" fn stage(context: *mut c_void, radio: *mut c_void, raw: *const c_void) -> i32 {
    boundary(-1, || {
        // SAFETY: the radio is uniquely serialized; the plan remains owned by its caller.
        let (Some(reserved), Some(plan)) = (unsafe {
            (
                radio.cast::<ReservedRadio>().as_ref(),
                raw.cast::<Arc<Plan>>().as_ref(),
            )
        }) else {
            return -1;
        };
        let (Some(current), Some(radio)) = (&reserved.binding, &reserved.radio) else {
            return -1;
        };
        if current.host != context as usize
            || plan.host != current.host
            || !ptr::eq(plan.ops, current.ops)
        {
            return -1;
        }
        // SAFETY: the driver verifies the actual mounted channel/port and queues the typed operation on its existing owner.
        unsafe { (plan.ops.stage.expect("validated"))(radio.owner(), plan.raw.as_ptr()) }
    })
}
pub(super) unsafe extern "C" fn finish(
    context: *mut c_void,
    radio: *mut c_void,
    phase: u32,
) -> i32 {
    boundary(-1, || {
        // SAFETY: the phase dispatch borrows its unique serialized reservation.
        let Some(reserved) = (unsafe { radio.cast::<ReservedRadio>().as_ref() }) else {
            return -1;
        };
        let Some(plan) = &reserved.binding else {
            return if phase == 0 { 1 } else { -7 };
        };
        let Some(radio) = &reserved.radio else {
            return -7;
        };
        if plan.host != context as usize || !matches!(phase, 1 | 2) {
            return -1;
        }
        // SAFETY: the driver confirms its mounted original and explicit rollback before returning success.
        unsafe { (plan.ops.finish.expect("validated"))(radio.owner(), u32::from(phase == 1)) }
    })
}
unsafe extern "C" fn release(_: *mut c_void, raw: *mut c_void) {
    if !raw.is_null() {
        // SAFETY: the caller transfers the unique plan wrapper exactly once.
        drop(unsafe { Box::from_raw(raw.cast::<Arc<Plan>>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    thread_local! { static RELEASES:Cell<u32>=const{Cell::new(0)}; static STATUS:Cell<i32>=const{Cell::new(0)}; }
    unsafe extern "C" fn with_request(
        _: *const c_void,
        context: *mut c_void,
        callback: ffi::UrpStationRequestChannel,
    ) -> i32 {
        // SAFETY: the controller supplies its synchronous public ast_request callback.
        unsafe { callback.unwrap()(context, c"usb".as_ptr()) }
    }
    unsafe extern "C" fn stage(owner: *mut c_void, plan: *const c_void) -> i32 {
        assert!(!owner.is_null() && !plan.is_null());
        STATUS.with(Cell::get)
    }
    unsafe extern "C" fn finish(owner: *mut c_void, commit: u32) -> i32 {
        assert!(!owner.is_null() && commit < 2);
        STATUS.with(Cell::get)
    }
    unsafe extern "C" fn release(_: *mut c_void) {
        RELEASES.with(|count| count.set(count.get() + 1));
    }

    #[test]
    fn asterisk_services_retain_the_station_through_public_request_stage_and_safe_finish() {
        crate::fixture::reset();
        RELEASES.with(|value| value.set(0));
        let provider = Box::leak(Box::new(ffi::UrpAstStationBindingDescriptor {
            struct_size: size_of::<ffi::UrpAstStationBindingDescriptor>() as u32,
            abi_version: 1,
            capability_name: c"usbradioplus.asterisk-station1".as_ptr(),
            prepare: None,
            with_request: Some(with_request),
            stage: Some(stage),
            finish: Some(finish),
            release: Some(release),
        }));
        let first = Arc::new(Plan {
            host: 0,
            raw: NonNull::<u8>::dangling().cast(),
            ops: provider,
            _library: libloading::os::unix::Library::this().into(),
        });
        let raw = Box::into_raw(Box::new(first)).cast();
        let host = super::super::descriptor();
        let null = ptr::null_mut();
        // SAFETY: the test owns the installed service table, dormant plan and serialized Asterisk radio.
        unsafe {
            let ops = &*host.station_binding;
            let mut radio = ptr::dangling_mut();
            assert_ne!(ops.open_prepared.unwrap()(null, raw, 0, &mut radio), 0);
            assert!(radio.is_null());
            assert_eq!(ops.open_prepared.unwrap()(null, raw, 960, &mut radio), 0);
            for expected in [1, 2, 0, -7] {
                STATUS.with(|status| status.set(expected));
                assert_eq!(ops.stage_update.unwrap()(null, radio, raw), expected);
                assert_eq!(host.radio_update.unwrap()(null, radio, 2), expected);
            }
            ops.release_plan.unwrap()(null, raw);
            assert_eq!(RELEASES.with(Cell::get), 0);
            STATUS.with(|status| status.set(0));
            assert_eq!(host.radio_update.unwrap()(null, radio, 1), 0);
            let operation = ffi::UrpRemoteOperation {
                struct_size: size_of::<ffi::UrpRemoteOperation>() as u32,
                abi_version: 1,
                operation: 9,
                selector: 7,
                signed_value: -12,
                magnitude: 145_500_000,
                decimal_places: 1,
                flags: 4,
            };
            let mut ticket = std::mem::zeroed();
            assert_eq!(
                host.radio_remote_submit.unwrap()(null, radio, &operation, 3, &mut ticket),
                0
            );
            host.radio_destroy.unwrap()(null, radio);
            assert_eq!(
                RELEASES.with(Cell::get),
                0,
                "independent ticket pins the driver/module after hangup"
            );
            assert_eq!(ticket.source.unwrap()(ticket.context, 3), 0);
            assert_eq!(ticket.take.unwrap()(ticket.context, None, null), 1);
            ticket.destroy.unwrap()(ticket.context);
            assert_eq!(RELEASES.with(Cell::get), 1);
        }
    }
}
