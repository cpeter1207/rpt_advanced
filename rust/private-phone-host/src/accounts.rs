use crate::phone::PreparedAccount;
use std::{net::SocketAddr, sync::Arc};
/// A real resolved numeric route for an exact immutable private transport row.
pub struct Route {
    pub(crate) account: Arc<PreparedAccount>,
    pub(crate) remote: SocketAddr,
}
impl Route {
    /// Reject malformed/unusable results before materializing a native account.
    pub fn new(account: Arc<PreparedAccount>, remote: SocketAddr) -> Option<Self> {
        (account.endpoint().1 == remote.port() && remote.port() != 0 && !remote.ip().is_unspecified()
            && !remote.ip().is_multicast()).then_some(Self {account, remote})
    }
    pub(crate) fn generation(&self) -> u64 { self.account.generation() }
    pub(crate) fn remote(&self) -> SocketAddr { self.remote }
}
