//! Real loopback signaling reaches actual typed host metadata without radio I/O.
use super::*;
use crate::{
    abi,
    iax::{accounts::DialRequest, tests::selected_client},
    providers::{ProductInspection, ProviderSet},
    runtime::ProductRuntime,
    secrets::SecretsFile,
};
use std::{
    cell::RefCell,
    ffi::{c_char, c_void},
    mem::size_of,
    ptr, thread,
    time::Instant,
};

#[derive(Default)]
struct Product {
    host: Option<abi::rptadv_host_services_v10>,
    authorized: usize,
    admitted: usize,
    correct_metadata: bool,
    accepted: Option<usize>,
    authorize_result: i32,
    incoming_result: i32,
    provider_destroyed: usize,
    authorized_handle: usize,
}
thread_local! { static PRODUCT: RefCell<Product> = RefCell::default(); }

unsafe extern "C" fn start(
    host: *const abi::rptadv_host_services_v10,
    _: *const abi::rptadv_control_descriptor_v1,
    _: *const abi::rptadv_file_descriptor,
    _: *const abi::rptadv_speech_descriptor,
    _: *const c_char,
    _: usize,
) -> i32 {
    PRODUCT.with_borrow_mut(|state| state.host = Some(unsafe { *host }));
    0
}
unsafe extern "C" fn reload(_: *const c_char, _: usize) -> i32 {
    0
}
unsafe extern "C" fn stop() -> i32 {
    let owner = PRODUCT
        .with_borrow_mut(|state| state.accepted.take().map(|raw| (state.host.unwrap(), raw)));
    if let Some((host, raw)) = owner {
        unsafe { host.peer_destroy.unwrap()(host.context, raw as *mut c_void) };
    }
    0
}

unsafe fn bytes<'a>(value: abi::rptadv_peer_bytes_v1) -> &'a [u8] {
    if value.length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(value.data, value.length) }
    }
}

unsafe extern "C" fn authorize(
    request: *const abi::rptadv_incoming_peer_v1,
    raw: *mut c_void,
) -> i32 {
    let host = PRODUCT.with_borrow(|state| state.host.unwrap());
    let mut metadata: abi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
    metadata.struct_size = size_of::<abi::rptadv_peer_info_v1>() as u32;
    metadata.abi_version = 1;
    let inspected = unsafe { host.peer_info.unwrap()(host.context, raw, &mut metadata) };
    let request = unsafe { &*request };
    let correct = !raw.is_null()
        && inspected == 0
        && request.protocol == 3
        && metadata.protocol == 3
        && metadata.account_generation != 0
        && metadata.qso_generation != 0
        && unsafe { bytes(metadata.provider_account) } == b"office"
        && unsafe { bytes(metadata.local) } == b"desk"
        && metadata.remote.length == 32
        && unsafe { bytes(request.local) } == unsafe { bytes(metadata.local) }
        && unsafe { bytes(request.remote) } == unsafe { bytes(metadata.remote) }
        && unsafe { bytes(request.source) } == b"127.0.0.1";
    PRODUCT.with_borrow_mut(|state| {
        state.authorized += 1;
        state.correct_metadata = correct;
        state.authorized_handle = raw as usize;
    });
    if correct {
        PRODUCT.with_borrow(|state| state.authorize_result)
    } else {
        -1
    }
}

unsafe extern "C" fn incoming(_: *const abi::rptadv_incoming_peer_v1, raw: *mut c_void) -> i32 {
    let (host, status) = PRODUCT.with_borrow_mut(|state| {
        state.admitted += 1;
        state.correct_metadata &= state.authorized_handle == raw as usize;
        if state.incoming_result == 0 {
            state.accepted = Some(raw as usize);
        }
        (state.host.unwrap(), state.incoming_result)
    });
    if status == -1 {
        unsafe { host.peer_destroy.unwrap()(host.context, raw) };
    }
    status
}

fn runtime(fixture: &Fixture, text: &str) -> ProductRuntime {
    PRODUCT.with_borrow_mut(|state| *state = Product::default());
    let mut providers = ProviderSet::load().unwrap();
    let inspection = ProductInspection::open().unwrap();
    let mut product = *inspection.api();
    product.start = Some(start);
    product.reload = Some(reload);
    product.stop = Some(stop);
    product.authorize_incoming = Some(authorize);
    product.incoming = Some(incoming);
    crate::providers::tests::replace_descriptor(&mut providers, "librptadv_product.so.3", product);
    ProductRuntime::start_with_providers(
        &fixture.path(),
        text,
        resolve_radio_nodes(text).unwrap(),
        SecretsFile::parse("").unwrap(),
        providers,
    )
    .unwrap()
}

#[test]
fn real_incoming_account_uses_actual_private_handle_for_product_authorization() {
    let fixture = Fixture::new();
    let local_reservation = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    let remote_reservation = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let port = local_reservation.local_addr().unwrap().port();
    let remote = remote_reservation.local_addr().unwrap();
    let text = fixture
        .text(port)
        .replace("127.0.0.1:4569", &remote.to_string());
    let radios = resolve_radio_nodes(&text).unwrap();
    let accounts = ForegroundAccounts::default()
        .prepare(&fixture.path(), &text, &radios)
        .unwrap();
    let client = selected_client(Path::new("librptadviax2.so.2"), 1);
    let mut runtime = runtime(&fixture, &text);
    drop(local_reservation);
    let mut listeners = bind_iax_listeners(&client, &radios, &accounts).unwrap();
    enable_iax_listeners(&mut listeners, &runtime).unwrap();
    let remote_client = client.clone();
    drop(remote_reservation);
    // Only the test peer needs a thread so synchronous dial and polling advance together.
    let (release, released) = std::sync::mpsc::channel();
    let dial = thread::spawn(move || {
        let providers = Box::new(ProviderSet::load().unwrap());
        // Match ProductRuntime's retained-box contract: this executor and its
        // listener/call drop before their provider-code owner.
        let provider_ref: &'static ProviderSet = unsafe { &*ptr::from_ref(providers.as_ref()) };
        let executor = provider_ref.iax_executor().unwrap();
        let account = ProviderAccount {
            account: "gateway",
            generation: 1,
            local_binding: "gateway",
            remote: SocketAddr::from(([127, 0, 0, 1], port)),
            username: "fixture-user",
            secret: "fixture-secret",
            authentication_timeout_ms: 3000,
        };
        let mut listener =
            crate::iax::IaxServer::bind_with_accounts(&remote_client, remote, &[], &[account])
                .unwrap();
        listener.enable_async(&executor).unwrap();
        let call = listener.dial_provider_account(&DialRequest {
            account: "gateway",
            generation: 1,
            destination: "fixture",
            caller_number: "524950",
            caller_name: "Untrusted presentation",
            context: "",
            timeout_ms: 3000,
        });
        let accepted = call.is_ok();
        if accepted {
            let _ = released.recv_timeout(Duration::from_secs(5));
        }
        accepted
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dial.is_finished()
        && PRODUCT.with_borrow(|state| state.admitted == 0)
        && Instant::now() < deadline
    {
        poll_iax_listeners(&mut listeners, &runtime, &client, &accounts);
        thread::sleep(Duration::from_millis(1));
    }
    let _ = release.send(());
    let dialed = dial.join().unwrap();
    let (authorized, admitted, correct) =
        PRODUCT.with_borrow(|state| (state.authorized, state.admitted, state.correct_metadata));
    assert_eq!(
        authorized, 1,
        "real provider call must reach product authorization with its live typed handle"
    );
    assert_eq!(admitted, 1);
    assert!(
        correct,
        "caller presentation must not replace private call metadata"
    );
    assert!(dialed);
    runtime.stop().unwrap();
    listeners.clear();
}

unsafe extern "C" fn destroy_fixture_peer(_: *mut c_void) {
    PRODUCT.with_borrow_mut(|state| state.provider_destroyed += 1);
}
unsafe extern "C" fn unused_timed_read(
    _: *mut c_void,
    _: *mut f32,
    _: usize,
    _: *mut u8,
    _: usize,
    _: *mut abi::rptadv_iax2_timed_event_v1,
) -> i32 {
    -1
}

// Ownership-only fixture: advertise the complete private provider contract,
// but fail closed if the fake product unexpectedly tries to use media.
unsafe extern "C" fn unused_timed_send(
    _: *mut c_void,
    _: *const u8,
    _: usize,
    _: u32,
    _: u32,
    _: *const abi::rptadv_iax2_timed_send_v1,
) -> i32 {
    -1
}
unsafe extern "C" fn unused_retire_egress(_: *mut c_void, _: *mut u64) -> i32 {
    -1
}

fn fixture_call(
    account: &crate::phone_accounts::PreparedAccount,
    local: SocketAddr,
) -> crate::iax::accounts::ProviderCall {
    let mut api = crate::iax::tests::descriptor();
    api.destroy = Some(destroy_fixture_peer);
    api.poll_timed = Some(unused_timed_read);
    api.send_prepared_audio_timed = Some(unused_timed_send);
    api.retire_egress = Some(unused_retire_egress);
    crate::iax::accounts::ProviderCall {
        peer: crate::iax::tests::host_fixture_peer(api),
        receipt: crate::iax::accounts::CallReceipt {
            account_generation: account.generation(),
            call_generation: 17,
            format: 4,
            local_call: 11,
            remote_call: 23,
            local,
            remote: account.remote(),
        },
    }
}

#[test]
fn incoming_authorization_and_product_consumption_destroy_each_unique_peer_once() {
    let fixture = Fixture::new();
    let text = fixture.text(4569);
    let radios = resolve_radio_nodes(&text).unwrap();
    let accounts = ForegroundAccounts::default()
        .prepare(&fixture.path(), &text, &radios)
        .unwrap();
    let account = &accounts.as_slice()[0];
    let local = SocketAddr::from(([0, 0, 0, 0], 4569));
    for (authorization, disposition, accepted, admissions, destroyed_before_stop) in [
        (0, 0, true, 1, 0),
        (-1, 0, false, 0, 1),
        (0, -1, false, 1, 1),
        (0, 1, false, 1, 1),
    ] {
        let mut runtime = runtime(&fixture, &text);
        PRODUCT.with_borrow_mut(|state| {
            state.authorize_result = authorization;
            state.incoming_result = disposition;
        });
        assert_eq!(
            runtime
                .incoming_provider(fixture_call(account, local), account, local)
                .is_ok(),
            accepted
        );
        PRODUCT.with_borrow(|state| {
            assert_eq!(state.authorized, 1);
            assert_eq!(state.admitted, admissions);
            assert!(state.correct_metadata);
            assert_eq!(state.provider_destroyed, destroyed_before_stop);
        });
        runtime.stop().unwrap();
        PRODUCT.with_borrow(|state| assert_eq!(state.provider_destroyed, 1));
        assert!(
            runtime
                .incoming_provider(fixture_call(account, local), account, local)
                .is_err()
        );
        PRODUCT.with_borrow(|state| {
            assert_eq!(
                state.authorized, 1,
                "stopped runtime must not invoke stale callbacks"
            );
            assert_eq!(state.provider_destroyed, 2);
        });
    }
}

#[test]
fn provider_metadata_must_match_current_account_listener_and_transport_generation() {
    let fixture = Fixture::new();
    let text = fixture.text(4569);
    let radios = resolve_radio_nodes(&text).unwrap();
    let accounts = ForegroundAccounts::default()
        .prepare(&fixture.path(), &text, &radios)
        .unwrap();
    let mut runtime = runtime(&fixture, &text);
    let mut context = InboundContext {
        runtime: &runtime,
        client: test_client(),
        accounts: &accounts,
        listener: SocketAddr::from(([0, 0, 0, 0], 4569)),
    };
    let span = |text: &str| abi::rptadv_iax2_bytes_v1 {
        data: text.as_ptr(),
        length: text.len(),
    };
    let mut metadata: abi::rptadv_iax2_provider_incoming_v1 = unsafe { std::mem::zeroed() };
    metadata.struct_size = size_of::<abi::rptadv_iax2_provider_incoming_v1>() as u32;
    metadata.abi_version = 1;
    metadata.account = span("office");
    metadata.local_binding = span("desk");
    metadata.username = span("fixture-user");
    metadata.receipt.struct_size = size_of::<abi::rptadv_iax2_provider_call_receipt_v1>() as u32;
    metadata.receipt.abi_version = 1;
    metadata.receipt.account_generation = accounts.as_slice()[0].generation();
    metadata.receipt.call_generation = 17;
    metadata.receipt.format = 4;
    metadata.receipt.local_call = 11;
    metadata.receipt.remote_call = 23;
    metadata.receipt.local.family = 4;
    metadata.receipt.local.port = 4569;
    metadata.receipt.remote.family = 4;
    metadata.receipt.remote.port = 4569;
    metadata.receipt.remote.address[..4].copy_from_slice(&[127, 0, 0, 1]);
    assert!(unsafe { provider_admission(&context, &metadata) }.is_some());
    let changes: [fn(&mut abi::rptadv_iax2_provider_incoming_v1); 6] = [
        |value| value.receipt.account_generation += 1,
        |value| value.receipt.call_generation = 0,
        |value| value.receipt.local.port += 1,
        |value| value.receipt.remote.port += 1,
        |value| {
            value.local_binding = abi::rptadv_iax2_bytes_v1 {
                data: b"other".as_ptr(),
                length: 5,
            }
        },
        |value| {
            value.username = abi::rptadv_iax2_bytes_v1 {
                data: b"other".as_ptr(),
                length: 5,
            }
        },
    ];
    for change in changes {
        let mut stale = metadata;
        change(&mut stale);
        assert!(unsafe { provider_admission(&context, &stale) }.is_none());
    }
    let retired = crate::phone_accounts::PreparedAccounts::default();
    context.accounts = &retired;
    assert!(unsafe { provider_admission(&context, &metadata) }.is_none());
    PRODUCT.with_borrow(|state| {
        assert_eq!(
            state.authorized, 0,
            "metadata alone is never product authorization"
        )
    });
    runtime.stop().unwrap();
}
