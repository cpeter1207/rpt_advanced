//! Sized metadata admits phones only with a complete explicit private-call contract.
use super::*;

unsafe extern "C" fn info(
    mode: *mut c_void,
    peer: *const c_void,
    output: *mut abi::rptadv_peer_info_v1,
) -> i32 {
    unsafe { crate::fixture::peer_info(mode, peer, output) };
    let output = unsafe { &mut *output };
    let bytes = |value: &'static [u8]| abi::rptadv_peer_bytes_v1 {
        data: value.as_ptr(),
        length: value.len(),
    };
    output.protocol = 3;
    output.capabilities = 32;
    output.account_generation = 7;
    output.qso_generation = 9;
    output.remote = bytes(b"aabbccddeeff00112233445566778899");
    output.provider_account = bytes(b"provider");
    match mode.addr() {
        0 => (),
        1 => {
            output.protocol = 1;
            output.capabilities = 12;
            output.remote = bytes(b"2000");
            output.struct_size =
                std::mem::offset_of!(abi::rptadv_peer_info_v1, provider_account) as u32;
            // An ordinary prefix record has no readable account tail.
            output.provider_account = abi::rptadv_peer_bytes_v1 {
                data: ptr::without_provenance(1),
                length: usize::MAX,
            };
        }
        2 => {
            output.struct_size =
                std::mem::offset_of!(abi::rptadv_peer_info_v1, provider_account) as u32
        }
        3 => output.struct_size -= 1,
        4 => output.capabilities = 0,
        5 => output.capabilities = 40,
        6 => output.account_generation = 0,
        7 => output.qso_generation = 0,
        8 => output.provider_account.length = 0,
        9 => output.provider_account.length = 65,
        10 => output.provider_account.data = ptr::null(),
        11 => output.provider_account = bytes(&[0xff]),
        12 => output.provider_account = bytes(b"provider/other"),
        13 => output.remote = bytes(b"AABBCCDDEEFF00112233445566778899"),
        14 => output.capabilities = 64,
        _ => unreachable!(),
    }
    0
}

#[test]
fn phone_requires_a_complete_tail_while_node_peers_keep_the_original_prefix() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for mode in 0..=14 {
        let services = host_services(|table| {
            table.context = ptr::without_provenance_mut(mode);
            table.peer_info = Some(info);
        });
        let handle = crate::fixture::peer();
        let observed = unsafe { services.inspect_peer(handle) };
        assert_eq!(observed.is_ok(), mode <= 1, "borrowed mode {mode}");
        let adopted = unsafe { services.peer(handle) };
        if let Ok(peer) = adopted {
            assert!(mode <= 1);
            if mode == 0 {
                assert!(peer.identity().node().is_none());
                assert_eq!(peer.phone_account().unwrap().as_str(), "provider");
            } else {
                assert_eq!(peer.identity().node(), Some("2000"));
                assert!(peer.phone_account().is_none());
            }
        } else {
            assert!(mode > 1);
            unsafe { crate::fixture::release_peer(handle) };
        }
    }
    let services = host_services(|_| {});
    assert!(unsafe { services.inspect_peer(ptr::null()) }.is_err());
}

#[test]
fn phone_local_binding_uses_node_id_semantics_without_relaxing_asl_wire_ids() {
    static LONG: [u8; 63] = [b'a'; 63];
    static TOO_LONG: [u8; 64] = [b'a'; 64];
    unsafe extern "C" fn local_info(
        mode: *mut c_void,
        peer: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        unsafe { info(ptr::null_mut(), peer, output) };
        let output = unsafe { &mut *output };
        let local: &[u8] = match mode.addr() {
            0 | 4 => b"local-repeater",
            1 => &LONG,
            2 => &TOO_LONG,
            3 => b"bad name",
            5 => "répéteur".as_bytes(),
            6 => b"",
            7 => b"bad[node]",
            _ => unreachable!(),
        };
        output.local = abi::rptadv_peer_bytes_v1 {
            data: local.as_ptr(),
            length: local.len(),
        };
        if mode.addr() == 4 {
            output.protocol = 1;
            output.capabilities = 12;
            output.remote = abi::rptadv_peer_bytes_v1 {
                data: b"2000".as_ptr(),
                length: 4,
            };
        }
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (mode, expected) in [true, true, false, false, false, true, false, false]
        .into_iter()
        .enumerate()
    {
        let services = host_services(|table| {
            table.context = ptr::without_provenance_mut(mode);
            table.peer_info = Some(local_info);
        });
        let handle = crate::fixture::peer();
        let observed = unsafe { services.inspect_peer(handle) };
        unsafe { crate::fixture::release_peer(handle) };
        assert_eq!(
            observed.is_ok(),
            expected,
            "phone local binding mode {mode}"
        );
    }
}

#[test]
fn optional_phone_group_requires_the_complete_host11_table() {
    let mut table = unsafe { *crate::fixture::host_descriptor() };
    let phone = offset_of!(abi::rptadv_host_services_v11, phone_provider);
    let radio = offset_of!(abi::rptadv_host_services_v11, station_binding);
    let full = size_of_val(&table);
    assert_eq!((radio, phone, full), (384, 392, 416));
    for truncated in radio..phone + size_of::<*const ()>() {
        table.struct_size = truncated as u32;
        assert!(unsafe { HostServices::open(&table) }.is_err());
    }
    table.struct_size = full as u32;
    table.phone_provider = ptr::null();
    let services = unsafe { HostServices::open(&table) }.unwrap();
    assert!(services.phone_provider().is_err());

    let mut group: abi::rptadv_phone_provider_v2 = unsafe { std::mem::zeroed() };
    group.struct_size = std::mem::size_of_val(&group) as u32;
    group.abi_version = 2;
    table.phone_provider = &group;
    let services = unsafe { HostServices::open(&table) }.unwrap();
    assert!(
        services.phone_provider().is_err(),
        "an empty group cannot stage a phone account"
    );
}

#[test]
fn asl_host_extension_rejects_a_partially_present_provider_pointer() {
    let mut table = unsafe { *crate::fixture::host_descriptor() };
    let start = offset_of!(abi::rptadv_host_services_v11, asl_provider);
    let end = start + size_of::<*const ()>();
    assert_eq!((start, end), (400, 408));
    for available in start..=end {
        table.struct_size = available as u32;
        assert!(
            unsafe { HostServices::open(&table) }.is_err(),
            "truncated host table with {available} readable bytes"
        );
    }
    table.struct_size = size_of_val(&table) as u32;
    table.asl_provider = ptr::null();
    assert!(
        unsafe { HostServices::open(&table) }
            .unwrap()
            .asl_provider()
            .is_err()
    );
}

#[test]
fn managed_echo_pointer_accepts_only_complete_host_extents_and_functions() {
    unsafe extern "C" fn begin(_: *mut c_void, _: u64, _: u64) -> i32 {
        0
    }
    unsafe extern "C" fn row(
        _: *mut c_void,
        _: u64,
        _: *const abi::rptadv_managed_echo_row_v1,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn step(_: *mut c_void, _: u64, _: u32) -> i32 {
        0
    }
    unsafe extern "C" fn shutdown(_: *mut c_void) -> i32 {
        0
    }
    let (peer, _owner) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let mut table = peer.services.0;
    let start = std::mem::offset_of!(abi::rptadv_host_services_v11, managed_echo_provider);
    let end = start + size_of::<*const ()>();
    assert_eq!((start, end), (408, 416));
    unsafe extern "C" fn observe(
        _: *mut c_void,
        _: *const u8,
        _: *const u8,
        _: u64,
        _: *mut abi::rptadv_managed_echo_status_v1,
    ) -> i32 {
        -1
    }
    let complete = || abi::rptadv_managed_echo_provider_v2 {
        struct_size: size_of::<abi::rptadv_managed_echo_provider_v2>() as u32,
        abi_version: 2,
        begin: Some(begin),
        row: Some(row),
        step: Some(step),
        shutdown: Some(shutdown),
        observe: Some(observe),
    };
    let group = complete();
    table.managed_echo_provider = &group;
    for size in start..=end {
        table.struct_size = size as u32;
        let opened = unsafe { HostServices::open(&table) };
        if size == end {
            assert!(opened.unwrap().echo_provider().is_ok());
        } else {
            assert!(opened.is_err(), "partial Echo pointer at extent {size}");
        }
    }
    table.managed_echo_provider = ptr::null();
    assert!(
        unsafe { HostServices::open(&table) }
            .unwrap()
            .echo_provider()
            .is_err()
    );
    for missing in 0..7 {
        let mut group = complete();
        match missing {
            0 => group.begin = None,
            1 => group.row = None,
            2 => group.step = None,
            3 => group.shutdown = None,
            4 => group.struct_size -= 1,
            5 => group.observe = None,
            _ => group.abi_version = 1,
        }
        table.managed_echo_provider = &group;
        assert!(
            unsafe { HostServices::open(&table) }
                .unwrap()
                .echo_provider()
                .is_err(),
            "incomplete Echo group variant {missing}"
        );
    }
}
