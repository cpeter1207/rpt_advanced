//! Sized metadata admits phones only with a complete explicit private-call contract.
use super::*;

pub(super) unsafe extern "C" fn info(
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
fn private_inband_capability_requires_the_complete_timed_phone_profile() {
    unsafe extern "C" fn mode_info(
        context: *mut c_void,
        peer: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        unsafe { info(ptr::null_mut(), peer, output) };
        unsafe { (*output).capabilities = context.addr() as u32 };
        0
    }
    let (source, _state) = crate::link::session::tests::phone_source_peer();
    for (capabilities, expected) in [
        (51, true),
        (115, true),
        (64, false),
        (96, false),
        (99, false),
        (114, false),
        (119, false),
        (83, false),
    ] {
        let services = host_services(|table| {
            *table = source.services.0;
            table.context = ptr::without_provenance_mut(capabilities);
            table.peer_info = Some(mode_info);
        });
        let handle = crate::fixture::peer();
        let result = unsafe { services.inspect_peer(handle) };
        unsafe { crate::fixture::release_peer(handle) };
        assert_eq!(result.is_ok(), expected, "capability mask {capabilities}");
    }
}
