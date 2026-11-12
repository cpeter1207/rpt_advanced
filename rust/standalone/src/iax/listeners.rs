//! Scoped composition of numeric and provider profiles on the existing listener map.
//! Preparation does not poll, authenticate, register or dial a candidate listener.
use super::{
    IaxClient, IaxError, IaxServer,
    accounts::ProviderAccount,
    configuration::{ConfigurationBatch, ListenerConfiguration},
};
use crate::{
    configuration::{PhoneAccountProtocol, ResolvedRadioNode},
    phone_accounts::PreparedAccounts,
};
use rptadv_peer_support::reload::ReloadOutcome;
use std::{collections::BTreeMap, net::SocketAddr};

/// Complete borrowed settings; numeric bindings and account labels stay distinct.
pub(crate) struct ListenerProfile<'a> {
    pub(crate) local_nodes: Vec<String>,
    pub(crate) accounts: Vec<ProviderAccount<'a>>,
}

/// Map prepared IAX accounts to their explicit enabled node's configured port.
pub(crate) fn resolve_profiles<'a>(
    nodes: &[ResolvedRadioNode],
    accounts: &'a PreparedAccounts,
) -> Result<BTreeMap<u16, ListenerProfile<'a>>, IaxError> {
    let by_node: BTreeMap<_, _> = nodes
        .iter()
        .map(|node| (node.node.as_str(), node))
        .collect();
    let mut profiles = BTreeMap::new();
    for node in nodes.iter().filter(|node| node.enabled) {
        if node.iax_port == 0 {
            return Err(IaxError::InvalidParameters);
        }
        if !node.node.is_empty() && node.node.bytes().all(|byte| byte.is_ascii_digit()) {
            profiles
                .entry(node.iax_port)
                .or_insert_with(empty_profile)
                .local_nodes
                .push(node.node.clone());
        }
    }
    for account in accounts.as_slice() {
        let settings = account.settings();
        if !settings.enabled || settings.protocol != PhoneAccountProtocol::Iax {
            continue;
        }
        let node = by_node
            .get(settings.local_node.as_str())
            .ok_or(IaxError::InvalidParameters)?;
        if !node.enabled {
            continue;
        }
        profiles
            .entry(node.iax_port)
            .or_insert_with(empty_profile)
            .accounts
            .push(ProviderAccount {
                account: &settings.id,
                generation: account.generation(),
                local_binding: &settings.local_node,
                remote: account.remote(),
                username: &settings.username,
                secret: account.secret(),
                authentication_timeout_ms: settings.authentication_timeout_ms,
            });
    }
    Ok(profiles)
}

fn empty_profile<'a>() -> ListenerProfile<'a> {
    ListenerProfile {
        local_nodes: Vec::new(),
        accounts: Vec::new(),
    }
}

/// Reserve the entire cohort around the caller's exact product disposition.
///
/// The sole control owner must not poll during this call. Candidate receive tasks
/// only queue NEW traffic; bind/enable do not send or authenticate. The borrowed
/// context is reused sequentially, never retained or lent to a provider callback.
/// Fatal outcomes return all listener owners for fenced orderly shutdown.
pub(crate) fn update<C>(
    listeners: &mut BTreeMap<u16, IaxServer>,
    profiles: &BTreeMap<u16, ListenerProfile<'_>>,
    client: &IaxClient,
    context: &mut C,
    mut enable_added: impl FnMut(&mut C, &mut IaxServer) -> Result<(), IaxError>,
    apply: impl FnOnce(&mut C) -> ReloadOutcome,
) -> Result<ReloadOutcome, IaxError> {
    let mut added = Vec::with_capacity(profiles.len());
    let prepared = (|| {
        for (port, profile) in profiles {
            if listeners.contains_key(port) {
                continue;
            }
            let mut listener = IaxServer::bind_with_accounts(
                client,
                SocketAddr::from(([0, 0, 0, 0], *port)),
                &profile.local_nodes,
                &profile.accounts,
            )?;
            enable_added(context, &mut listener)?;
            // Allocate map ownership before any product publication. No other
            // owner can see the map through this exclusive control borrow.
            listeners.insert(*port, listener);
            added.push(*port);
        }
        Ok(())
    })();
    if let Err(error) = prepared {
        remove_added(listeners, &added);
        return Err(error);
    }
    let result = if listeners.is_empty() {
        Ok(apply(context))
    } else {
        // The borrow/token scope ends before any map removal or shutdown return.
        let entries = listeners
            .iter_mut()
            .map(|(port, listener)| {
                let (local_nodes, accounts) =
                    profiles.get(port).map_or((&[][..], &[][..]), |profile| {
                        (profile.local_nodes.as_slice(), profile.accounts.as_slice())
                    });
                ListenerConfiguration {
                    listener,
                    local_nodes,
                    accounts,
                }
            })
            .collect();
        ConfigurationBatch::prepare(entries).map(|batch| {
            let outcome = apply(context);
            if outcome.published() {
                batch.commit();
            } else {
                // Fatal cleanup discards only uncommitted provider preparation.
                // It does not compensate product state or permit renewed polling.
                drop(batch);
            }
            outcome
        })
    };
    match result {
        Ok(ReloadOutcome::Committed | ReloadOutcome::CommittedWithError) => {
            listeners.retain(|port, _| profiles.contains_key(port));
        }
        Ok(ReloadOutcome::Rejected) | Err(_) => remove_added(listeners, &added),
        Ok(ReloadOutcome::Fatal(_)) => {}
    }
    result
}

fn remove_added(listeners: &mut BTreeMap<u16, IaxServer>, added: &[u16]) {
    for port in added {
        listeners.remove(port);
    }
}

#[cfg(test)]
#[path = "listeners_tests.rs"]
mod tests;
