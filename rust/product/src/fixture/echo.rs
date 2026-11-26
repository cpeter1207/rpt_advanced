//! Host callbacks for Product tests that admit a managed direct Echo account.
use crate::abi;
use std::ffi::c_void;
unsafe extern "C" fn begin(_: *mut c_void, _: u64, attempt: u64) -> i32 {
    i32::from(attempt == 0)
}
unsafe extern "C" fn row(
    _: *mut c_void,
    _: u64,
    row: *const abi::rptadv_managed_echo_row_v1,
) -> i32 {
    let row = unsafe { &*row };
    i32::from(
        row.account_uuid == [0; 16] || row.directory_password.length == 0 || row.settings.is_null(),
    )
}
unsafe extern "C" fn step(_: *mut c_void, _: u64, phase: u32) -> i32 {
    i32::from(!(1..=4).contains(&phase))
}
unsafe extern "C" fn shutdown(_: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn observe(
    _: *mut c_void,
    _: *const u8,
    _: *const u8,
    _: u64,
    _: *mut abi::rptadv_managed_echo_status_v1,
) -> i32 {
    -1
}
pub(crate) static PROVIDER: abi::rptadv_managed_echo_provider_v2 =
    abi::rptadv_managed_echo_provider_v2 {
        struct_size: size_of::<abi::rptadv_managed_echo_provider_v2>() as u32,
        abi_version: 2,
        begin: Some(begin),
        row: Some(row),
        step: Some(step),
        shutdown: Some(shutdown),
        observe: Some(observe),
    };
