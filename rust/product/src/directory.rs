//! Product-owned directory ordering, validation, and source authentication.

use std::net::{IpAddr, SocketAddr};

/// Backend translation preserves whether a DNS failure permits external-file fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendError {
    /// An authoritative answer or backend contract is invalid.
    Rejected,
    /// DNS is unavailable; the selected method determines whether to fall back.
    Unavailable,
}

/// The existing standalone diagnostic categories, shared with the product policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DirectoryError {
    /// Invalid identity, source, or authoritative record.
    InvalidRecord = 1,
    /// An authoritative address contradicts the incoming source.
    SourceMismatch = 2,
    /// No selected directory supplied a destination.
    NotFound = 3,
    /// The DNS-only backend was unavailable.
    DnsFailed = 4,
}

/// Control-plane directory I/O; implementations own native resolver/file resources.
pub trait Backend {
    /// Return a copied extnodes value; an unavailable file is absent.
    fn record(&self, path: &str, node: &str) -> Result<Option<String>, BackendError>;
    /// Return the backend-selected SRV target and port, or absence.
    fn srv(&self, service: &str) -> Result<Option<(String, u16)>, BackendError>;
    /// Resolve addresses in backend order for the selected host and port.
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, BackendError>;
}

/// One configured static/DNS/external lookup policy over a host I/O backend.
pub struct DirectoryResolver<B> {
    /// Backend owns only directory I/O and platform error translation.
    pub backend: B,
    method: u32,
    static_file: String,
    external_file: String,
}
impl<B: Backend> DirectoryResolver<B> {
    /// Select sources: zero both, one DNS, two external file; static always wins.
    pub fn new(backend: B, method: u32, static_file: &str, external_file: &str) -> Self {
        Self {
            backend,
            method,
            static_file: static_file.into(),
            external_file: external_file.into(),
        }
    }

    /// Resolve a decimal node and optionally authenticate its numeric source IP.
    pub fn lookup(&self, node: &str, source: Option<&str>) -> Result<String, DirectoryError> {
        if self.method > 2
            || node.is_empty()
            || node.len() > 63
            || !node.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(DirectoryError::InvalidRecord);
        }
        let source = source
            .map(|ip| {
                ip.parse::<IpAddr>()
                    .map(normalize)
                    .map_err(|_| DirectoryError::InvalidRecord)
            })
            .transpose()?;
        if let Some(destination) = self.file(&self.static_file, node, source)? {
            return Ok(destination);
        }
        if self.method != 2 {
            match self.dns(node, source) {
                Ok(Some(destination)) => return Ok(destination),
                Ok(None) => {}
                Err(DirectoryError::DnsFailed) if self.method == 0 => {}
                Err(error) => return Err(error),
            }
        }
        if self.method != 1 {
            if let Some(destination) = self.file(&self.external_file, node, source)? {
                return Ok(destination);
            }
        }
        Err(DirectoryError::NotFound)
    }

    fn dns(&self, node: &str, source: Option<IpAddr>) -> Result<Option<String>, DirectoryError> {
        let (host, port) = self
            .backend
            .srv(&format!("_iax._udp.{node}.nodes.allstarlink.org"))
            .map_err(dns_error)?
            .unwrap_or_else(|| (format!("{node}.nodes.allstarlink.org"), 4569));
        let addresses = self.backend.addresses(&host, port).map_err(dns_error)?;
        if let Some(address) = addresses
            .iter()
            .find(|ip| source.is_none_or(|source| source == normalize(**ip)))
        {
            return Ok(Some(format!(
                "radio@{}/{node}",
                SocketAddr::new(*address, port)
            )));
        }
        if source.is_some() && !addresses.is_empty() {
            return Err(DirectoryError::SourceMismatch);
        }
        Ok(None)
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
        let Some(record) = self
            .backend
            .record(path, node)
            .map_err(|_| DirectoryError::InvalidRecord)?
        else {
            return Ok(None);
        };
        let (target, address) = record
            .split_once(',')
            .ok_or(DirectoryError::InvalidRecord)?;
        let host = target
            .strip_prefix("radio@")
            .and_then(|target| target.strip_suffix(&format!("/{node}")))
            .ok_or(DirectoryError::InvalidRecord)?;
        let address = normalize(
            address
                .parse::<IpAddr>()
                .map_err(|_| DirectoryError::InvalidRecord)?,
        );
        if host.is_empty() || record.bytes().any(|b| b.is_ascii_whitespace() || b == 0) {
            return Err(DirectoryError::InvalidRecord);
        }
        if source.is_some_and(|source| source != address) {
            return Err(DirectoryError::SourceMismatch);
        }
        Ok(Some(target.into()))
    }
}

fn dns_error(error: BackendError) -> DirectoryError {
    match error {
        BackendError::Rejected => DirectoryError::InvalidRecord,
        BackendError::Unavailable => DirectoryError::DnsFailed,
    }
}
fn normalize(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    }
}

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;
