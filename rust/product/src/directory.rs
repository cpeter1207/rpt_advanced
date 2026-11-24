//! Product-owned directory ordering, validation, and source authentication.

use std::net::{IpAddr, SocketAddr};
use std::{collections::BTreeMap, num::{NonZeroU16, NonZeroU64}, sync::Arc};
use rpt_advanced_core::config::resources::{
    candidate::ResourceId, generation::RuntimeConfiguration, profiles::{AllStarLinkProfileSettings, JitterProfile},
};

/// Public inputs to a dormant ASL participant. The resource UUID selects the
/// encrypted registration record independently of its public network identity.
pub struct AccountPlan<'a> {
    /// Station UUID for local observation and media ownership.
    pub station: ResourceId,
    /// Explicit ASL account UUID for credential selection and retirement.
    pub account: ResourceId,
    /// Canonical wire number used by listeners, the registrar and statistics.
    pub wire: NonZeroU64,
    /// Already resolved nullable endpoints, directory policy and ACL.
    pub settings: &'a AllStarLinkProfileSettings,
    /// Independent effective jitter tuple and inherited fallback sources.
    pub jitter: &'a JitterProfile,
}

/// Select only enabled ports with an attached, complete ASL authority. A site
/// default supplies inherited values but never contributes an account on its own.
pub fn account_plans(config: &RuntimeConfiguration) -> impl Iterator<Item = AccountPlan<'_>> {
    config.protocols.ports().filter_map(|(station, port)| {
        let account = port.allstarlink.as_ref().filter(|_| port.metadata.enabled)?;
        Some(AccountPlan { station, account: account.binding.id, wire: account.settings.node_number?,
                           settings: &account.settings, jitter: &account.jitter })
    })
}

/// A trusted static row selected by the modular resource owner. External feeds
/// continue to use the existing independent protocol-file decoder.
#[derive(Clone, Debug)]
pub struct RegisteredNode {
    host: url::Host<String>,
    port: NonZeroU16,
    source: IpAddr,
}

impl RegisteredNode {
    /// Validate an already selected endpoint and retain its independent permitted source.
    pub fn new(host: &str, port: NonZeroU16, source: IpAddr) -> Result<Self, DirectoryError> {
        let host = url::Host::parse(host).map_err(|_| DirectoryError::InvalidRecord)?;
        Ok(Self { host, port, source: normalize(source) })
    }
}

/// A station's immutable directory selection, captured before leaving the serialized owner.
#[derive(Clone)]
pub struct ManagedDirectory {
    /// Registered station UUID.
    pub station: [u8; 16],
    /// Registered ASL resource UUID.
    pub account: [u8; 16],
    /// Changes on replacement of this account or its directory/ACL authority.
    pub generation: NonZeroU64,
    /// The existing directory method (both, DNS or protocol file).
    pub method: u32,
    /// Downloaded directory feed, independent of any controller-authored static entries.
    pub external: String,
    /// Registered rows, empty when none were selected.
    pub registered: Arc<BTreeMap<String, RegisteredNode>>,
}

/// Completed source lookup tied to its exact request and selected owner.
pub struct VerifiedRoute {
    owner: Arc<ManagedDirectory>,
    remote: String,
    source: Option<IpAddr>,
    destination: String,
}

impl ManagedDirectory {
    /// Execute on the dial/admission worker. Static source mismatch is authoritative;
    /// only an absent row enters the product's existing DNS/external-file ordering.
    pub fn resolve<B: Backend>(self: &Arc<Self>, backend: B, remote: &str,
                              source: Option<IpAddr>) -> Result<VerifiedRoute, DirectoryError> {
        if self.method > 2 || remote.is_empty() || remote.len() > 63 || !remote.bytes().all(|b| b.is_ascii_digit()) {
            return Err(DirectoryError::InvalidRecord);
        }
        let source = source.map(normalize);
        let destination = if let Some(row) = self.registered.get(remote) {
            if source.is_some_and(|ip| ip != row.source) { return Err(DirectoryError::SourceMismatch); }
            format!("radio@{}:{}/{remote}", row.host, row.port)
        } else {
            // Managed static input is typed; no legacy controller file is consulted.
            DirectoryResolver::new(backend, self.method, "", &self.external)
                .lookup(remote, source.map(|ip| ip.to_string()).as_deref())?
        };
        Ok(VerifiedRoute { owner: Arc::clone(self), remote: remote.to_owned(), source, destination })
    }
}

impl VerifiedRoute {
    /// Select one numeric destination on the dial worker. DNS routes already contain
    /// the selected address. Registered and external hostnames use the same resolver.
    #[cfg(target_os = "linux")]
    pub(crate) fn outbound_endpoint(&self, backend: &impl Backend) -> Result<SocketAddr, DirectoryError> {
        let (authority, remote) = self.destination.strip_prefix("radio@")
            .and_then(|s| s.rsplit_once('/')).ok_or(DirectoryError::InvalidRecord)?;
        if self.source.is_some() || remote != self.remote || authority.contains(['/', '@', '\\', '?', '#']) {
            return Err(DirectoryError::InvalidRecord);
        }
        if let Ok(address) = authority.parse::<SocketAddr>() {
            return (address.port() != 0).then_some(address).ok_or(DirectoryError::InvalidRecord);
        }
        let literal = authority.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(authority);
        if let Ok(ip) = literal.parse::<IpAddr>() { return Ok(SocketAddr::new(ip, 4569)); }
        let (name, port) = match authority.split_once(':') {
            Some((name, port)) => (name, port.parse::<NonZeroU16>().map_err(|_| DirectoryError::InvalidRecord)?.get()),
            None => (authority, 4569),
        };
        let ip = match url::Host::parse(name).map_err(|_| DirectoryError::InvalidRecord)? {
            url::Host::Domain(name) => backend.addresses(&name, port).map_err(dns_error)?
                .into_iter().next().ok_or(DirectoryError::NotFound)?,
            url::Host::Ipv4(ip) => IpAddr::V4(ip),
            url::Host::Ipv6(ip) => IpAddr::V6(ip),
        };
        Ok(SocketAddr::new(ip, port))
    }

    /// Recheck the captured selection, remote number and source before authorizing
    /// the operation or attaching its peer. Recreated accounts cannot use old proofs.
    pub fn destination(&self, current: &Arc<ManagedDirectory>, remote: &str,
                       source: Option<IpAddr>) -> Option<&str> {
        (Arc::ptr_eq(&self.owner, current) && self.remote == remote && self.source == source.map(normalize))
            .then_some(self.destination.as_str())
    }
}

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
