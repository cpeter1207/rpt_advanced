//! Verify the real frontend account transaction, generation and peer ownership.
use super::host_abi;
use rptadv_peer_support::echolink::abi as echo;
use std::{ffi::c_void, mem::size_of, ptr};

pub(crate) fn check(host: &host_abi::rptadv_host_services_v11, wire: u32) {
    let group = unsafe { host.managed_echo_provider.as_ref() }.unwrap();
    let phase = |attempt, phase| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let code = unsafe { group.step.unwrap()(host.context, attempt, phase) };
            if code != 1 {
                break code;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    };
    let stage = |attempt, wire, role| {
        let config = echo::AccountConfig {
            enabled: 1,
            node_number: wire,
            local_node: echo::ByteSpan {
                data: b"524950".as_ptr(),
                length: 6,
            },
            ..Default::default()
        };
        let mut row: host_abi::rptadv_managed_echo_row_v1 = unsafe { std::mem::zeroed() };
        row.struct_size = size_of::<host_abi::rptadv_managed_echo_row_v1>() as u32;
        row.abi_version = 1;
        row.station_uuid = [1; 16];
        row.account_uuid = [2; 16];
        row.credential_version = [3; 16];
        row.local_label = 524950;
        row.settings = ptr::from_ref(&config).cast();
        row.minimum_ms = 20;
        row.initial_ms = 40;
        row.maximum_ms = 100;
        row.role = role;
        row.directory_password = host_abi::rptadv_peer_bytes_v1 {
            data: b"fixture".as_ptr(),
            length: 7,
        };
        unsafe { group.row.unwrap()(host.context, attempt, &row) }
    };
    let observe = || {
        let mut o: host_abi::rptadv_managed_echo_status_v1 = unsafe { std::mem::zeroed() };
        o.struct_size = size_of::<host_abi::rptadv_managed_echo_status_v1>() as u32;
        o.abi_version = 1;
        let result = unsafe {
            group.observe.unwrap()(
                host.context,
                [1; 16].as_ptr(),
                [2; 16].as_ptr(),
                524950,
                &mut o,
            )
        };
        (result, o.generation)
    };
    assert_eq!(unsafe { group.begin.unwrap()(host.context, 0, 1) }, 0);
    assert_eq!(stage(1, 42, 1), 0);
    assert_eq!(phase(1, 1), -1);
    assert_eq!(phase(1, 3), 0);
    assert_eq!(phase(1, 4), 0);
    assert_eq!(observe().0, -1);

    assert_eq!(unsafe { group.begin.unwrap()(host.context, 0, 2) }, 0);
    assert_eq!(stage(2, wire, 1), 0);
    for step in [1, 2, 4] {
        assert_eq!(phase(2, step), 0);
    }
    assert_eq!(observe(), (0, 4));
    // The selected station can really dial the native fixture after publication.
    unsafe extern "C" fn current(_: *mut c_void) -> u32 {
        1
    }
    let local = c"01010101-0101-0101-0101-010101010101";
    unsafe extern "C" fn credit(context: *mut c_void) -> u32 {
        let left = unsafe { &*context.cast::<std::sync::atomic::AtomicUsize>() };
        u32::from(
            left.fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |n| n.checked_sub(1),
            )
            .is_ok(),
        )
    }
    for count in [0, 1] {
        let value = std::sync::atomic::AtomicUsize::new(count);
        let mut peer = ptr::null_mut();
        let result = unsafe {
            host.peer_dial.unwrap()(
                host.context,
                c"EchoLink/4294967295".as_ptr(),
                19,
                local.as_ptr(),
                36,
                960,
                Some(credit),
                ptr::from_ref(&value).cast_mut().cast(),
                &mut peer,
            )
        };
        assert_eq!(result, -1);
        assert!(peer.is_null());
    }
    let mut peer = ptr::null_mut();
    assert_eq!(
        unsafe {
            host.peer_dial.unwrap()(
                host.context,
                c"EchoLink/4294967295".as_ptr(),
                19,
                local.as_ptr(),
                36,
                960,
                Some(current),
                ptr::null_mut(),
                &mut peer,
            )
        },
        0
    );
    assert!(!peer.is_null());
    unsafe { host.peer_destroy.unwrap()(host.context, peer) };
}
