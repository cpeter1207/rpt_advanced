//! Product command-time local attachment configuration, not a media grant.
//! Both lists use whitespace-separated exact identifiers; commas and stars in
//! a valid NodeId remain literal. Empty or invalid lists authorize nothing.
use super::{ConfigDocument, NodeId};
use crate::link::PhoneAccountId;

pub(super) fn node_list_valid(value: &str) -> bool {
    value
        .split_whitespace()
        .all(|item| NodeId::new(item).is_ok())
}

pub(super) fn account_list_valid(value: &str) -> bool {
    value
        .split_whitespace()
        .all(|item| PhoneAccountId::new(item).is_ok())
}

fn contains(value: Option<&str>, exact: &str, valid: fn(&str) -> bool) -> bool {
    value.is_some_and(|value| valid(value) && value.split_whitespace().any(|item| item == exact))
}

impl ConfigDocument {
    /// Read both exact local permission lists from this current document.
    /// Account binding supplies only a default target, not either permission.
    /// Node permission is explicit, never inherited from general settings.
    /// Call authentication, current hosted targets and media readiness remain the
    /// command owner's responsibility; this returns no reservation or permit.
    pub fn phone_local_attachment_allowed(&self, account: &PhoneAccountId, target: &str) -> bool {
        contains(
            self.lookup(
                "attach_local_nodes",
                &[&format!("phone_account {}", account.as_str())],
            ),
            target,
            node_list_valid,
        ) && contains(
            self.lookup("phone_allow_accounts", &[target]),
            account.as_str(),
            account_list_valid,
        )
    }
}

#[cfg(test)]
#[path = "phone_attachment_tests.rs"]
mod tests;
