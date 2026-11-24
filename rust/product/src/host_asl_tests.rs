use super::*;
use crate::{host::PeerOwner, link::session::{PeerSession, tests::{State, peer_with_async}}};
use rpt_advanced_core::{audio::LinkAudioQueue, link::Mode};

// Model the checked native FINISH rule for a retained authority. It can see
// retirement only once the exact real media registration receives stop.
unsafe extern "C" fn prepare(_: *mut c_void, _: u64, _: u64, _: *const abi::rptadv_asl_account_v1,
    _: usize, _: *const abi::rptadv_asl_admission_v1, _: *mut *mut c_void) -> i32 { -1 }
unsafe extern "C" fn step(_: *mut c_void, token: *mut c_void, phase: u32) -> i32 {
    if phase != 4 { return -1; }
    let peer = unsafe { &*token.cast::<std::sync::Mutex<State>>() }.lock().unwrap();
    i32::from(peer.async_stops != 0 && peer.drops == 0)
}
unsafe extern "C" fn observe(_: *mut c_void, _: *const abi::rptadv_asl_identity_v1, _: *mut u32) -> i32 { -1 }
unsafe extern "C" fn originate(_: *mut c_void, _: *const abi::rptadv_asl_identity_v1,
    _: abi::rptadv_peer_bytes_v1, _: abi::rptadv_peer_bytes_v1, _: usize, _: abi::rptadv_current_v1,
    _: *mut c_void, _: *mut *mut c_void) -> i32 { -1 }

#[test]
fn product_finishes_acl_withdrawal_after_the_rejected_peer_detaches() {
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    let native = rpt_advanced_standalone::host_services::HostServicesOwner::new(Vec::new(), None, Default::default()).unwrap();
    let provider = abi::rptadv_asl_provider_v1 { struct_size: size_of::<abi::rptadv_asl_provider_v1>() as u32,
        abi_version: 1, prepare: Some(prepare), step: Some(step), observe: Some(observe), originate: Some(originate) };
    // The independently bound frontend and product both use the same checked C header.
    let mut table = unsafe { ptr::from_ref(native.descriptor()).cast::<abi::rptadv_host_services_v10>().read() };
    table.asl_provider = &provider;
    // The token outlives Host, including unwinding through its shutdown.
    let token_owner;
    let mut host = crate::host::tests::host();
    let services = host.services;
    host.services = unsafe { HostServices::open(&table) }.unwrap();

    let station: ResourceId = "01010101-0101-4101-8101-010101010101".parse().unwrap();
    let account: ResourceId = "02020202-0202-4202-8202-020202020202".parse().unwrap();
    let attempt = NonZeroU64::new(2).unwrap();
    let wire = NonZeroU64::new(716701).unwrap();
    let allowed = "716798";
    let denied = "716799";
    let mut peer_states = Vec::new();
    for remote in [allowed, denied] {
        let (io, state) = peer_with_async(8000, true);
        state.lock().unwrap().no_async_thread = true;
        let (_, ring) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
        let (session, control) = PeerSession::prepare(io, ring, &station.to_string(), remote, true).unwrap();
        assert!(host.peer_io.attach(session).is_ok());
        host.peers.push(PeerOwner { superseded: false, local: station.to_string(), remote: remote.into(),
            mode: Mode::TRANSCEIVE, group: None, announced: false, control });
        peer_states.push(state);
    }
    let old = Account {
        station, account, generation: NonZeroU64::MIN, wire,
        settings: AllStarLinkProfileSettings { node_number: Some(wire), callsign: String::new(), allow_nodes: vec![],
            deny_nodes: vec![], directory_file: String::new(), lookup_method: LinkLookupMethod::File,
            registration_url: None, registration_interval_s: 60, local_port: 4569, status_post_url: None,
            status_post_interval_s: 60 },
        directory: Arc::new(ManagedDirectory { station: *station.uuid().as_bytes(), account: *account.uuid().as_bytes(),
            generation: NonZeroU64::MIN, method: 2, external: String::new(), registered: Default::default() }),
        jitter: JitterTiming { minimum_ms: 20, initial_ms: 40, maximum_ms: 100 }, version: None,
    };
    let mut next = old.clone();
    next.directory = Arc::new((*old.directory).clone());
    next.settings.deny_nodes.push(denied.into());
    host.asl.active.insert(station, old);
    token_owner = Arc::clone(&peer_states[1]);
    host.asl.staged = Some(Stage { attempt, next: Selection::from([(station, next)]),
        affected: BTreeSet::from([station]), token: Some(NonNull::from(token_owner.as_ref()).cast()), started: true,
        applied: true, published: true, reversed: false, irreversible: false, finished: false });
    host.begin_account_release(attempt).unwrap();
    let ready = host.step_asl(attempt, 4).unwrap();
    if !ready {
        // Ordinary product pump performs this same off-callback reap between visits.
        host.peer_io.reap();
        assert!(host.step_asl(attempt, 4).unwrap());
    }
    let counts = peer_states.iter().map(|state| {
        let peer = state.lock().unwrap();
        (peer.async_stops, peer.async_detaches, peer.drops)
    }).collect::<Vec<_>>();
    host.services = services;
    host.asl = Participant::default();
    assert!(host.stop(20));
    assert_eq!(counts, [(0, 0, 0), (1, 1, 1)], "FINISH acknowledged before selective detach and destruction");
}
