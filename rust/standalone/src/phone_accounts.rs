//! Off-control account preparation, without provider activation or product publication.
//!
//! Ordinary configuration remains owned by the product visitor. This module reads
//! only a literal private secret and resolves one numeric endpoint through its caller.
//! The caller must retain bounded resolver admission in the actual blocking job,
//! including after async cancellation. Prepared snapshots do not commit a reload:
//! the host must distinguish rejected preparation from post-publication failures.

use crate::configuration::{PhoneAccountProtocol, ResolvedPhoneAccount};
use rptadv_peer_support::secrets::{SecretError, account_secrets_path, read_private_file_bounded};
use std::{fmt, io, net::SocketAddr, path::Path, sync::Arc};

/// A prepared immutable account; credentials and provider endpoints are not printable.
pub struct PreparedAccount {
    settings: ResolvedPhoneAccount,
    remote: SocketAddr,
    secret: String,
    generation: u64,
}
impl PreparedAccount {
    /// Borrow the resolved public settings copied by the product visitor.
    pub fn settings(&self) -> &ResolvedPhoneAccount {
        &self.settings
    }
    /// Borrow the numeric provider endpoint selected during preparation.
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }
    /// Borrow literal credentials only for transport preparation/authentication.
    pub fn secret(&self) -> &str {
        &self.secret
    }
    /// Exact nonzero identity generation, never inferred from caller fields.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
impl fmt::Debug for PreparedAccount {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("PreparedAccount")
            .field("id", &self.settings.id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Sorted immutable candidate or active accounts, sharing unchanged owners.
#[derive(Default)]
pub struct PreparedAccounts {
    accounts: Vec<Arc<PreparedAccount>>,
}
impl PreparedAccounts {
    /// Borrow accounts in ascending opaque-label order.
    pub fn as_slice(&self) -> &[Arc<PreparedAccount>] {
        &self.accounts
    }
}

/// Content-free preparation failure, safe to expose without credential material.
#[derive(Debug, PartialEq, Eq)]
pub enum PreparationError {
    /// Invalid prepared identity or exhausted generation space.
    Invalid(&'static str),
    /// Existing private-file diagnostics contain no path or credential material.
    Secret(SecretError),
}
impl fmt::Display for PreparationError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => out.write_str(reason),
            Self::Secret(error) => fmt::Display::fmt(error, out),
        }
    }
}
impl std::error::Error for PreparationError {}

/// One serialized preparation owner's monotonic identity allocator.
///
/// Retain this owner across candidate failures, drops and reloads; replacing it
/// would discard the generation history belonging to its active snapshots.
#[derive(Default)]
pub struct AccountPreparer {
    last_generation: u64,
}
impl AccountPreparer {
    /// Prepare before publication; failure or candidate drop never changes active owners.
    ///
    /// A caller that permits name resolution runs this off control/foreground.
    /// Numeric-only startup/reload may use its existing serialized configuration owner.
    ///
    /// The resolver runs only for nonnumeric endpoints. The caller owns its bounded
    /// execution and cancellation contract. No listener, registrar, poller or thread
    /// is created here, and no candidate is automatically published.
    pub fn prepare(
        &mut self,
        configuration_path: &Path,
        mut records: Vec<ResolvedPhoneAccount>,
        active: &PreparedAccounts,
        mut resolve: impl FnMut(&str) -> io::Result<SocketAddr>,
    ) -> Result<PreparedAccounts, PreparationError> {
        records.retain(|record| record.enabled);
        records.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        if records.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(PreparationError::Invalid("duplicate phone account label"));
        }
        let mut accounts = Vec::with_capacity(records.len());
        for settings in records {
            let path =
                account_secrets_path(configuration_path, Some(Path::new(&settings.secrets_file)));
            let mut secret =
                read_private_file_bounded(&path, 4096).map_err(PreparationError::Secret)?;
            if secret.ends_with('\n') {
                secret.pop();
                if secret.ends_with('\r') {
                    secret.pop();
                }
            }
            if secret.is_empty()
                || secret.chars().any(char::is_control)
                || (settings.protocol == PhoneAccountProtocol::Iax && secret.len() > 255)
            {
                return Err(PreparationError::Invalid(
                    "invalid phone account credential",
                ));
            }
            let remote = settings
                .server
                .parse::<SocketAddr>()
                .or_else(|_| resolve(&settings.server))
                .map_err(|_| PreparationError::Invalid("cannot resolve phone account endpoint"))?;
            if remote.port() == 0 || remote.ip().is_unspecified() {
                return Err(PreparationError::Invalid("invalid phone account endpoint"));
            }
            let unchanged = active
                .accounts
                .binary_search_by(|account| account.settings.id.cmp(&settings.id))
                .ok()
                .map(|index| &active.accounts[index])
                .filter(|account| {
                    account.settings == settings
                        && account.remote == remote
                        && account.secret == secret
                });
            if let Some(account) = unchanged {
                accounts.push(Arc::clone(account));
            } else {
                self.last_generation =
                    self.last_generation
                        .checked_add(1)
                        .ok_or(PreparationError::Invalid(
                            "phone account generation space exhausted",
                        ))?;
                accounts.push(Arc::new(PreparedAccount {
                    settings,
                    remote,
                    secret,
                    generation: self.last_generation,
                }));
            }
        }
        Ok(PreparedAccounts { accounts })
    }
}

#[cfg(test)]
#[path = "phone_accounts_tests.rs"]
mod tests;
