//! Asterisk-only syntax for already-authorized shared site operations.
use super::*;

pub(crate) unsafe extern "C" fn callback(
    entry: *mut ffi::ast_cli_entry,
    command: c_int,
    args: *mut ffi::ast_cli_args,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        if command == ffi::CLI_INIT {
            if let Some(entry) = unsafe { entry.as_mut() } {
                entry.command = c"rpt_advanced site".as_ptr().cast_mut();
                entry.usage = c"Usage: rpt_advanced site output <node> <name> {on|off|pulse}\n       rpt_advanced site alarm <node> <name> {acknowledge|reset}\n       rpt_advanced site sensor <node> <name> reset-extrema\n       rpt_advanced site fan <node> <name> {automatic|duty-0-to-1}\n       rpt_advanced site inhibit <node> {on|off}\n".as_ptr();
            }
            return ptr::null_mut();
        }
        if command == ffi::CLI_GENERATE {
            return ptr::null_mut();
        }
        let Some((args, argv)) = (unsafe { arguments(args) }) else {
            return ffi::RESULT_SHOWUSAGE as *mut c_char;
        };
        let Some((local, operation, target, value)) = parse(&argv) else {
            return ffi::RESULT_SHOWUSAGE as *mut c_char;
        };
        let result = crate::lifecycle::product()
            .map_or(-1, |product| product.site_command(local, operation, target, value));
        let text = match result {
            0 => "request accepted; hardware status is reported separately",
            -2 => "unknown node or site target",
            -3 => "request rejected by safety policy",
            -4 => "invalid site request",
            _ => "operation unavailable; check target status or retry after reload",
        };
        output(args.fd, &format!("rpt_advanced: {text}\n"));
        if result == 0 { ptr::null_mut() } else { ffi::RESULT_FAILURE as *mut c_char }
    })).unwrap_or(ffi::RESULT_FAILURE as *mut c_char)
}

/// Translate exact CLI syntax only; shared policy validates target and safety.
fn parse<'a>(argv: &[&'a str]) -> Option<(&'a str, u32, &'a str, f64)> {
    use ffi::*;
    let (local, operation, target, value) = match *argv {
        [_, _, "output", local, target, "on"] => (local, RPTADV_SITE_OUTPUT_ON, target, 0.0),
        [_, _, "output", local, target, "off"] => (local, RPTADV_SITE_OUTPUT_OFF, target, 0.0),
        [_, _, "output", local, target, "pulse"] => (local, RPTADV_SITE_OUTPUT_PULSE, target, 0.0),
        [_, _, "alarm", local, target, "acknowledge"] => {
            (local, RPTADV_SITE_ALARM_ACKNOWLEDGE, target, 0.0)
        }
        [_, _, "alarm", local, target, "reset"] => (local, RPTADV_SITE_ALARM_RESET, target, 0.0),
        [_, _, "sensor", local, target, "reset-extrema"] => {
            (local, RPTADV_SITE_RESET_EXTREMA, target, 0.0)
        }
        [_, _, "fan", local, target, "automatic"] => {
            (local, RPTADV_SITE_FAN_AUTOMATIC, target, 0.0)
        }
        [_, _, "fan", local, target, duty] => {
            let duty: f64 = duty.parse().ok()?;
            if !duty.is_finite() || !(0.0..=1.0).contains(&duty) {
                return None;
            }
            (local, RPTADV_SITE_FAN_DUTY, target, duty)
        }
        [_, _, "inhibit", local, "on"] => (local, RPTADV_SITE_INHIBIT_ON, "", 0.0),
        [_, _, "inhibit", local, "off"] => (local, RPTADV_SITE_INHIBIT_OFF, "", 0.0),
        _ => return None,
    };
    (!local.is_empty() && (argv.len() == 5 || !target.is_empty()))
        .then_some((local, operation, target, value))
}
