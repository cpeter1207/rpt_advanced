//! Inactive cohort composition regressions against the official released IAX DSO.
//! This file is staged before the reviewed typed-outcome dependency is integrated.
use super::{ListenerProfile, resolve_profiles, update};
use crate::{
    configuration::{resolve_phone_accounts, resolve_radio_nodes},
    iax::{IaxClient, IaxError, IaxServer, accounts::ProviderAccount, executor::Executor},
    phone_accounts::{AccountPreparer, PreparedAccounts},
};
use rptadv_peer_support::reload::ReloadOutcome;
use std::{
    cell::Cell,
    collections::BTreeMap,
    ffi::c_void,
    io,
    net::{SocketAddr, UdpSocket},
    path::Path,
    ptr,
    time::{Duration, Instant},
};

#[path = "../../tests/support/cohort_allocations.rs"]
mod allocations;

fn client() -> IaxClient {
    crate::iax::tests::selected_client(Path::new("librptadviax2.so.2"), 1)
}

fn account(id: &'static str, generation: u64) -> ProviderAccount<'static> {
    ProviderAccount {
        account: id,
        generation,
        local_binding: "named-node",
        remote: "127.0.0.1:4569".parse().unwrap(),
        username: id,
        secret: "fixture-secret",
        authentication_timeout_ms: 1000,
    }
}

fn profile(nodes: &[&str], accounts: Vec<ProviderAccount<'static>>) -> ListenerProfile<'static> {
    ListenerProfile {
        local_nodes: nodes.iter().map(|node| (*node).to_owned()).collect(),
        accounts,
    }
}

fn bound(client: &IaxClient, nodes: &[&str], id: &'static str, generation: u64) -> IaxServer {
    IaxServer::bind_with_accounts(
        client,
        "127.0.0.1:0".parse().unwrap(),
        &nodes
            .iter()
            .map(|node| (*node).to_owned())
            .collect::<Vec<_>>(),
        &[account(id, generation)],
    )
    .unwrap()
}

/// A changed credential using the old generation must reject if that exact owner survived.
fn assert_generation(server: &mut IaxServer, id: &'static str, generation: u64) {
    let mut conflict = account(id, generation);
    conflict.secret = "different-secret";
    assert_eq!(
        server.configure_accounts(&[conflict]),
        Err(IaxError::InvalidParameters)
    );
    assert_eq!(
        server.configure_accounts(&[account(id, generation)]),
        Ok(())
    );
}

#[test]
fn terminal_dispositions_consume_the_reserved_cohort_without_guessing_publication() {
    for outcome in [
        ReloadOutcome::Rejected,
        ReloadOutcome::Committed,
        ReloadOutcome::CommittedWithError,
        ReloadOutcome::Fatal(-3),
        ReloadOutcome::Fatal(73),
    ] {
        let client = client();
        let first = bound(&client, &["1000"], "first", 1);
        let raw = first.handle.as_ptr();
        let setter = first.api.set_local_nodes.unwrap();
        let old = bound(&client, &["2000"], "removed", 2);
        let mut listeners = BTreeMap::from([(4569, first), (4570, old)]);
        let profiles = BTreeMap::from([
            (0, profile(&[], vec![account("added", 3)])),
            (4569, profile(&["1001"], vec![account("first", 4)])),
        ]);
        let calls = Cell::new(0);
        let result = update(
            &mut listeners,
            &profiles,
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| {
                calls.set(calls.get() + 1);
                // The foreign handle is retained by the real exclusive batch.
                // A conflicting C mutation proves reservation spans product work.
                assert_ne!(unsafe { setter(raw, b"9999".as_ptr(), 4) }, 0);
                outcome
            },
        );
        assert_eq!(result, Ok(outcome));
        assert_eq!(calls.get(), 1);
        match outcome {
            ReloadOutcome::Rejected => {
                assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569, 4570]);
                assert_generation(listeners.get_mut(&4569).unwrap(), "first", 1);
            }
            ReloadOutcome::Committed | ReloadOutcome::CommittedWithError => {
                assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [0, 4569]);
                assert_generation(listeners.get_mut(&4569).unwrap(), "first", 4);
            }
            ReloadOutcome::Fatal(_) => {
                // The caller keeps this entire map until orderly fenced shutdown.
                assert_eq!(
                    listeners.keys().copied().collect::<Vec<_>>(),
                    [0, 4569, 4570]
                );
                assert_generation(listeners.get_mut(&4569).unwrap(), "first", 1);
            }
        }
        // Every terminal path consumed the token before listener owners are returned.
        assert_eq!(unsafe { setter(raw, b"9999".as_ptr(), 4) }, 0);
    }
}

#[test]
fn last_listener_preparation_failure_preserves_earlier_owners_and_skips_product() {
    let client = client();
    let mut listeners = BTreeMap::from([
        (4569, bound(&client, &["1000"], "first", 1)),
        (4570, bound(&client, &["2000"], "last", 2)),
    ]);
    let mut invalid = account("last", 4);
    invalid.secret = "";
    let profiles = BTreeMap::from([
        (0, profile(&[], vec![account("added", 5)])),
        (4569, profile(&["1001"], vec![account("first", 3)])),
        (4570, profile(&["2001"], vec![invalid])),
    ]);
    let called = Cell::new(false);
    assert_eq!(
        update(
            &mut listeners,
            &profiles,
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| {
                called.set(true);
                ReloadOutcome::Committed
            }
        ),
        Err(IaxError::InvalidParameters)
    );
    assert!(!called.get());
    assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569, 4570]);
    assert_generation(listeners.get_mut(&4569).unwrap(), "first", 1);
    assert_generation(listeners.get_mut(&4570).unwrap(), "last", 2);
}

#[test]
fn added_listener_enable_failure_reclaims_only_candidates_before_product() {
    let client = client();
    let mut listeners = BTreeMap::from([(4569, bound(&client, &["1000"], "first", 1))]);
    let profiles = BTreeMap::from([
        (0, profile(&[], vec![account("added", 2)])),
        (4569, profile(&["1001"], vec![account("first", 3)])),
    ]);
    let called = Cell::new(false);
    let enabled = Cell::new(0);
    assert_eq!(
        update(
            &mut listeners,
            &profiles,
            &client,
            &mut (),
            |_, _| {
                enabled.set(enabled.get() + 1);
                Err(IaxError::Operation)
            },
            |_| {
                called.set(true);
                ReloadOutcome::Committed
            }
        ),
        Err(IaxError::Operation)
    );
    assert_eq!(enabled.get(), 1);
    assert!(!called.get());
    assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569]);
    assert_generation(listeners.get_mut(&4569).unwrap(), "first", 1);
}

#[test]
fn occupied_candidate_port_never_reaches_product_or_changes_existing_accounts() {
    let client = client();
    let occupied = UdpSocket::bind("0.0.0.0:0").unwrap();
    let port = occupied.local_addr().unwrap().port();
    let current_port = if port == 4569 { 4570 } else { 4569 };
    let mut listeners = BTreeMap::from([(current_port, bound(&client, &["1000"], "first", 1))]);
    let profiles = BTreeMap::from([
        (port, profile(&[], vec![account("added", 2)])),
        (current_port, profile(&["1001"], vec![account("first", 3)])),
    ]);
    let called = Cell::new(false);
    assert_eq!(
        update(
            &mut listeners,
            &profiles,
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| {
                called.set(true);
                ReloadOutcome::Committed
            }
        ),
        Err(IaxError::Bind)
    );
    assert!(!called.get());
    assert_eq!(listeners.len(), 1);
    assert_generation(listeners.get_mut(&current_port).unwrap(), "first", 1);
}

#[test]
fn removing_the_last_listener_reserves_an_empty_profile_before_publication() {
    let client = client();
    let first = bound(&client, &["1000"], "first", 1);
    let raw = first.handle.as_ptr();
    let setter = first.api.set_local_nodes.unwrap();
    let mut listeners = BTreeMap::from([(4569, first)]);
    assert_eq!(
        update(
            &mut listeners,
            &BTreeMap::new(),
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| {
                assert_ne!(unsafe { setter(raw, b"1000".as_ptr(), 4) }, 0);
                ReloadOutcome::Committed
            }
        ),
        Ok(ReloadOutcome::Committed)
    );
    assert!(listeners.is_empty());
}

#[test]
fn an_empty_cohort_still_applies_product_once_without_a_provider_reservation() {
    let client = client();
    let calls = Cell::new(0);
    assert_eq!(
        update(
            &mut BTreeMap::new(),
            &BTreeMap::new(),
            &client,
            &mut (),
            |_, _| { panic!("empty cohort enabled a socket") },
            |_| {
                calls.set(calls.get() + 1);
                ReloadOutcome::Committed
            }
        ),
        Ok(ReloadOutcome::Committed)
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn terminal_consumption_and_map_adoption_allocate_nothing_after_product_publication() {
    for outcome in [
        ReloadOutcome::Committed,
        ReloadOutcome::CommittedWithError,
        ReloadOutcome::Fatal(73),
        ReloadOutcome::Rejected,
    ] {
        let client = client();
        let mut listeners = BTreeMap::from([
            (4569, bound(&client, &["1000"], "first", 1)),
            (4570, bound(&client, &["2000"], "removed", 2)),
        ]);
        let profiles = BTreeMap::from([
            (0, profile(&[], vec![account("added", 3)])),
            (4569, profile(&["1001"], vec![account("first", 4)])),
        ]);
        let result = update(
            &mut listeners,
            &profiles,
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| {
                allocations::begin();
                outcome
            },
        );
        let allocations = allocations::end();
        assert_eq!(result, Ok(outcome));
        assert_eq!(allocations, 0, "late allocation for {outcome:?}");
    }
}

/// Build an independent IAX NEW with empty CALLTOKEN, asking for a challenge.
fn new_packet() -> Vec<u8> {
    let mut bytes = vec![0x80, 77, 0, 0, 0, 0, 0, 0, 0, 0, 6, 1];
    for (kind, value) in [
        (6, b"passive".as_slice()),
        (1, b"incoming-extension".as_slice()),
        (9, &[0, 0, 0, 4]),
        (8, &[0, 0, 0, 4]),
        (54, &[]),
    ] {
        bytes.extend_from_slice(&[kind, value.len() as u8]);
        bytes.extend_from_slice(value);
    }
    bytes
}

/// Borrow the actual retained DSO, just as ProviderSet does; no second executor per peer.
fn executor(client: &IaxClient) -> Executor {
    type Descriptor = unsafe extern "C" fn() -> *const c_void;
    let symbol = unsafe {
        client
            .0
            .library
            .as_ref()
            .unwrap()
            .get::<Descriptor>(b"rptadv_iax2_async_descriptor_v1\0")
    }
    .unwrap();
    unsafe { Executor::new(symbol()) }.unwrap()
}

#[test]
fn fully_prepared_async_candidate_is_passive_until_the_only_control_owner_polls() {
    let client = client();
    let executor = executor(&client);
    let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
    remote
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let port_guard = UdpSocket::bind("0.0.0.0:0").unwrap();
    let port = port_guard.local_addr().unwrap().port();
    drop(port_guard);
    let local = SocketAddr::from(([127, 0, 0, 1], port));
    let mut configured = account("passive", 1);
    configured.remote = remote.local_addr().unwrap();
    let profiles = BTreeMap::from([(port, profile(&[], vec![configured]))]);
    let mut listeners = BTreeMap::new();
    let packet = new_packet();
    update(
        &mut listeners,
        &profiles,
        &client,
        &mut (),
        |_, listener| listener.enable_async(&executor),
        |_| {
            remote.send_to(&packet, local).unwrap();
            let error = remote.recv_from(&mut [0; 2048]).unwrap_err();
            assert!(matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ));
            ReloadOutcome::Committed
        },
    )
    .unwrap();
    // Positive control: the exact queued valid packet produces CALLTOKEN only
    // after the foreground owner explicitly polls the committed listener.
    remote.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = [0; 2048];
    loop {
        unsafe {
            listeners.get_mut(&port).unwrap().poll_accounts(
                100,
                None,
                None,
                None,
                None,
                ptr::null_mut(),
            )
        }
        .unwrap();
        match remote.recv_from(&mut bytes) {
            Ok((length, source)) => {
                assert_eq!(source.port(), port);
                assert!(length >= 12);
                assert_eq!((bytes[10], bytes[11]), (6, 40));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "valid NEW was not retained for post-commit poll"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("loopback receive failed: {error}"),
        }
    }
    // Receiver joins before releasing the original executor and its DSO.
    drop(listeners);
    drop(executor);
    drop(client);
}

#[cfg(unix)]
#[test]
fn profiles_follow_explicit_node_ports_without_numeric_phone_aliases_or_sip_leakage() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "rptadv-iax-cohort-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let secret = directory.join("fixture.secret");
    fs::write(&secret, "literal;#secret\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let source = "\
[1000]
iax_local_port=4569
[north]
iax_local_port=4570
[3000]
node_enabled=no
iax_local_port=4580
[phone_account mixed]
local_node=1000
enabled=yes
server=127.0.0.1:4569
username=mixed
secrets_file=fixture.secret
[phone_account named]
local_node=north
enabled=yes
server=127.0.0.1:4569
username=named
secrets_file=fixture.secret
[phone_account disabled]
local_node=1000
enabled=no
server=127.0.0.1:4569
username=disabled
secrets_file=missing.secret
[phone_account sip]
local_node=1000
enabled=yes
protocol=sip
server=127.0.0.1:5060
username=sip
secrets_file=fixture.secret
[phone_account disabled_node]
local_node=3000
enabled=yes
server=127.0.0.1:4569
username=disabled_node
secrets_file=fixture.secret
";
    let radios = resolve_radio_nodes(source).unwrap();
    let records = resolve_phone_accounts(source).unwrap();
    let prepared = AccountPreparer::default()
        .prepare(
            &directory.join("rpt_advanced.conf"),
            records,
            &PreparedAccounts::default(),
            |_| panic!("numeric fixture used DNS"),
        )
        .unwrap();
    let profiles = resolve_profiles(&radios, &prepared).unwrap();
    assert_eq!(profiles.keys().copied().collect::<Vec<_>>(), [4569, 4570]);
    assert_eq!(profiles[&4569].local_nodes, ["1000"]);
    assert!(profiles[&4570].local_nodes.is_empty());
    assert_eq!(profiles[&4569].accounts.len(), 1);
    assert_eq!(profiles[&4570].accounts.len(), 1);
    let mixed = &profiles[&4569].accounts[0];
    let named = &profiles[&4570].accounts[0];
    assert_eq!(
        (mixed.account, mixed.local_binding, mixed.secret),
        ("mixed", "1000", "literal;#secret")
    );
    assert_eq!((named.account, named.local_binding), ("named", "north"));
    assert_ne!(mixed.generation, 0);
    assert_ne!(named.generation, 0);
}

struct NumericCapture {
    client: IaxClient,
    peer: Option<crate::iax::IaxPeer>,
    exact_identity: bool,
}

unsafe extern "C" fn numeric_authorize(
    _: *mut c_void,
    local: *const u8,
    local_length: usize,
    remote: *const u8,
    remote_length: usize,
    _: *const u8,
    _: usize,
) -> i32 {
    let local = unsafe { std::slice::from_raw_parts(local, local_length) };
    let remote = unsafe { std::slice::from_raw_parts(remote, remote_length) };
    i32::from(local != b"1000" || remote != b"2000")
}

unsafe extern "C" fn numeric_accept(
    context: *mut c_void,
    local: *const u8,
    local_length: usize,
    remote: *const u8,
    remote_length: usize,
    _: *const u8,
    _: usize,
    peer: *mut c_void,
) -> i32 {
    let capture = unsafe { &mut *context.cast::<NumericCapture>() };
    let local = unsafe { std::slice::from_raw_parts(local, local_length) };
    let remote = unsafe { std::slice::from_raw_parts(remote, remote_length) };
    capture.exact_identity = local == b"1000" && remote == b"2000";
    match unsafe { capture.client.clone().adopt_inbound(peer) } {
        Ok(peer) => {
            capture.peer = Some(peer);
            0
        }
        Err(_) => -1,
    }
}

#[test]
fn rejected_mixed_reload_keeps_the_existing_numeric_route_and_peer_usable() {
    let client = client();
    let executor = executor(&client);
    let port_guard = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = port_guard.local_addr().unwrap();
    drop(port_guard);
    let mut server = IaxServer::bind_with_accounts(
        &client,
        address,
        &["1000".to_owned()],
        &[account("first", 1)],
    )
    .unwrap();
    server.enable_async(&executor).unwrap();
    let mut listeners = BTreeMap::from([(address.port(), server)]);
    let outgoing_client = client.clone();
    let (send, sent) = std::sync::mpsc::sync_channel::<()>(1);
    let (finish, finished) = std::sync::mpsc::sync_channel::<()>(1);
    // Only the independent loopback caller uses a test thread, never production.
    let outgoing = std::thread::spawn(move || {
        let mut peer = outgoing_client
            .dial(address, "2000", "1000", "", 2000)
            .unwrap();
        if sent.recv_timeout(Duration::from_secs(3)).is_err() {
            return;
        }
        peer.send_digit(b'5').unwrap();
        let _ = finished.recv_timeout(Duration::from_secs(3));
    });
    let mut capture = NumericCapture {
        client: client.clone(),
        peer: None,
        exact_identity: false,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while capture.peer.is_none() {
        unsafe {
            listeners.get_mut(&address.port()).unwrap().poll_accounts(
                100,
                Some(numeric_authorize),
                Some(numeric_accept),
                None,
                None,
                ptr::from_mut(&mut capture).cast(),
            )
        }
        .unwrap();
        assert!(
            Instant::now() < deadline,
            "numeric loopback call was not admitted"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(capture.exact_identity);
    let profiles = BTreeMap::from([(
        address.port(),
        profile(&["1001"], vec![account("first", 2)]),
    )]);
    assert_eq!(
        update(
            &mut listeners,
            &profiles,
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| ReloadOutcome::Rejected
        ),
        Ok(ReloadOutcome::Rejected)
    );
    assert_generation(listeners.get_mut(&address.port()).unwrap(), "first", 1);
    send.send(()).unwrap();
    let peer = capture.peer.as_mut().unwrap();
    let mut pcm = [0.0; 160];
    let mut text = [0; 1024];
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match peer.poll(&mut pcm, &mut text).unwrap() {
            crate::iax::IaxEvent::Digit(b'5') => break,
            crate::iax::IaxEvent::Hangup => {
                panic!("rejected reload retired the original numeric call")
            }
            _ => {
                assert!(
                    Instant::now() < deadline,
                    "original route no longer delivered its digit"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    finish.send(()).unwrap();
    outgoing.join().unwrap();
    drop(capture);
    drop(listeners);
    drop(executor);
}

/// Public node records are copied from the product ABI without normalizing these
/// fields. Malformed external records must not open port zero or create an alias.
#[test]
fn listener_profiles_reject_zero_ports_and_ignore_empty_numeric_identities() {
    let mut nodes = resolve_radio_nodes("[1000]\niax_local_port=4569\n").unwrap();
    let accounts = PreparedAccounts::default();
    nodes[0].iax_port = 0;
    assert!(matches!(
        resolve_profiles(&nodes, &accounts),
        Err(IaxError::InvalidParameters)
    ));
    nodes[0].iax_port = 4569;
    nodes[0].node.clear();
    assert!(resolve_profiles(&nodes, &accounts).unwrap().is_empty());
}
