//! Borrowed, single-consumption listener configuration preparation.
//! Commit only after confirmed product publication; drop on rejected preparation.
use super::{
    IaxError, IaxServer,
    accounts::{self, ProviderAccount},
};
use crate::abi;
use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
    sync::Arc,
};

type ConsumeToken = unsafe extern "C" fn(*mut c_void);

/// One complete listener replacement, including numeric and phone admission.
pub struct ListenerConfiguration<'listener, 'settings> {
    /// Retained unique listener, never destroyed while its batch remains alive.
    pub listener: &'listener mut IaxServer,
    /// Numeric ASL identities; empty with no accounts disables admission.
    pub local_nodes: &'settings [String],
    /// Copied provider settings; credentials never become numeric aliases.
    pub accounts: &'settings [ProviderAccount<'settings>],
}

/// One reserved token retaining listener borrows through commit or drop-abort.
/// Configuration spans are copied at preparation; no new long-lived owner exists.
///
/// ```compile_fail
/// use rpt_advanced_standalone::iax::{
///     IaxServer, configuration::{ConfigurationBatch, ListenerConfiguration},
/// };
/// fn cannot_destroy_reserved_listener(mut server: IaxServer) {
///     let batch = ConfigurationBatch::prepare(vec![ListenerConfiguration {
///         listener: &mut server, local_nodes: &[], accounts: &[],
///     }]).unwrap();
///     drop(server);
///     drop(batch);
/// }
/// ```
pub struct ConfigurationBatch<'listener> {
    token: Option<NonNull<c_void>>,
    commit: ConsumeToken,
    abort: ConsumeToken,
    // Dropped only after Drop has consumed the provider token. Each server keeps
    // its original DSO/catalog owner, without another long-lived ownership layer.
    _listeners: Vec<&'listener mut IaxServer>,
}
impl<'listener> ConfigurationBatch<'listener> {
    /// Copy and reserve the entire cohort before any product publication.
    /// All listeners must share their existing retained client/DSO owner.
    pub fn prepare(
        configurations: Vec<ListenerConfiguration<'listener, '_>>,
    ) -> Result<Self, IaxError> {
        let first = configurations.first().ok_or(IaxError::InvalidParameters)?;
        let (Some(prepare), Some(commit), Some(abort)) = (
            first.listener.api.prepare_configuration_batch,
            first.listener.api.commit_configuration_batch,
            first.listener.api.destroy_configuration_batch,
        ) else {
            return Err(IaxError::IncompatibleAdapter);
        };
        for configuration in &configurations {
            let server = &configuration.listener;
            if !Arc::ptr_eq(&first.listener._owner.0, &server._owner.0)
                || server.api.prepare_configuration_batch.is_none()
                || server.api.commit_configuration_batch.is_none()
                || server.api.destroy_configuration_batch.is_none()
            {
                return Err(IaxError::IncompatibleAdapter);
            }
            if configuration
                .local_nodes
                .iter()
                .any(|node| node.is_empty() || !node.bytes().all(|byte| byte.is_ascii_digit()))
            {
                return Err(IaxError::InvalidParameters);
            }
        }
        // Flat temporary storage avoids one pair of extra vectors per listener.
        // Borrowed account strings and these numeric strings remain live until
        // the single synchronous prepare call has copied the entire cohort.
        let addresses: Vec<_> = configurations
            .iter()
            .flat_map(|configuration| configuration.accounts)
            .map(|account| account.remote.to_string())
            .collect();
        let records: Vec<_> = configurations
            .iter()
            .flat_map(|configuration| configuration.accounts)
            .zip(&addresses)
            .map(|(account, address)| accounts::account_record(account, address))
            .collect();
        let nodes: Vec<_> = configurations
            .iter()
            .map(|configuration| configuration.local_nodes.join(","))
            .collect();
        let mut offset = 0;
        let entries: Vec<_> = configurations
            .iter()
            .zip(&nodes)
            .map(|(configuration, nodes)| {
                let start = offset;
                offset += configuration.accounts.len();
                abi::rptadv_iax2_configuration_entry_v1 {
                    struct_size: size_of::<abi::rptadv_iax2_configuration_entry_v1>() as u32,
                    abi_version: 1,
                    listener: configuration.listener.handle.as_ptr(),
                    local_nodes: accounts::bytes(nodes),
                    accounts: records[start..offset].as_ptr(),
                    account_count: configuration.accounts.len(),
                }
            })
            .collect();
        // Reserve exact small borrow storage before the provider reserves anything.
        // No allocation, validation or fallible work follows a successful token.
        let mut listeners = Vec::with_capacity(configurations.len());
        for configuration in configurations {
            listeners.push(configuration.listener);
        }
        let mut token = ptr::null_mut();
        // SAFETY: every span and listener/DSO owner remains live through this call.
        accounts::status(unsafe { prepare(entries.as_ptr(), entries.len(), &mut token) })?;
        let token = NonNull::new(token).ok_or(IaxError::Operation)?;
        Ok(Self {
            token: Some(token),
            commit,
            abort,
            _listeners: listeners,
        })
    }

    /// Consume a valid reservation once, with no fallible late result.
    pub fn commit(mut self) {
        // Preparation creates the only valid token; consuming self prevents reuse.
        let token = self.token.take().expect("prepared configuration token");
        // SAFETY: the unique token is consumed while all listener borrows live.
        unsafe { (self.commit)(token.as_ptr()) };
    }
}
impl Drop for ConfigurationBatch<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            // SAFETY: uncommitted tokens abort once before listener borrows end.
            unsafe { (self.abort)(token.as_ptr()) };
        }
    }
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
