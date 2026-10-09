//! Public Asterisk directory I/O; product owns ordering and authentication.
use crate::bindings as ffi;
use crate::services::{boundary, text};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    net::IpAddr,
    ptr,
};

/// An authoritative native resolver result cannot be represented safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryError {
    /// Invalid backend input or result.
    Rejected,
}
fn cstring(value: &str) -> Result<CString, DirectoryError> {
    CString::new(value).map_err(|_| DirectoryError::Rejected)
}

/// Public Asterisk config, SRV and address resolver implementation.
pub struct AsteriskDirectory;
impl AsteriskDirectory {
    fn record(&self, path: &str, node: &str) -> Result<Option<String>, DirectoryError> {
        let path = cstring(path)?;
        let node = cstring(node)?;
        // SAFETY: config and its borrowed value remain live until copied and destroyed.
        unsafe {
            let config = ffi::ast_config_load2(
                path.as_ptr(),
                c"rpt_advanced".as_ptr(),
                ffi::ast_flags { flags: 0 },
            );
            if config.is_null() || config as usize >= usize::MAX - 1 {
                return Ok(None);
            }
            let value = ffi::ast_variable_retrieve(config, c"extnodes".as_ptr(), node.as_ptr());
            let result = if value.is_null() {
                Ok(None)
            } else {
                CStr::from_ptr(value)
                    .to_str()
                    .map(|value| Some(value.to_owned()))
                    .map_err(|_| DirectoryError::Rejected)
            };
            ffi::ast_config_destroy(config);
            result
        }
    }
    fn srv(&self, service: &str) -> Result<Option<(String, u16)>, DirectoryError> {
        let service = cstring(service)?;
        // SAFETY: result host is copied before freeing the resolver context.
        unsafe {
            let mut context = ptr::null_mut();
            let mut host = ptr::null();
            let mut port = 4569;
            let code = ffi::ast_srv_lookup(&mut context, service.as_ptr(), &mut host, &mut port);
            let result = if code != 0 {
                Ok(None)
            } else if host.is_null() {
                Err(DirectoryError::Rejected)
            } else {
                CStr::from_ptr(host)
                    .to_str()
                    .map(|host| Some((host.to_owned(), port)))
                    .map_err(|_| DirectoryError::Rejected)
            };
            ffi::ast_srv_cleanup(&mut context);
            result
        }
    }
    fn addresses(&self, host: &str) -> Result<Vec<IpAddr>, DirectoryError> {
        let host = cstring(host)?;
        // SAFETY: Asterisk allocates count addresses; copy each before releasing the array.
        unsafe {
            let mut addresses = ptr::null_mut();
            let count = ffi::ast_sockaddr_resolve(
                &mut addresses,
                host.as_ptr(),
                ffi::PARSE_PORT_FORBID as i32,
                ffi::AST_AF_UNSPEC as i32,
            );
            let mut result = Vec::new();
            for index in 0..count.max(0) as usize {
                let text = ffi::ast_sockaddr_stringify_fmt(
                    addresses.add(index),
                    ffi::AST_SOCKADDR_STR_ADDR as i32,
                );
                if !text.is_null() {
                    if let Ok(ip) = CStr::from_ptr(text)
                        .to_string_lossy()
                        .trim_matches(['[', ']'])
                        .parse()
                    {
                        result.push(ip);
                    }
                }
            }
            free(addresses.cast());
            Ok(result)
        }
    }
}
unsafe extern "C" {
    fn free(pointer: *mut std::ffi::c_void);
}

/// Copy one raw Asterisk extnodes value through the borrowed result sink.
pub(crate) unsafe extern "C" fn directory_record(
    _: *mut c_void,
    path: *const c_char,
    path_length: usize,
    node: *const c_char,
    node_length: usize,
    sink: ffi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(path), Some(node), Some(sink)) = (
            unsafe { text(path, path_length) },
            unsafe { text(node, node_length) },
            sink,
        ) else {
            return -1;
        };
        let Ok(record) = AsteriskDirectory.record(path, node) else {
            return -1;
        };
        if let Some(record) = record {
            unsafe { sink(context, record.as_ptr().cast(), record.len()) };
        }
        0
    })
}
/// Preserve Asterisk's SRV absence versus malformed-result distinction.
pub(crate) unsafe extern "C" fn directory_srv(
    _: *mut c_void,
    service: *const c_char,
    length: usize,
    sink: ffi::rptadv_directory_srv_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(service), Some(sink)) = (unsafe { text(service, length) }, sink) else {
            return -1;
        };
        let Ok(record) = AsteriskDirectory.srv(service) else {
            return -1;
        };
        if let Some((host, port)) = record {
            unsafe { sink(context, host.as_ptr().cast(), host.len(), port) };
        }
        0
    })
}
/// Copy numeric addresses in Asterisk resolver order, retaining empty-answer behavior.
pub(crate) unsafe extern "C" fn directory_addresses(
    _: *mut c_void,
    host: *const c_char,
    length: usize,
    _: u16,
    sink: ffi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(host), Some(sink)) = (unsafe { text(host, length) }, sink) else {
            return -1;
        };
        let Ok(addresses) = AsteriskDirectory.addresses(host) else {
            return -1;
        };
        for address in addresses {
            let address = address.to_string();
            unsafe { sink(context, address.as_ptr().cast(), address.len()) };
        }
        0
    })
}
/// Asterisk did not emit a directory-specific failure diagnostic.
pub(crate) unsafe extern "C" fn directory_notice(_: *mut c_void, _: u32) {}

#[cfg(test)]
#[path = "directory_tests.rs"]
pub(crate) mod tests;
