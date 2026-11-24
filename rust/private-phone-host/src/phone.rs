//! Final private transport rows copied during host preparation. DNS, registrar
//! and call I/O belong to the bounded off-control provider owner.
use crate::sip_provider::{Account, Error, Provider, Settings};
use zeroize::Zeroizing;
use std::{fmt::Write, net::{IpAddr, SocketAddr}, sync::Arc};
use uuid::Uuid;

/// Transport family selected for one explicit registered account.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Protocol { Sip, Iax2 }

/// Borrowed view of the checked product ABI row. No credential is printable.
pub struct AccountRow<'a> {
    pub port_id: [u8; 16],
    pub account_id: [u8; 16],
    pub generation: u64,
    pub protocol: Protocol,
    pub server: &'a str,
    pub port: u16,
    pub username: &'a str,
    pub credential: &'a [u8],
    pub jitter_ms: [u32; 3],
}

/// One immutable checked transport account. Allocation is inert. Resolving the
/// address and activating a native SIP/IAX context are separate owner operations.
pub struct PreparedAccount {
    port_id: Uuid,
    account_id: Uuid,
    generation: u64,
    protocol: Protocol,
    server: String,
    port: u16,
    username: String,
    jitter_ms: [u32; 3],
    credential: Zeroizing<Vec<u8>>,
}
impl PreparedAccount {
    /// Registry port; never a radio network number.
    pub fn port_id(&self) -> Uuid { self.port_id }
    /// Exact explicit provider resource.
    pub fn account_id(&self) -> Uuid { self.account_id }
    /// Nonreusable admission generation selected by the product.
    pub fn generation(&self) -> u64 { self.generation }
    /// Prepared transport family.
    pub fn protocol(&self) -> Protocol { self.protocol }
    /// Authored registrar/provider endpoint, before numeric resolution.
    pub fn endpoint(&self) -> (&str, u16) { (&self.server, self.port) }
    /// Literal transport username.
    pub fn username(&self) -> &str { &self.username }
    /// Native minimum, initial, and maximum packet jitter bounds.
    pub fn jitter_ms(&self) -> [u32; 3] { self.jitter_ms }
    /// Copy and validate an exact borrowed row without opening any network socket.
    pub fn copy(row: AccountRow<'_>) -> Result<Self, Error> {
        let port_id = Uuid::from_bytes(row.port_id);
        let account_id = Uuid::from_bytes(row.account_id);
        let [min, initial, max] = row.jitter_ms;
        let credential = std::str::from_utf8(row.credential).map_err(|_| Error::Contract)?;
        if port_id.is_nil() || account_id.is_nil() || port_id == account_id || row.generation == 0
            || row.port == 0 || row.server.is_empty() || row.server.len() > 255
            || row.server.bytes().any(|byte| !byte.is_ascii_alphanumeric() && !b".:-[]".contains(&byte))
            || row.username.is_empty() || row.username.len() > 255 || row.username.chars().any(char::is_control)
            || credential.is_empty() || credential.len() > 4096 || credential.as_bytes().contains(&0)
            || min > initial || initial > max {
            return Err(Error::Contract);
        }
        if row.protocol == Protocol::Sip && (credential.contains(':') || row.username.contains([':', '"', '\\'])
            || row.username.chars().any(char::is_whitespace)) {
            return Err(Error::Contract);
        }
        Ok(Self { port_id, account_id, generation: row.generation, protocol: row.protocol,
            server: row.server.to_owned(), port: row.port, username: row.username.to_owned(), jitter_ms: row.jitter_ms,
            credential: Zeroizing::new(row.credential.to_vec()) })
    }
    /// Reject reuse of one native generation for a different immutable configuration.
    pub fn same_registration(&self, other: &Self) -> bool {
        self.port_id == other.port_id && self.account_id == other.account_id
            && self.generation == other.generation && self.protocol == other.protocol
            && self.server == other.server && self.port == other.port && self.username == other.username
            && self.credential.as_slice() == other.credential.as_slice()
    }
    /// Borrow the native transport credential for its synchronous call only.
    pub fn credential(&self) -> &str {
        std::str::from_utf8(self.credential.as_slice()).expect("validated transport credential")
    }
    /// Build the URI for this account's configured registrar, optionally with
    /// one encoded user component. The native next hop stays pinned by open_sip.
    pub(crate) fn sip_uri(&self, user: Option<&str>) -> String {
        let host = self.server.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(&self.server);
        let authority = match host.parse::<IpAddr>() {
            Ok(IpAddr::V6(ip)) => format!("[{ip}]:{}", self.port),
            _ => format!("{host}:{}", self.port),
        };
        let mut uri = String::from("sip:");
        if let Some(user) = user {
            for byte in user.bytes() {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    uri.push(char::from(byte));
                } else {
                    write!(uri, "%{byte:02X}").expect("String writes cannot fail");
                }
            }
            uri.push('@');
        }
        uri.push_str(&authority);
        uri
    }

    /// Native allocation is allowed only on the reactor's apply turn, after
    /// bounded resolution. Registrar authority stays distinct from a numeric next hop.
    pub fn open_sip(&self, provider: &Arc<Provider>, remote: SocketAddr) -> Result<Account, Error> {
        if self.protocol != Protocol::Sip || !provider.can_originate() || remote.port() != self.port
            || remote.ip().is_unspecified() || remote.ip().is_multicast() { return Err(Error::Contract); }
        provider.prepare(&Settings {
            generation: self.generation, expires: 300,
            bind: if remote.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" },
            next_hop: &remote.to_string(), registrar: &self.sip_uri(None),
            aor: &self.sip_uri(Some(&self.username)), username: &self.username,
            password: self.credential(),
        })
    }
}
