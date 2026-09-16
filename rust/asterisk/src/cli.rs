//! Administrative input validation shared by the three public CLI registrations.
use crate::bindings as ffi;
use std::{
    ffi::{CStr, CString, c_char, c_int},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

fn output(fd: c_int, text: &str) {
    if let Ok(text) = CString::new(text) {
        unsafe {
            ffi::ast_cli(fd, c"%s".as_ptr(), text.as_ptr());
        }
    }
}
unsafe fn arguments<'a>(
    args: *mut ffi::ast_cli_args,
) -> Option<(&'a ffi::ast_cli_args, Vec<&'a str>)> {
    let args = unsafe { args.as_ref() }?;
    if args.argc < 0 || args.argc > 32 || args.argv.is_null() {
        return None;
    }
    let mut result = Vec::with_capacity(args.argc as usize);
    for pointer in unsafe { std::slice::from_raw_parts(args.argv, args.argc as usize) } {
        if pointer.is_null() {
            return None;
        }
        result.push(unsafe { CStr::from_ptr(*pointer) }.to_str().ok()?);
    }
    Some((args, result))
}
unsafe fn link_entry(
    entry: *mut ffi::ast_cli_entry,
    command: c_int,
    args: *mut ffi::ast_cli_args,
    alias: bool,
) -> *mut c_char {
    if command == ffi::CLI_INIT {
        let Some(entry) = (unsafe { entry.as_mut() }) else {
            return ptr::null_mut();
        };
        entry.command = if alias {
            c"rpt link".as_ptr().cast_mut()
        } else {
            c"rpt_advanced link".as_ptr().cast_mut()
        };
        entry.usage = if alias {
            c"Usage: rpt link {connect|monitor|local-monitor|disconnect} <local-node> <remote-node>\n       rpt link status <local-node>\n".as_ptr()
        } else {
            c"Usage: rpt_advanced link {connect|monitor|local-monitor|disconnect} <local-node> <remote-node>\n       rpt_advanced link status <local-node>\n".as_ptr()
        };
        return ptr::null_mut();
    }
    if command == ffi::CLI_GENERATE {
        return ptr::null_mut();
    }
    let Some((args, argv)) = (unsafe { arguments(args) }) else {
        return ffi::RESULT_SHOWUSAGE as *mut c_char;
    };
    let Some(request) = parse_link(&argv) else {
        return ffi::RESULT_SHOWUSAGE as *mut c_char;
    };
    let Some(product) = crate::lifecycle::product() else {
        output(
            args.fd,
            "rpt_advanced: operation failed; module is reloading or stopping\n",
        );
        return ffi::RESULT_FAILURE as *mut c_char;
    };
    let result = match request {
        LinkRequest::Status(local) => {
            struct Sink {
                text: String,
                valid: bool,
            }
            unsafe extern "C" fn sink(
                context: *mut std::ffi::c_void,
                text: *const std::ffi::c_char,
                length: usize,
            ) {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    if context.is_null() || (text.is_null() && length != 0) {
                        return false;
                    }
                    let bytes = if length == 0 {
                        &[]
                    } else {
                        unsafe { std::slice::from_raw_parts(text.cast(), length) }
                    };
                    let Ok(text) = std::str::from_utf8(bytes) else {
                        return false;
                    };
                    unsafe { &mut *context.cast::<Sink>() }.text.push_str(text);
                    true
                }));
                if !result.unwrap_or(false) && !context.is_null() {
                    unsafe { &mut *context.cast::<Sink>() }.valid = false;
                }
            }
            let mut status = Sink {
                text: String::new(),
                valid: true,
            };
            if unsafe { product.link_status(local, sink, std::ptr::from_mut(&mut status).cast()) }
                && status.valid
            {
                {
                    let text = status.text;
                    output(args.fd, &text);
                    true
                }
            } else {
                {
                    output(args.fd, &format!("rpt_advanced: unknown node {local}\n"));
                    false
                }
            }
        }
        LinkRequest::Command {
            local,
            remote,
            action,
        } => {
            let result = product.link_command(local, remote, action as u32);
            output(
                args.fd,
                &format!(
                    "rpt_advanced: link {} {}\n",
                    argv[2],
                    if result { "completed" } else { "failed" }
                ),
            );
            result
        }
    };
    if result {
        ptr::null_mut()
    } else {
        ffi::RESULT_FAILURE as *mut c_char
    }
}
pub(crate) unsafe extern "C" fn link_callback(
    entry: *mut ffi::ast_cli_entry,
    command: c_int,
    args: *mut ffi::ast_cli_args,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        link_entry(entry, command, args, false)
    }))
    .unwrap_or(ffi::RESULT_FAILURE as *mut c_char)
}
pub(crate) unsafe extern "C" fn alias_callback(
    entry: *mut ffi::ast_cli_entry,
    command: c_int,
    args: *mut ffi::ast_cli_args,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        link_entry(entry, command, args, true)
    }))
    .unwrap_or(ffi::RESULT_FAILURE as *mut c_char)
}
pub(crate) unsafe extern "C" fn command_callback(
    entry: *mut ffi::ast_cli_entry,
    command: c_int,
    args: *mut ffi::ast_cli_args,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        if command == ffi::CLI_INIT {
            if let Some(entry) = unsafe { entry.as_mut() } {
                entry.command = c"rpt_advanced command".as_ptr().cast_mut();
                entry.usage = c"Usage: rpt_advanced command <local-node> <DTMF>\n".as_ptr();
            }
            return ptr::null_mut();
        }
        if command == ffi::CLI_GENERATE {
            return ptr::null_mut();
        }
        let Some((args, argv)) = (unsafe { arguments(args) }) else {
            return ffi::RESULT_SHOWUSAGE as *mut c_char;
        };
        if argv.len() != 4 || argv[2].is_empty() || argv[3].is_empty() {
            return ffi::RESULT_SHOWUSAGE as *mut c_char;
        }
        let Some(product) = crate::lifecycle::product() else {
            output(
                args.fd,
                "rpt_advanced: operation failed; module is reloading or stopping\n",
            );
            return ffi::RESULT_FAILURE as *mut c_char;
        };
        let result = execute_digits(argv[3], |digit| {
            product.digit(argv[2], digit).map_err(|_| ())
        });
        let text = match result {
            Ok(()) => "rpt_advanced: DTMF command completed\n".into(),
            Err(DigitError::Invalid(digit)) => {
                format!("rpt_advanced: invalid DTMF digit {digit}\n")
            }
            Err(DigitError::Operation) => "rpt_advanced: DTMF command failed\n".into(),
            Err(_) => "rpt_advanced: incomplete or unknown DTMF command\n".into(),
        };
        output(args.fd, &text);
        if result.is_ok() {
            ptr::null_mut()
        } else {
            ffi::RESULT_FAILURE as *mut c_char
        }
    }))
    .unwrap_or(ffi::RESULT_FAILURE as *mut c_char)
}

/// A copied CLI operation; command meaning remains in the core.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkRequest<'a> {
    /// Print one local node's peer/topology snapshot.
    Status(&'a str),
    /// Apply one existing core link command.
    Command {
        /// Configured local node.
        local: &'a str,
        /// Requested remote identity.
        remote: &'a str,
        /// Core operation selected by the CLI spelling.
        action: LinkAction,
    },
}

/// Stable product link-action number selected by a public CLI spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LinkAction {
    /// Connect and exchange audio.
    Transceive = 1,
    /// Connect and monitor the remote.
    Monitor = 2,
    /// Connect without forwarding the remote onward.
    LocalMonitor = 3,
    /// Disconnect the selected peer.
    Disconnect = 4,
}

/// Parse exact existing CLI arity and case-sensitive operation spellings.
pub fn parse_link<'a>(arguments: &[&'a str]) -> Option<LinkRequest<'a>> {
    match arguments {
        [_, "link", "status", local] => Some(LinkRequest::Status(local)),
        [_, "link", operation, local, remote] => Some(LinkRequest::Command {
            local,
            remote,
            action: match *operation {
                "connect" => LinkAction::Transceive,
                "monitor" => LinkAction::Monitor,
                "local-monitor" => LinkAction::LocalMonitor,
                "disconnect" => LinkAction::Disconnect,
                _ => return None,
            },
        }),
        _ => None,
    }
}

/// Existing distinct CLI usage/error outcomes.
#[derive(Debug, PartialEq, Eq)]
pub enum DigitError {
    /// Missing command text.
    Usage,
    /// Invalid symbol, rejected before any collector mutation.
    Invalid(char),
    /// A completed operation failed.
    Operation,
    /// No complete known operation was produced.
    Incomplete,
}

/// Validate the complete string, then feed the same collector used by radio input.
/// The callback returns whether a command completed; execution stops on its first error.
pub fn execute_digits(
    digits: &str,
    mut feed: impl FnMut(char) -> Result<bool, ()>,
) -> Result<(), DigitError> {
    if digits.is_empty() {
        return Err(DigitError::Usage);
    }
    if let Some(invalid) = digits
        .chars()
        .find(|digit| !"0123456789ABCD*#".contains(*digit))
    {
        return Err(DigitError::Invalid(invalid));
    }
    let mut completed = false;
    for digit in digits
        .chars()
        .chain((!digits.ends_with('#')).then_some('#'))
    {
        completed |= feed(digit).map_err(|_| DigitError::Operation)?;
    }
    if completed {
        Ok(())
    } else {
        Err(DigitError::Incomplete)
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
