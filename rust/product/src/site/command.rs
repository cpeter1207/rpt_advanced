//! Typed administrative boundary; policy and physical ownership stay on control.
use crate::{
    abi,
    lifecycle::{input, selected},
};
use rpt_advanced_core::site::{OutputRequest, SiteCommand, SiteError};
use std::{
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

/// Copy the borrowed request before scheduling it on the current control owner.
/// Acceptance is a policy decision, never confirmation of physical service.
pub(crate) unsafe extern "C" fn execute(
    local: *const std::ffi::c_char,
    local_length: usize,
    request: *const abi::rptadv_site_command_v1,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let local = unsafe { input(local, local_length) }
            .map_err(|_| SiteError::InvalidValue)?
            .to_owned();
        let command = unsafe { decode(request) }?;
        let (engine, _call) = selected().map_err(|_| SiteError::Unavailable)?;
        let clock = engine.clock();
        engine
            .run(move |host| Ok(host.site_command(&local, command, clock)))
            .map_err(|_| SiteError::Unavailable)?
    }))
    .unwrap_or(Err(SiteError::Unavailable));
    match result {
        Ok(()) => 0,
        Err(SiteError::Unavailable) => -1,
        Err(SiteError::UnknownName) => -2,
        Err(SiteError::Unsafe) => -3,
        Err(SiteError::InvalidValue) => -4,
    }
}

/// Validate the exact record before reading its tail; no caller data escapes.
unsafe fn decode(pointer: *const abi::rptadv_site_command_v1) -> Result<SiteCommand, SiteError> {
    use abi::*;
    if pointer.is_null()
        || unsafe { ptr::addr_of!((*pointer).struct_size).read() }
            != size_of::<rptadv_site_command_v1>() as u32
        || unsafe { ptr::addr_of!((*pointer).abi_version).read() }
            != RPTADV_SITE_COMMAND_ABI_VERSION
    {
        return Err(SiteError::InvalidValue);
    }
    let request = unsafe { &*pointer };
    let name = unsafe { input(request.target, request.target_length) }
        .map_err(|_| SiteError::InvalidValue)?
        .to_owned();
    let inhibit = matches!(
        request.operation,
        RPTADV_SITE_INHIBIT_ON | RPTADV_SITE_INHIBIT_OFF
    );
    let valid_value = if request.operation == RPTADV_SITE_FAN_DUTY {
        request.value.is_finite() && (0.0..=1.0).contains(&request.value)
    } else {
        request.value == 0.0
    };
    if name.is_empty() != inhibit || !valid_value {
        return Err(SiteError::InvalidValue);
    }
    Ok(match request.operation {
        RPTADV_SITE_OUTPUT_ON => SiteCommand::Output {
            name,
            request: OutputRequest::On,
        },
        RPTADV_SITE_OUTPUT_OFF => SiteCommand::Output {
            name,
            request: OutputRequest::Off,
        },
        RPTADV_SITE_OUTPUT_PULSE => SiteCommand::Output {
            name,
            request: OutputRequest::Pulse,
        },
        RPTADV_SITE_ALARM_ACKNOWLEDGE => SiteCommand::AlarmAcknowledge(name),
        RPTADV_SITE_ALARM_RESET => SiteCommand::AlarmReset(name),
        RPTADV_SITE_RESET_EXTREMA => SiteCommand::ResetExtrema(name),
        RPTADV_SITE_FAN_AUTOMATIC => SiteCommand::Fan { name, duty: None },
        RPTADV_SITE_FAN_DUTY => SiteCommand::Fan {
            name,
            duty: Some(request.value),
        },
        RPTADV_SITE_INHIBIT_ON => SiteCommand::Inhibit(true),
        RPTADV_SITE_INHIBIT_OFF => SiteCommand::Inhibit(false),
        _ => return Err(SiteError::InvalidValue),
    })
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
