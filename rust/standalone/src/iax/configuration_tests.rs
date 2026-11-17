//! ABI ownership fixtures plus the official released provider on loopback only.
use super::*;
use crate::{
    abi,
    iax::{IaxClient, tests as legacy},
};
use std::{
    cell::Cell,
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
};

thread_local! {
    static PREPARES: Cell<usize> = const { Cell::new(0) };
    static COMMITS: Cell<usize> = const { Cell::new(0) };
    static ABORTS: Cell<usize> = const { Cell::new(0) };
    static STATUS: Cell<i32> = const { Cell::new(0) };
    static NULL_TOKEN: Cell<bool> = const { Cell::new(false) };
    static EMPTY_PROFILE: Cell<bool> = const { Cell::new(false) };
}
unsafe fn bytes<'a>(value: abi::rptadv_iax2_bytes_v1) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(value.data, value.length) }
}
unsafe extern "C" fn prepare(
    entries: *const abi::rptadv_iax2_configuration_entry_v1,
    count: usize,
    output: *mut *mut c_void,
) -> i32 {
    PREPARES.set(PREPARES.get() + 1);
    assert_eq!(count, 2);
    let entries = unsafe { std::slice::from_raw_parts(entries, count) };
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.struct_size as usize,
            size_of::<abi::rptadv_iax2_configuration_entry_v1>()
        );
        assert_eq!(entry.abi_version, 1);
        assert_eq!(entry.listener.addr(), index + 1);
        if EMPTY_PROFILE.get() {
            assert!(unsafe { bytes(entry.local_nodes) }.is_empty());
            assert_eq!(entry.account_count, 0);
            continue;
        }
        assert_eq!(
            unsafe { bytes(entry.local_nodes) },
            if index == 0 {
                b"524950,524951".as_slice()
            } else {
                b"".as_slice()
            }
        );
        assert_eq!(entry.account_count, 1);
        let record = unsafe { &*entry.accounts };
        assert_eq!(
            record.struct_size as usize,
            size_of::<abi::rptadv_iax2_provider_account_v1>()
        );
        assert_eq!(record.abi_version, 1);
        assert_eq!(record.reserved, 0);
        assert_eq!(record.generation, 7 + index as u64);
        assert_eq!(unsafe { bytes(record.account) }, b"provider-label");
        assert_eq!(unsafe { bytes(record.local_binding) }, b"named-node");
        assert_eq!(unsafe { bytes(record.remote_address) }, b"[::1]:4569");
        assert_eq!(unsafe { bytes(record.username) }, b"provider-login");
        assert_eq!(unsafe { bytes(record.secret) }, b"literal;#secret");
        assert_eq!(record.authentication_timeout_ms, 1000);
    }
    unsafe { output.write(ptr::null_mut()) };
    if STATUS.get() == 0 && !NULL_TOKEN.get() {
        unsafe { output.write(Box::into_raw(Box::new(42_u8)).cast()) };
    }
    STATUS.get()
}
unsafe extern "C" fn commit(token: *mut c_void) {
    assert_eq!(unsafe { *Box::from_raw(token.cast::<u8>()) }, 42);
    COMMITS.set(COMMITS.get() + 1);
}
unsafe extern "C" fn abort(token: *mut c_void) {
    assert_eq!(unsafe { *Box::from_raw(token.cast::<u8>()) }, 42);
    ABORTS.set(ABORTS.get() + 1);
}
unsafe extern "C" fn destroy(_: *mut c_void) {}
fn account(generation: u64) -> ProviderAccount<'static> {
    ProviderAccount {
        account: "provider-label",
        generation,
        local_binding: "named-node",
        remote: "[::1]:4569".parse().unwrap(),
        username: "provider-login",
        secret: "literal;#secret",
        authentication_timeout_ms: 1000,
    }
}
fn fixture(client: &IaxClient, handle: usize) -> IaxServer {
    let mut api = legacy::server_descriptor();
    api.destroy = Some(destroy);
    api.prepare_configuration_batch = Some(prepare);
    api.commit_configuration_batch = Some(commit);
    api.destroy_configuration_batch = Some(abort);
    IaxServer {
        _owner: client.clone(),
        api,
        handle: NonNull::new(ptr::without_provenance_mut(handle)).unwrap(),
    }
}
fn fixtures() -> [IaxServer; 2] {
    PREPARES.set(0);
    COMMITS.set(0);
    ABORTS.set(0);
    STATUS.set(0);
    NULL_TOKEN.set(false);
    EMPTY_PROFILE.set(false);
    let client = legacy::fixture_client(legacy::descriptor());
    [fixture(&client, 1), fixture(&client, 2)]
}
fn prepare_fixture<'a>(
    first: &'a mut IaxServer,
    second: &'a mut IaxServer,
) -> Result<ConfigurationBatch<'a>, IaxError> {
    // These settings end at return. The token may retain listeners, not spans.
    let nodes = vec!["524950".into(), "524951".into()];
    let first_accounts = [account(7)];
    let second_accounts = [account(8)];
    ConfigurationBatch::prepare(vec![
        ListenerConfiguration {
            listener: first,
            local_nodes: &nodes,
            accounts: &first_accounts,
        },
        ListenerConfiguration {
            listener: second,
            local_nodes: &[],
            accounts: &second_accounts,
        },
    ])
}
#[test]
fn reserved_batch_copies_numeric_and_account_spans_then_commits_exactly_once() {
    let [mut first, mut second] = fixtures();
    let batch = prepare_fixture(&mut first, &mut second).unwrap();
    assert_eq!(PREPARES.get(), 1);
    assert_eq!((COMMITS.get(), ABORTS.get()), (0, 0));
    batch.commit();
    assert_eq!((COMMITS.get(), ABORTS.get()), (1, 0));
}
#[test]
fn dropping_uncommitted_batch_aborts_exactly_once() {
    let [mut first, mut second] = fixtures();
    let batch = prepare_fixture(&mut first, &mut second).unwrap();
    drop(batch);
    assert_eq!((PREPARES.get(), COMMITS.get(), ABORTS.get()), (1, 0, 1));
}
#[test]
fn preparation_failure_or_null_success_never_creates_a_consumable_token() {
    for (status, null, expected) in [
        (-1, false, IaxError::InvalidParameters),
        (-2, false, IaxError::Network),
        (-7, false, IaxError::Operation),
        (0, true, IaxError::Operation),
    ] {
        let [mut first, mut second] = fixtures();
        STATUS.set(status);
        NULL_TOKEN.set(null);
        assert_eq!(
            prepare_fixture(&mut first, &mut second).err(),
            Some(expected)
        );
        assert_eq!((PREPARES.get(), COMMITS.get(), ABORTS.get()), (1, 0, 0));
    }
}
#[test]
fn batch_requires_all_transaction_slots_on_every_listener_before_calling_provider() {
    for listener in 0..2 {
        for slot in 0..3 {
            let mut servers = fixtures();
            match slot {
                0 => servers[listener].api.prepare_configuration_batch = None,
                1 => servers[listener].api.commit_configuration_batch = None,
                _ => servers[listener].api.destroy_configuration_batch = None,
            }
            let [mut first, mut second] = servers;
            assert_eq!(
                prepare_fixture(&mut first, &mut second).err(),
                Some(IaxError::IncompatibleAdapter)
            );
            assert_eq!((PREPARES.get(), COMMITS.get(), ABORTS.get()), (0, 0, 0));
        }
    }
}
#[test]
fn empty_batches_and_mixed_provider_owners_reject_before_passing_opaque_handles() {
    assert_eq!(
        ConfigurationBatch::prepare(Vec::new()).err(),
        Some(IaxError::InvalidParameters)
    );
    let [mut first, _] = fixtures();
    let other_client = legacy::fixture_client(legacy::descriptor());
    let mut second = fixture(&other_client, 2);
    assert_eq!(
        prepare_fixture(&mut first, &mut second).err(),
        Some(IaxError::IncompatibleAdapter)
    );
    assert_eq!(PREPARES.get(), 0);
}
#[test]
fn released_batch_failure_abort_and_commit_keep_exact_account_generation_rules() {
    let client = legacy::selected_client(std::path::Path::new("librptadviax2.so.2"), 1);
    let mut first = IaxServer::bind_with_accounts(
        &client,
        "127.0.0.1:0".parse().unwrap(),
        &["524950".into()],
        &[account(7)],
    )
    .unwrap();
    let mut second = IaxServer::bind_with_accounts(
        &client,
        "127.0.0.1:0".parse().unwrap(),
        &["524951".into()],
        &[account(7)],
    )
    .unwrap();
    let next = [account(8)];
    let invalid = [account(0)];
    let result = ConfigurationBatch::prepare(vec![
        ListenerConfiguration {
            listener: &mut first,
            local_nodes: &[],
            accounts: &next,
        },
        ListenerConfiguration {
            listener: &mut second,
            local_nodes: &[],
            accounts: &invalid,
        },
    ]);
    assert_eq!(result.err(), Some(IaxError::InvalidParameters));
    for server in [&mut first, &mut second] {
        let mut conflict = account(7);
        conflict.secret = "changed-secret";
        assert_eq!(
            server.configure_accounts(&[conflict]),
            Err(IaxError::InvalidParameters)
        );
    }
    let raw = first.handle.as_ptr();
    let setter = first.api.set_local_nodes.unwrap();
    let batch = ConfigurationBatch::prepare(vec![
        ListenerConfiguration {
            listener: &mut first,
            local_nodes: &[],
            accounts: &next,
        },
        ListenerConfiguration {
            listener: &mut second,
            local_nodes: &[],
            accounts: &next,
        },
    ])
    .unwrap();
    // Token retains the listener while this deliberate conflicting C entry runs.
    assert_ne!(unsafe { setter(raw, b"524999".as_ptr(), 6) }, 0);
    drop(batch);
    first.set_local_nodes(&["524950".into()]).unwrap();
    let mut old_conflict = account(7);
    old_conflict.username = "changed-login";
    assert_eq!(
        first.configure_accounts(&[old_conflict]),
        Err(IaxError::InvalidParameters)
    );
    ConfigurationBatch::prepare(vec![
        ListenerConfiguration {
            listener: &mut first,
            local_nodes: &[],
            accounts: &next,
        },
        ListenerConfiguration {
            listener: &mut second,
            local_nodes: &[],
            accounts: &next,
        },
    ])
    .unwrap()
    .commit();
    for server in [&mut first, &mut second] {
        let mut conflict = account(8);
        conflict.secret = "changed-secret";
        assert_eq!(
            server.configure_accounts(&[conflict]),
            Err(IaxError::InvalidParameters)
        );
        assert_eq!(server.configure_accounts(&[account(8)]), Ok(()));
    }
}

#[test]
fn empty_replacement_profiles_can_reserve_disabled_admission() {
    let [mut first, mut second] = fixtures();
    EMPTY_PROFILE.set(true);
    let batch = ConfigurationBatch::prepare(vec![
        ListenerConfiguration {
            listener: &mut first,
            local_nodes: &[],
            accounts: &[],
        },
        ListenerConfiguration {
            listener: &mut second,
            local_nodes: &[],
            accounts: &[],
        },
    ])
    .unwrap();
    batch.commit();
    assert_eq!((PREPARES.get(), COMMITS.get(), ABORTS.get()), (1, 1, 0));
}

#[test]
fn numeric_identity_elements_cannot_smuggle_delimiters_or_non_decimal_aliases() {
    for invalid in ["", "named-node", "524950,524951", "524950\n"] {
        let [mut first, _] = fixtures();
        assert_eq!(
            ConfigurationBatch::prepare(vec![ListenerConfiguration {
                listener: &mut first,
                local_nodes: &[invalid.into()],
                accounts: &[],
            },])
            .err(),
            Some(IaxError::InvalidParameters)
        );
        assert_eq!(PREPARES.get(), 0);
    }
}
