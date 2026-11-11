//! Fixtures exercise ABI copying and ownership, never live provider accounts.

use super::*;
use crate::iax::{ServerDescriptor, tests as legacy};
use std::{cell::Cell, ffi::c_void, mem::size_of, ptr::NonNull};

thread_local! {
    static RESULT: Cell<i32> = const { Cell::new(0) };
    static MALFORMED: Cell<u32> = const { Cell::new(0) };
    static DESTROYED: Cell<usize> = const { Cell::new(0) };
}

unsafe fn text<'a>(value: abi::rptadv_iax2_bytes_v1) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(value.data, value.length) }
}

fn endpoint() -> abi::rptadv_iax2_endpoint_v1 {
    abi::rptadv_iax2_endpoint_v1 {
        family: 4,
        port: 4569,
        address: [127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        flowinfo: 0,
        scope_id: 0,
    }
}

unsafe extern "C" fn configure(
    _: *mut c_void,
    records: *const abi::rptadv_iax2_provider_account_v1,
    count: usize,
) -> i32 {
    assert_eq!(count, 1);
    let record = unsafe { &*records };
    assert_eq!(
        record.struct_size as usize,
        size_of::<abi::rptadv_iax2_provider_account_v1>()
    );
    assert_eq!(record.abi_version, 1);
    assert_eq!(record.reserved, 0);
    assert_eq!(record.generation, 7);
    assert_eq!(unsafe { text(record.account) }, b"home-phone");
    assert_eq!(unsafe { text(record.local_binding) }, b"named-node");
    assert_eq!(unsafe { text(record.remote_address) }, b"127.0.0.1:4569");
    assert_eq!(unsafe { text(record.username) }, b"login-name");
    assert_eq!(unsafe { text(record.secret) }, b"literal;secret");
    assert_eq!(record.authentication_timeout_ms, 5000);
    RESULT.get()
}

unsafe extern "C" fn register(
    _: *mut c_void,
    input: *const abi::rptadv_iax2_registration_options_v1,
    output: *mut abi::rptadv_iax2_registration_result_v1,
) -> i32 {
    let request = unsafe { &*input };
    assert_eq!(
        request.struct_size as usize,
        size_of::<abi::rptadv_iax2_registration_options_v1>()
    );
    assert_eq!(request.abi_version, 1);
    assert_eq!(unsafe { text(request.account) }, b"home-phone");
    assert_eq!(request.generation, 7);
    assert_eq!(request.refresh, 60);
    assert_eq!(request.release, 1);
    assert_eq!(request.timeout_ms, 1000);
    let output = unsafe { &mut *output };
    assert_eq!(
        output.struct_size as usize,
        size_of::<abi::rptadv_iax2_registration_result_v1>()
    );
    output.abi_version = 1;
    output.refresh = 45;
    output.local = endpoint();
    output.apparent = abi::rptadv_iax2_endpoint_v1 {
        family: 6,
        port: 4570,
        address: [1; 16],
        flowinfo: 3,
        scope_id: 2,
    };
    match MALFORMED.get() {
        1 => output.struct_size = 0,
        2 => output.abi_version = 2,
        3 => output.refresh = 0,
        4 => output.reserved = 1,
        5 => output.local.family = 0,
        6 => output.local.port = 0,
        7 => output.local.address[4] = 1,
        8 => output.local.flowinfo = 1,
        9 => output.local.scope_id = 1,
        10 => output.apparent.family = 7,
        11 => output.apparent.family = 0,
        12 => output.apparent = unsafe { std::mem::zeroed() },
        _ => {}
    }
    RESULT.get()
}

unsafe extern "C" fn dial(
    _: *mut c_void,
    input: *const abi::rptadv_iax2_provider_dial_options_v1,
    output: *mut *mut c_void,
    receipt: *mut abi::rptadv_iax2_provider_call_receipt_v1,
) -> i32 {
    let request = unsafe { &*input };
    assert_eq!(
        request.struct_size as usize,
        size_of::<abi::rptadv_iax2_provider_dial_options_v1>()
    );
    assert_eq!(request.abi_version, 1);
    assert_eq!(request.reserved, 0);
    assert_eq!(unsafe { text(request.account) }, b"home-phone");
    assert_eq!(request.generation, 7);
    assert_eq!(unsafe { text(request.destination) }, b"00911");
    assert_eq!(unsafe { text(request.caller_number) }, b"524950");
    assert_eq!(unsafe { text(request.caller_name) }, b"Not authenticated");
    assert_eq!(unsafe { text(request.context) }, b"phone-context");
    assert_eq!(request.timeout_ms, 1000);
    let receipt = unsafe { &mut *receipt };
    assert_eq!(
        receipt.struct_size as usize,
        size_of::<abi::rptadv_iax2_provider_call_receipt_v1>()
    );
    receipt.abi_version = 1;
    receipt.account_generation = 7;
    receipt.call_generation = 19;
    receipt.format = 4;
    receipt.local_call = 12;
    receipt.remote_call = 17;
    receipt.local = endpoint();
    receipt.remote = endpoint();
    match MALFORMED.get() {
        1 => receipt.struct_size = 0,
        2 => receipt.abi_version = 2,
        3 => receipt.account_generation = 8,
        4 => receipt.call_generation = 0,
        5 => receipt.format = 0,
        6 => receipt.format = 3,
        7 => receipt.local_call = 0,
        8 => receipt.remote_call = 32768,
        9 => receipt.reserved = 1,
        10 => receipt.local.family = 0,
        11 => receipt.remote.family = 0,
        _ => {}
    }
    if RESULT.get() == 0 && MALFORMED.get() != 12 {
        unsafe { output.write(std::ptr::dangling_mut::<c_void>()) };
    }
    RESULT.get()
}

unsafe extern "C" fn destroy(_: *mut c_void) {
    DESTROYED.set(DESTROYED.get() + 1);
}

unsafe extern "C" fn poll(
    _: *mut c_void,
    now: u32,
    authorize: crate::iax::ServerAuthorize,
    accept: crate::iax::ServerAccept,
    provider_authorize: ProviderAuthorize,
    provider_accept: ProviderAccept,
    context: *mut c_void,
) -> i32 {
    assert_eq!(now, 123);
    assert!(authorize.is_none() && accept.is_none());
    assert!(provider_authorize.is_none() && provider_accept.is_none());
    assert!(context.is_null());
    RESULT.get()
}

fn fixture() -> IaxServer {
    RESULT.set(0);
    MALFORMED.set(0);
    DESTROYED.set(0);
    let mut client = legacy::descriptor();
    client.destroy = Some(destroy);
    let mut api: ServerDescriptor = legacy::server_descriptor();
    api.configure_accounts = Some(configure);
    api.register_account = Some(register);
    api.dial_provider_account = Some(dial);
    api.poll_accounts = Some(poll);
    IaxServer {
        _owner: legacy::fixture_client(client),
        api,
        handle: NonNull::dangling(),
    }
}

fn account() -> ProviderAccount<'static> {
    ProviderAccount {
        account: "home-phone",
        generation: 7,
        local_binding: "named-node",
        remote: "127.0.0.1:4569".parse().unwrap(),
        username: "login-name",
        secret: "literal;secret",
        authentication_timeout_ms: 5000,
    }
}

fn registration() -> RegistrationRequest<'static> {
    RegistrationRequest {
        account: "home-phone",
        generation: 7,
        refresh_seconds: 60,
        release: true,
        timeout_ms: 1000,
    }
}

fn request() -> DialRequest<'static> {
    DialRequest {
        account: "home-phone",
        generation: 7,
        destination: "00911",
        caller_number: "524950",
        caller_name: "Not authenticated",
        context: "phone-context",
        timeout_ms: 1000,
    }
}

#[test]
fn configuration_preserves_distinct_identity_and_literal_credentials() {
    let mut server = fixture();
    assert_eq!(server.configure_accounts(&[account()]), Ok(()));
    RESULT.set(-1);
    assert_eq!(
        server.configure_accounts(&[account()]),
        Err(IaxError::InvalidParameters)
    );
    server.api.configure_accounts = None;
    assert_eq!(
        server.configure_accounts(&[account()]),
        Err(IaxError::IncompatibleAdapter)
    );
}

#[test]
fn registration_copies_granted_interval_and_endpoints() {
    let mut server = fixture();
    let result = server.register_account(&registration()).unwrap();
    assert_eq!(result.refresh_seconds, 45);
    assert_eq!(result.local, "127.0.0.1:4569".parse().unwrap());
    let Some(SocketAddr::V6(apparent)) = result.apparent else {
        panic!("missing IPv6 receipt")
    };
    assert_eq!(apparent.port(), 4570);
    assert_eq!(apparent.flowinfo(), 3);
    assert_eq!(apparent.scope_id(), 2);
    MALFORMED.set(12);
    assert!(
        server
            .register_account(&registration())
            .unwrap()
            .apparent
            .is_none()
    );
}

#[test]
fn registration_rejects_errors_and_malformed_receipts() {
    let mut server = fixture();
    for malformed in 1..=11 {
        MALFORMED.set(malformed);
        assert_eq!(
            server.register_account(&registration()),
            Err(IaxError::Operation),
            "case {malformed}"
        );
    }
    MALFORMED.set(0);
    for (status, error) in [
        (-1, IaxError::InvalidParameters),
        (-2, IaxError::Network),
        (-3, IaxError::Timeout),
        (-4, IaxError::Rejected),
        (-5, IaxError::Codec),
        (-6, IaxError::Protocol),
        (-7, IaxError::Operation),
    ] {
        RESULT.set(status);
        assert_eq!(server.register_account(&registration()), Err(error));
    }
    server.api.register_account = None;
    assert_eq!(
        server.register_account(&registration()),
        Err(IaxError::IncompatibleAdapter)
    );
}

#[test]
fn dialing_preserves_destination_and_returns_one_owned_call() {
    let mut server = fixture();
    let call = server.dial_provider_account(&request()).unwrap();
    assert_eq!(call.receipt.account_generation, 7);
    assert_eq!(call.receipt.call_generation, 19);
    assert_eq!(call.receipt.local_call, 12);
    assert_eq!(call.receipt.remote_call, 17);
    assert_eq!(call.receipt.format, 4);
    assert_eq!(call.receipt.local, "127.0.0.1:4569".parse().unwrap());
    assert_eq!(call.receipt.remote, call.receipt.local);
    assert_eq!(DESTROYED.get(), 0);
    drop(call);
    assert_eq!(DESTROYED.get(), 1);
}

#[test]
fn malformed_dial_receipt_destroys_transferred_peer_exactly_once() {
    let mut server = fixture();
    for malformed in 1..=12 {
        MALFORMED.set(malformed);
        assert!(
            matches!(
                server.dial_provider_account(&request()),
                Err(IaxError::Operation)
            ),
            "case {malformed}"
        );
        assert_eq!(DESTROYED.get(), malformed.min(11) as usize);
    }
    RESULT.set(-1);
    assert!(matches!(
        server.dial_provider_account(&request()),
        Err(IaxError::InvalidParameters)
    ));
    assert_eq!(DESTROYED.get(), 11);
    server.api.dial_provider_account = None;
    assert!(matches!(
        server.dial_provider_account(&request()),
        Err(IaxError::IncompatibleAdapter)
    ));
}

#[test]
fn mixed_poll_uses_single_provider_capable_stream() {
    let mut server = fixture();
    for (result, expected) in [
        (0, Ok(false)),
        (1, Ok(true)),
        (-1, Err(IaxError::Operation)),
    ] {
        RESULT.set(result);
        assert_eq!(
            unsafe { server.poll_accounts(123, None, None, None, None, std::ptr::null_mut()) },
            expected
        );
    }
    server.api.poll_accounts = None;
    assert_eq!(
        unsafe { server.poll_accounts(123, None, None, None, None, std::ptr::null_mut()) },
        Err(IaxError::IncompatibleAdapter)
    );
}

#[test]
fn released_provider_binds_phone_only_without_numeric_aliases() {
    let client = legacy::selected_client(std::path::Path::new("librptadviax2.so.2"), 1);
    let mut server =
        IaxServer::bind_with_accounts(&client, "127.0.0.1:0".parse().unwrap(), &[], &[account()])
            .unwrap();
    assert_eq!(server.configure_accounts(&[account()]), Ok(()));
    assert_eq!(
        unsafe { server.poll_accounts(123, None, None, None, None, std::ptr::null_mut()) },
        Ok(false)
    );
}
