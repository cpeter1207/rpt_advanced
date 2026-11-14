//! Typed trusted-host telephone command; callee media never reaches this entry.
use super::{abi, input, selected};
use rpt_advanced_core::{config::NodeId, link::PhoneAccountId};
use std::{
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

pub(super) unsafe extern "C" fn execute(
    local: *const std::ffi::c_char,
    local_length: usize,
    request: *const abi::rptadv_phone_command_v1,
    output: *mut u64,
) -> i32 {
    if output.is_null() {
        return -2;
    }
    unsafe { output.write(0) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if local_length == 0
            || local_length > 63
            || request.is_null()
            || unsafe { ptr::addr_of!((*request).struct_size).read() }
                < size_of::<abi::rptadv_phone_command_v1>() as u32
        {
            return Err(-2);
        }
        let request = unsafe { &*request };
        if request.abi_version != abi::RPTADV_PHONE_COMMAND_ABI_VERSION {
            return Err(-2);
        }
        let local = unsafe { input(local, local_length) }
            .map_err(|_| -2)?
            .to_owned();
        NodeId::new(&local).map_err(|_| -2)?;
        match request.operation {
            abi::RPTADV_PHONE_DIAL => {
                if request.call_id != 0
                    || request.account_length == 0
                    || request.account_length > 64
                    || request.destination_length == 0
                    || request.destination_length > 255
                {
                    return Err(-2);
                }
                let account = PhoneAccountId::new(
                    unsafe { input(request.account, request.account_length) }.map_err(|_| -2)?,
                )
                .map_err(|_| -2)?;
                let destination = unsafe { input(request.destination, request.destination_length) }
                    .map_err(|_| -2)?
                    .to_owned();
                let (engine, _call) = selected().map_err(|_| -1)?;
                engine
                    .start_phone(local, account, destination)
                    .map_err(|_| -1)
            }
            abi::RPTADV_PHONE_HANGUP => {
                if request.call_id == 0
                    || request.account_length != 0
                    || request.destination_length != 0
                {
                    return Err(-2);
                }
                let (engine, _call) = selected().map_err(|_| -1)?;
                engine
                    .hangup_phone(local, request.call_id)
                    .map_err(|_| -1)?;
                Ok(request.call_id)
            }
            _ => Err(-2),
        }
    }))
    .unwrap_or(Err(-1));
    match result {
        Ok(id) => {
            unsafe { output.write(id) };
            0
        }
        Err(code) => code,
    }
}
