//! Exercise the table supplied by an actual frontend against an independent UDP registrar.
#[path = "sip_digest.rs"]
mod digest;
#[path = "sip_registrar.rs"]
pub(super) mod registrar;
use rptadv_private_phone_host::abi;
use std::{
    ffi::c_void,
    mem::size_of,
    net::UdpSocket,
    ptr, thread,
    time::{Duration, Instant},
};

pub(crate) unsafe fn register(context: *mut c_void, raw: *const abi::rptadv_phone_provider_v2) {
    unsafe { while_registered(context, raw, |_, _| {}) }
}

pub(crate) unsafe fn while_registered(
    context: *mut c_void,
    raw: *const abi::rptadv_phone_provider_v2,
    checkpoint: impl FnOnce(&UdpSocket, std::net::SocketAddr),
) {
    assert!(!raw.is_null());
    let group = unsafe { &*raw };
    assert_eq!(group.abi_version, 2);
    assert!(group.struct_size as usize >= size_of::<abi::rptadv_phone_provider_v2>());
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let bytes = |text: &'static [u8]| abi::rptadv_peer_bytes_v1 {
        data: text.as_ptr(),
        length: text.len(),
    };
    let mut account = abi::rptadv_phone_account_v1 {
        struct_size: size_of::<abi::rptadv_phone_account_v1>() as u32,
        abi_version: 1,
        port_uuid: [1; 16],
        account_uuid: [2; 16],
        generation: 7,
        protocol: 1,
        port: u32::from(server.local_addr().unwrap().port()),
        server: bytes(b"127.0.0.1"),
        username: bytes(b"desk"),
        credential: bytes(b"fixture-only"),
        jitter_minimum_ms: 20,
        jitter_initial_ms: 40,
        jitter_maximum_ms: 100,
        reserved: 0,
    };
    let prepare = |rows: &[abi::rptadv_phone_account_v1]| {
        let mut token = ptr::null_mut();
        assert_eq!(
            unsafe { group.prepare.unwrap()(context, rows.as_ptr(), rows.len(), &mut token) },
            0
        );
        assert!(!token.is_null());
        token
    };
    let wait = |token, reverse: bool| {
        let drive = if reverse { group.restore } else { group.apply }.unwrap();
        let limit = Instant::now() + Duration::from_secs(4);
        loop {
            match unsafe { drive(context, token) } {
                0 => break,
                1 => assert!(Instant::now() < limit),
                state => panic!("native transition {state}"),
            }
            thread::sleep(Duration::from_millis(4));
        }
    };
    let observe = || {
        let mut out = abi::rptadv_phone_account_status_v1 {
            struct_size: size_of::<abi::rptadv_phone_account_status_v1>() as u32,
            abi_version: 1,
            generation: 0,
            state: 0,
            reserved: 0,
        };
        let rc = unsafe {
            group.observe.unwrap()(context, [1u8; 16].as_ptr(), [2u8; 16].as_ptr(), 7, &mut out)
        };
        (rc, out.state, out.generation)
    };
    let token = prepare(std::slice::from_ref(&account));
    assert_ne!(observe(), (0, 2, 7));
    assert_eq!(unsafe { group.apply.unwrap()(context, token) }, 1);
    let mut buf = [0; 8192];
    let (first, from) = registrar::receive_register(&server, &mut buf).unwrap();
    server
        .send_to(registrar::response(&first, 401, true).as_bytes(), from)
        .unwrap();
    let (second, source) = registrar::receive_register(&server, &mut buf).unwrap();
    assert_eq!(source, from);
    digest::check(registrar::header(&second, "Authorization"));
    server
        .send_to(registrar::response(&second, 200, false).as_bytes(), source)
        .unwrap();
    wait(token, false);
    assert_eq!(unsafe { group.release.unwrap()(context, token) }, 0);
    assert_eq!(observe(), (0, 2, 7));
    checkpoint(&server, source);
    account.jitter_initial_ms = 85;
    let changed = prepare(std::slice::from_ref(&account));
    wait(changed, false);
    wait(changed, true);
    assert_eq!(unsafe { group.release.unwrap()(context, changed) }, 0);
    assert_eq!(observe(), (0, 2, 7));
    let withdraw = prepare(&[]);
    assert_eq!(unsafe { group.apply.unwrap()(context, withdraw) }, 1);
    let (last, owner) = registrar::receive_register(&server, &mut buf).unwrap();
    assert_eq!(owner, from);
    assert_eq!(registrar::header(&last, "Expires"), "0");
    server
        .send_to(registrar::response(&last, 200, false).as_bytes(), owner)
        .unwrap();
    wait(withdraw, false);
    assert_eq!(unsafe { group.release.unwrap()(context, withdraw) }, 0);
    assert_ne!(observe(), (0, 2, 7));
}
