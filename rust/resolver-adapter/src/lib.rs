//! System DNS capability. Resolution runs only on control threads, never audio callbacks.
//! DNS policy belongs to the caller; this adapter preserves system address order and SRV selection.
#![deny(warnings, missing_docs)]

use std::{
    collections::hash_map::RandomState,
    ffi::{CStr, CString, c_void},
    hash::{BuildHasher, Hasher},
    io,
    net::{SocketAddr, ToSocketAddrs},
    panic::{AssertUnwindSafe, catch_unwind},
};

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    missing_docs
)]
mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}
/// Use the system resolver and its configured DNS servers on the control plane.
fn resolve_srv(service: &str) -> io::Result<Option<(String, u16)>> {
    resolve_srv_with(service, |service, answer| {
        // SAFETY: the query and writable answer buffer remain live for this synchronous call.
        unsafe {
            crate::abi::res_query(
                service.as_ptr(),
                1,
                33,
                answer.as_mut_ptr(),
                answer.len() as i32,
            )
        }
    })
}

fn resolve_srv_with(
    service: &str,
    query: impl FnOnce(&CStr, &mut [u8]) -> i32,
) -> io::Result<Option<(String, u16)>> {
    let service = CString::new(service).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut answer = [0_u8; 65535];
    let length = query(&service, &mut answer);
    if length < 0 {
        return Ok(None);
    }
    let answer = answer
        .get(..length as usize)
        .ok_or(io::ErrorKind::InvalidData)?;
    srv_record(answer, RandomState::new().build_hasher().finish())
}

fn valid_srv_host(host: &str) -> bool {
    !host.is_empty() && host != "."
}

/// Decode with libresolv, then apply SRV priority and weighted selection.
fn srv_record(answer: &[u8], choice: u64) -> io::Result<Option<(String, u16)>> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let mut message = std::mem::MaybeUninit::zeroed();
    // SAFETY: libresolv validates the packet before initializing the message view.
    if unsafe {
        crate::abi::ns_initparse(answer.as_ptr(), answer.len() as i32, message.as_mut_ptr())
    } != 0
    {
        return Err(invalid());
    }
    let mut message = unsafe { message.assume_init() };
    let mut records = Vec::new();
    for index in 0..message._counts[1] {
        // ns_parserr writes only the used prefix of name; initialize its unused bytes too.
        let mut record = std::mem::MaybeUninit::zeroed();
        // SAFETY: the initialized message borrows the live answer buffer.
        if unsafe { crate::abi::ns_parserr(&mut message, 1, i32::from(index), record.as_mut_ptr()) }
            != 0
        {
            return Err(invalid());
        }
        let record = unsafe { record.assume_init() };
        if record.type_ != 33 || record.rr_class != 1 {
            continue;
        }
        if record.rdlength < 7 {
            return Err(invalid());
        }
        // SAFETY: ns_parserr validated the RDATA extent within the answer.
        let data =
            unsafe { std::slice::from_raw_parts(record.rdata, usize::from(record.rdlength)) };
        let priority = u16::from_be_bytes([data[0], data[1]]);
        let weight = u16::from_be_bytes([data[2], data[3]]);
        let port = u16::from_be_bytes([data[4], data[5]]);
        let mut host = [0 as std::ffi::c_char; 1025];
        // SAFETY: dn_expand validates compressed labels against the complete answer bounds.
        let consumed = unsafe {
            crate::abi::dn_expand(
                answer.as_ptr(),
                answer.as_ptr().add(answer.len()),
                data.as_ptr().add(6),
                host.as_mut_ptr(),
                host.len() as i32,
            )
        };
        if consumed < 0 || consumed as usize + 6 != data.len() || port == 0 {
            return Err(invalid());
        }
        let host = unsafe { CStr::from_ptr(host.as_ptr()) }
            .to_str()
            .map_err(|_| invalid())?;
        if !valid_srv_host(host) {
            return Err(invalid());
        }
        records.push((priority, weight, host.to_owned(), port));
    }
    records.sort_by_key(|record| (record.0, record.1));
    let Some(priority) = records.first().map(|record| record.0) else {
        return Ok(None);
    };
    records.retain(|record| record.0 == priority);
    let total: u64 = records.iter().map(|record| u64::from(record.1)).sum();
    let mut selected = choice % (total + 1);
    Ok(records.into_iter().find_map(|(_, weight, host, port)| {
        if selected <= u64::from(weight) {
            Some((host, port))
        } else {
            selected -= u64::from(weight);
            None
        }
    }))
}

fn send_srv(
    record: io::Result<Option<(String, u16)>>,
    sink: unsafe extern "C" fn(*mut c_void, *const u8, usize, u16),
    context: *mut c_void,
) -> i32 {
    let Ok(record) = record else {
        return 1;
    };
    if let Some((host, port)) = record {
        // SAFETY: the owned host bytes stay live for the synchronous sink call.
        unsafe { sink(context, host.as_ptr().cast(), host.len(), port) };
    }
    0
}

fn send_addresses(
    addresses: io::Result<Vec<SocketAddr>>,
    sink: unsafe extern "C" fn(*mut c_void, *const u8, usize),
    context: *mut c_void,
) -> i32 {
    let Ok(addresses) = addresses else {
        return 1;
    };
    if addresses.is_empty() {
        return 1;
    }
    for address in addresses {
        let address = address.to_string();
        // SAFETY: the owned address bytes stay live for the synchronous sink call.
        unsafe { sink(context, address.as_ptr().cast(), address.len()) };
    }
    0
}

fn boundary(operation: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(-1)
}

unsafe fn input<'a>(pointer: *const u8, length: usize) -> Option<&'a str> {
    if length > isize::MAX as usize || (pointer.is_null() && length != 0) {
        return None;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        // SAFETY: callers provide a live readable byte extent; null and oversized borrows are rejected.
        unsafe { std::slice::from_raw_parts(pointer, length) }
    };
    std::str::from_utf8(bytes).ok()
}

unsafe extern "C" fn create(context: *mut *mut c_void) -> i32 {
    boundary(|| {
        let Some(context) = (unsafe { context.as_mut() }) else {
            return -1;
        };
        *context = Box::into_raw(Box::new(())).cast();
        0
    })
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    boundary(|| {
        if !context.is_null() {
            // SAFETY: only the unique handle returned by create may be destroyed, after all calls end.
            unsafe { drop(Box::from_raw(context.cast::<()>())) };
        }
        0
    });
}

unsafe extern "C" fn query_srv(
    context: *mut c_void,
    service: *const u8,
    length: usize,
    sink: abi::rptadv_resolver_srv_sink_v1,
    result_context: *mut c_void,
) -> i32 {
    boundary(|| {
        let (Some(service), Some(sink)) = (unsafe { input(service, length) }, sink) else {
            return -1;
        };
        if context.is_null() {
            return -1;
        }
        send_srv(resolve_srv(service), sink, result_context)
    })
}

unsafe extern "C" fn query_addresses(
    context: *mut c_void,
    endpoint: *const u8,
    length: usize,
    sink: abi::rptadv_resolver_address_sink_v1,
    result_context: *mut c_void,
) -> i32 {
    boundary(|| {
        let (Some(endpoint), Some(sink)) = (unsafe { input(endpoint, length) }, sink) else {
            return -1;
        };
        if context.is_null() {
            return -1;
        }
        send_addresses(
            endpoint.to_socket_addrs().map(Iterator::collect),
            sink,
            result_context,
        )
    })
}

static DESCRIPTOR: abi::rptadv_resolver_descriptor_v1 = abi::rptadv_resolver_descriptor_v1 {
    struct_size: std::mem::size_of::<abi::rptadv_resolver_descriptor_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.resolver\0",
    create: Some(create),
    destroy: Some(destroy),
    resolve_srv: Some(query_srv),
    resolve_addresses: Some(query_addresses),
};

/// Return the immutable resolver ABI 1 table, valid until the shared object is unloaded.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_resolver_adapter_descriptor_v1()
-> *const abi::rptadv_resolver_descriptor_v1 {
    &DESCRIPTOR
}

#[cfg(test)]
mod tests;
