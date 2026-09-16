//! Public-Asterisk implementation of the portable product host-services table.

use crate::{
    bindings as ffi,
    connection::Connection,
    link::{
        directory::{AsteriskDirectory, DirectoryResolver, Method},
        peer_io::{Input, PeerIo},
    },
    radio::Radio,
};
use std::{
    ffi::{CString, c_char, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

unsafe fn text<'a>(pointer: *const c_char, length: usize) -> Option<&'a str> {
    if pointer.is_null() && length != 0 {
        return None;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer.cast(), length) }
    };
    std::str::from_utf8(bytes).ok()
}

fn boundary<T>(fallback: T, operation: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(fallback)
}

unsafe extern "C" fn local_time(
    _context: *mut c_void,
    seconds: i64,
    result: *mut ffi::rptadv_local_time_v1,
) -> i32 {
    boundary(-1, || {
        let Some(result) = (unsafe { result.as_mut() }) else {
            return -1;
        };
        if result.struct_size < size_of::<ffi::rptadv_local_time_v1>() as u32 {
            return -1;
        }
        let seconds: ffi::time_t = seconds;
        let mut local = std::mem::MaybeUninit::<ffi::tm>::uninit();
        if unsafe { ffi::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
            result.valid = 0;
            return 0;
        }
        let local = unsafe { local.assume_init() };
        let Ok(year) = u16::try_from(i64::from(local.tm_year) + 1900) else {
            result.valid = 0;
            return 0;
        };
        result.valid = 1;
        result.year = year;
        result.month = (local.tm_mon + 1) as u8;
        result.day = local.tm_mday as u8;
        result.weekday = local.tm_wday as u8;
        result.hour = local.tm_hour as u8;
        result.minute = local.tm_min as u8;
        result.second = local.tm_sec as u8;
        0
    })
}
unsafe extern "C" fn command_notice(
    _context: *mut c_void,
    local: *const c_char,
    length: usize,
    completed: u32,
) {
    boundary((), || {
        let Some(local) =
            (unsafe { text(local, length) }).and_then(|value| CString::new(value).ok())
        else {
            return;
        };
        unsafe {
            ffi::ast_log(
                ffi::__LOG_NOTICE as i32,
                c"rust/asterisk/services.rs".as_ptr(),
                0,
                c"command".as_ptr(),
                c"rpt_advanced: node %s link command %s\n".as_ptr(),
                local.as_ptr(),
                if completed != 0 {
                    c"completed"
                } else {
                    c"failed"
                }
                .as_ptr(),
            );
        }
    });
}
unsafe extern "C" fn directory_lookup(
    context: *mut c_void,
    method: u32,
    static_file: *const c_char,
    static_length: usize,
    external_file: *const c_char,
    external_length: usize,
    remote: *const c_char,
    remote_length: usize,
    source: *const c_char,
    source_length: usize,
    output: *mut c_char,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    boundary(-1, || {
        let _ = context;
        let values = unsafe {
            (
                text(static_file, static_length),
                text(external_file, external_length),
                text(remote, remote_length),
                text(source, source_length),
            )
        };
        let (Some(static_file), Some(external_file), Some(remote), Some(source)) = values else {
            return -1;
        };
        let method = match method {
            0 => Method::Both,
            1 => Method::Dns,
            2 => Method::File,
            _ => return -1,
        };
        let Ok(destination) =
            DirectoryResolver::new(AsteriskDirectory, method, static_file, external_file)
                .lookup(remote, (!source.is_empty()).then_some(source))
        else {
            return -1;
        };
        if output.is_null() || written.is_null() || destination.len() > capacity {
            return -1;
        }
        unsafe {
            ptr::copy_nonoverlapping(destination.as_ptr(), output.cast(), destination.len());
            written.write(destination.len());
        }
        0
    })
}
struct ReservedRadio {
    radio: Option<Radio>,
    name: CString,
}
unsafe extern "C" fn radio_open(
    _context: *mut c_void,
    name: *const c_char,
    length: usize,
    maximum: usize,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        let Some(name) = (unsafe { text(name, length) }).and_then(|value| CString::new(value).ok())
        else {
            return -1;
        };
        let radio = Connection::open(&name).and_then(|connection| connection.into_radio(maximum));
        match radio {
            Ok(radio) => {
                *output = Box::into_raw(Box::new(ReservedRadio {
                    radio: Some(radio),
                    name,
                }))
                .cast();
                0
            }
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn radio_activate(
    _context: *mut c_void,
    radio: *mut c_void,
    receive: ffi::rptadv_radio_receive_v2,
    receive_context: *mut c_void,
    transmit: ffi::rptadv_radio_transmit_v2,
    transmit_context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(reserved) = (unsafe { radio.cast::<ReservedRadio>().as_mut() }) else {
            return -1;
        };
        let Some(radio) = reserved.radio.as_mut() else {
            return -1;
        };
        let mut direct = ffi::urp_ast_direct_callbacks {
            struct_size: size_of::<ffi::urp_ast_direct_callbacks>() as u32,
            abi_version: ffi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION,
            receive_context,
            receive,
            transmit_context,
            transmit,
            accepted_abi_version: 0,
        };
        if receive.is_some()
            && transmit.is_some()
            && unsafe { radio.attach_direct(&mut direct) }.is_ok()
            && radio.start(&reserved.name).is_ok()
        {
            return 0;
        }
        // A failed option/start may already have copied callbacks. Hangup must quiesce
        // those callbacks before returning failure to their owner.
        drop(reserved.radio.take());
        -1
    })
}
unsafe extern "C" fn radio_destroy(_context: *mut c_void, radio: *mut c_void) {
    boundary((), || {
        if !radio.is_null() {
            unsafe { drop(Box::from_raw(radio.cast::<ReservedRadio>())) };
        }
    });
}
unsafe extern "C" fn peer_dial(
    context: *mut c_void,
    destination: *const c_char,
    destination_length: usize,
    local: *const c_char,
    local_length: usize,
    maximum: usize,
    current: ffi::rptadv_current_v1,
    current_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let _ = context;
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        let values = unsafe {
            (
                text(destination, destination_length),
                text(local, local_length),
            )
        };
        let (Some(destination), Some(local), Some(current)) = (values.0, values.1, current) else {
            return -1;
        };
        let (Ok(destination), Ok(local)) = (CString::new(destination), CString::new(local)) else {
            return -1;
        };
        match PeerIo::dial(&destination, &local, maximum, || unsafe {
            current(current_context) != 0
        }) {
            Ok(peer) => {
                *output = Box::into_raw(Box::new(peer)).cast();
                0
            }
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn peer_rate(_context: *mut c_void, peer: *const c_void) -> u32 {
    boundary(0, || {
        unsafe { peer.cast::<PeerIo>().as_ref() }.map_or(0, PeerIo::rate)
    })
}
unsafe extern "C" fn peer_ready(_context: *mut c_void, peer: *mut c_void) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { peer.cast::<PeerIo>().as_mut() }) else {
            return -1;
        };
        peer.ready().map_or(-1, i32::from)
    })
}
unsafe extern "C" fn peer_read(
    _context: *mut c_void,
    peer: *mut c_void,
    event: ffi::rptadv_peer_event_v1,
    event_context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(event)) = (unsafe { peer.cast::<PeerIo>().as_mut() }, event) else {
            return -1;
        };
        peer.read(|input| unsafe {
            match input {
                Input::Text(bytes) => event(event_context, 1, bytes.as_ptr().cast(), bytes.len()),
                Input::Digit(digit) => {
                    let digit = digit as u8;
                    event(event_context, 2, ptr::from_ref(&digit).cast(), 1);
                }
                Input::Audio(samples) => {
                    event(event_context, 3, samples.as_ptr().cast(), samples.len());
                }
            }
        })
        .map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_send_text(
    _context: *mut c_void,
    peer: *mut c_void,
    text_pointer: *const c_char,
    length: usize,
) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { peer.cast::<PeerIo>().as_mut() }) else {
            return -1;
        };
        let Some(text) = (unsafe { text(text_pointer, length) }) else {
            return -1;
        };
        let Ok(text) = CString::new(text) else {
            return -1;
        };
        peer.send_text(&text).map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_send_digit(_context: *mut c_void, peer: *mut c_void, digit: u8) -> i32 {
    boundary(-1, || {
        unsafe { peer.cast::<PeerIo>().as_mut() }
            .ok_or(())
            .and_then(|peer| peer.send_digit(digit as char).map_err(|_| ()))
            .map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_write(
    _context: *mut c_void,
    peer: *mut c_void,
    samples: *const f32,
    count: usize,
) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { peer.cast::<PeerIo>().as_mut() }) else {
            return -1;
        };
        if samples.is_null() && count != 0 {
            return -1;
        }
        let samples = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(samples, count) }
        };
        peer.write(samples).map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_destroy(_context: *mut c_void, peer: *mut c_void) {
    boundary((), || {
        if !peer.is_null() {
            unsafe { drop(Box::from_raw(peer.cast::<PeerIo>())) };
        }
    });
}

struct Services(ffi::rptadv_host_services_v2);
// SAFETY: the table is immutable, its context is null, and object callbacks serialize each handle.
unsafe impl Sync for Services {}

static SERVICES: Services = Services(ffi::rptadv_host_services_v2 {
    struct_size: size_of::<ffi::rptadv_host_services_v2>() as u32,
    abi_version: 2,
    capability: *b"rptadv.hst2\0",
    context: ptr::null_mut(),
    local_time: Some(local_time),
    command_notice: Some(command_notice),
    reaper_acquire: Some(ffi::ast_replace_sigchld),
    reaper_release: Some(ffi::ast_unreplace_sigchld),
    directory_lookup: Some(directory_lookup),
    radio_open: Some(radio_open),
    radio_activate: Some(radio_activate),
    radio_destroy: Some(radio_destroy),
    peer_dial: Some(peer_dial),
    peer_rate: Some(peer_rate),
    peer_ready: Some(peer_ready),
    peer_read: Some(peer_read),
    peer_send_text: Some(peer_send_text),
    peer_send_digit: Some(peer_send_digit),
    peer_write: Some(peer_write),
    peer_destroy: Some(peer_destroy),
});

/// Return the immutable host-services table retained by the adapter DSO.
pub fn descriptor() -> &'static ffi::rptadv_host_services_v2 {
    &SERVICES.0
}

/// Transfer one raw peer owner to the portable product.
pub fn into_raw(peer: PeerIo) -> *mut c_void {
    Box::into_raw(Box::new(peer)).cast()
}

/// Release a peer retained by the adapter after product admission declined ownership.
///
/// # Safety
/// `peer` must be the unique pointer returned by [`into_raw`] and not yet transferred.
pub unsafe fn destroy_raw(peer: *mut c_void) {
    unsafe { peer_destroy(ptr::null_mut(), peer) };
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
