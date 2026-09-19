//! Authoritative static/DNS/file ASL directory resolution and source verification.
use crate::bindings as ffi;
use std::{
    ffi::{CStr, CString},
    net::{IpAddr, SocketAddr},
    ptr,
};

/// Directory ordering selected by the current configuration.
#[derive(Clone, Copy)]
pub enum Method {
    /// Static, then DNS, then external file.
    Both,
    /// Static then DNS.
    Dns,
    /// Static then external file.
    File,
}

/// Lookup failed without producing an authenticated destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryError {
    /// Invalid identity, address, or authoritative record.
    Rejected,
    /// No selected source has this identity.
    Absent,
}

/// External directory operations; called only from the Asterisk control owner.
pub trait Backend {
    /// Read an optional extnodes value; unavailable files are absent.
    fn record(&self, path: &str, node: &str) -> Result<Option<String>, DirectoryError>;
    /// Read an optional SRV target and port.
    fn srv(&self, service: &str) -> Result<Option<(String, u16)>, DirectoryError>;
    /// Resolve all addresses for a DNS host, preserving family information.
    fn addresses(&self, host: &str) -> Result<Vec<IpAddr>, DirectoryError>;
}

/// Directory lookup with explicit authority and no contradiction fallback.
pub struct DirectoryResolver<B> {
    /// External I/O owner.
    pub backend: B,
    method: Method,
    static_file: String,
    external_file: String,
}
impl<B: Backend> DirectoryResolver<B> {
    /// Select sources before accepting incoming links.
    pub fn new(backend: B, method: Method, static_file: &str, external_file: &str) -> Self {
        Self {
            backend,
            method,
            static_file: static_file.into(),
            external_file: external_file.into(),
        }
    }
    /// Resolve one decimal ASL node, optionally authenticating its numeric source IP.
    pub fn lookup(&self, node: &str, source: Option<&str>) -> Result<String, DirectoryError> {
        if node.is_empty() || node.len() > 63 || !node.bytes().all(|b| b.is_ascii_digit()) {
            return Err(DirectoryError::Rejected);
        }
        let source = source
            .map(|ip| {
                ip.parse::<IpAddr>()
                    .map(normalize)
                    .map_err(|_| DirectoryError::Rejected)
            })
            .transpose()?;
        if let Some(destination) = self.file(&self.static_file, node, source)? {
            return Ok(destination);
        }
        if !matches!(self.method, Method::File) {
            let (host, port) = self
                .backend
                .srv(&format!("_iax._udp.{node}.nodes.allstarlink.org"))?
                .unwrap_or_else(|| (format!("{node}.nodes.allstarlink.org"), 4569));
            let addresses = self.backend.addresses(&host)?;
            if let Some(address) = addresses
                .iter()
                .find(|ip| source.is_none_or(|source| source == normalize(**ip)))
            {
                return Ok(format!("radio@{}/{node}", SocketAddr::new(*address, port)));
            }
            if source.is_some() && !addresses.is_empty() {
                return Err(DirectoryError::Rejected);
            }
        }
        if !matches!(self.method, Method::Dns) {
            if let Some(destination) = self.file(&self.external_file, node, source)? {
                return Ok(destination);
            }
        }
        Err(DirectoryError::Absent)
    }
    fn file(
        &self,
        path: &str,
        node: &str,
        source: Option<IpAddr>,
    ) -> Result<Option<String>, DirectoryError> {
        if path.is_empty() {
            return Ok(None);
        }
        let Some(record) = self.backend.record(path, node)? else {
            return Ok(None);
        };
        let (target, address) = record.split_once(',').ok_or(DirectoryError::Rejected)?;
        let host = target
            .strip_prefix("radio@")
            .and_then(|target| target.strip_suffix(&format!("/{node}")))
            .ok_or(DirectoryError::Rejected)?;
        let address = normalize(
            address
                .parse::<IpAddr>()
                .map_err(|_| DirectoryError::Rejected)?,
        );
        if host.is_empty()
            || record.bytes().any(|b| b.is_ascii_whitespace() || b == 0)
            || source.is_some_and(|source| source != address)
        {
            return Err(DirectoryError::Rejected);
        }
        Ok(Some(target.into()))
    }
}
fn normalize(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    }
}
fn cstring(value: &str) -> Result<CString, DirectoryError> {
    CString::new(value).map_err(|_| DirectoryError::Rejected)
}

/// Public Asterisk config, SRV and address resolver implementation.
pub struct AsteriskDirectory;
impl Backend for AsteriskDirectory {
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

#[cfg(test)]
#[path = "directory_tests.rs"]
pub(crate) mod tests;
