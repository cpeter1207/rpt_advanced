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
        (307, true), // finite negotiated packets without inband digits
        (371, true), // complete phone group plus both optional promises
        (256, false),
        (306, false),
        (291, false),
        (563, true), // native RX and in-band TX
        (819, true), // finite packets plus TX-only synthesis
        (627, true), // redundant in-band bits preserve existing RX behavior
        (883, true), // complete group with both in-band bits and finite packets
        (512, false),
        (562, false),
        (547, false),
        (1075, false), // next unknown capability remains rejected
        (64, false),
        (96, false),
        (99, false),
        (114, false),
        (119, false),
        (83, false),
        (179, false),
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

#[test]
fn timed_g711_dial_tail_preserves_the_old_complete_prefix_and_rejects_partial_or_incomplete_hosts()
{
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    let prefix = std::mem::offset_of!(abi::rptadv_host_services_v10, peer_dial_timed_g711);
    table.struct_size = prefix as u32;
    assert!(
        unsafe { HostServices::open(&table) }.is_ok(),
        "previous complete host table remains valid"
    );
    table.struct_size += 1;
    assert!(
        unsafe { HostServices::open(&table) }.is_err(),
        "partial dial tail is not readable"
    );
    table.struct_size = std::mem::size_of_val(&table) as u32;
    table.peer_dial_timed_g711 = table.peer_dial;
    assert!(
        unsafe { HostServices::open(&table) }.is_err(),
        "timed dial requires timed async prepared callbacks"
    );
}

#[test]
fn timed_g711_capability_requires_an_ordinary_async_prepared_peer() {
    let (peer, state) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let (phone, _) = crate::link::session::tests::phone_source_peer();
    let mut services = peer.services;
    services.0.peer_send_prepared_timed = phone.services.0.peer_send_prepared_timed;
    for (capabilities, expected) in [
        (143, true),
        (128, false),
        (142, false),
        (141, false),
        (179, false),
        (151, false),
    ] {
        state.lock().unwrap().capabilities = capabilities;
        let result = unsafe { services.inspect_peer(peer.handle.as_ptr()) };
        assert_eq!(
            result.is_ok(),
            expected,
            "timed route capabilities {capabilities}"
        );
    }
}

#[test]
fn timed_g711_dial_rejects_missing_tail_or_wrong_returned_capability_without_leaking() {
    unsafe extern "C" fn dial(
        context: *mut c_void,
        _: *const c_char,
        _: usize,
        _: *const c_char,
        _: usize,
        _: usize,
        current: abi::rptadv_current_v1,
        current_context: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        if current.is_none_or(|check| unsafe { check(current_context) } == 0) {
            return -1;
        }
        let state = unsafe {
            &*context.cast::<std::sync::Arc<std::sync::Mutex<crate::link::session::tests::State>>>()
        };
        unsafe {
            *output = Box::into_raw(Box::new(state.clone())).cast();
        }
        0
    }
    let (peer, state) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let (phone, _) = crate::link::session::tests::phone_source_peer();
    let mut table = peer.services.0;
    table.context = ptr::from_ref(&state).cast_mut().cast();
    table.peer_send_prepared_timed = phone.services.0.peer_send_prepared_timed;
    let absent = unsafe { HostServices::open(&table) }.unwrap();
    assert!(
        absent
            .dial_timed_g711("radio@127.0.0.1/2000", "1000", 960, || true)
            .is_err()
    );
    table.peer_dial_timed_g711 = Some(dial);
    let services = unsafe { HostServices::open(&table) }.unwrap();
    assert!(
        services
            .dial_timed_g711("radio@127.0.0.1/2000", "1000", 960, || false)
            .is_err()
    );
    assert_eq!(state.lock().unwrap().drops, 0);
    assert!(
        services
            .dial_timed_g711("radio@127.0.0.1/2000", "1000", 960, || true)
            .is_err()
    );
    assert_eq!(
        state.lock().unwrap().drops,
        1,
        "wrong capability owner is destroyed exactly once"
    );
    state.lock().unwrap().capabilities |= 128;
    let returned = services
        .dial_timed_g711("radio@127.0.0.1/2000", "1000", 960, || true)
        .unwrap();
    assert!(returned.timed_g711_egress());
    drop(returned);
    assert_eq!(state.lock().unwrap().drops, 2);
}

#[test]
fn outgoing_phone_factory_tail_accepts_complete_prefix_but_never_partial_tail() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.struct_size = std::mem::offset_of!(abi::rptadv_host_services_v10, peer_dial_phone) as u32;
    let old =
        unsafe { HostServices::open(&table) }.expect("complete previous host prefix remains valid");
    assert!(old.0.peer_dial_phone.is_none());
    table.struct_size += 1;
    assert!(unsafe { HostServices::open(&table) }.is_err());
    table.struct_size = std::mem::size_of_val(&table) as u32;
    assert!(
        unsafe { HostServices::open(&table) }
            .unwrap()
            .0
            .peer_dial_phone
            .is_none()
    );
}

#[test]
fn outgoing_phone_factory_requires_complete_media_group() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    unsafe extern "C" fn dial(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: usize,
        _: abi::rptadv_current_v1,
        _: *mut c_void,
        _: *mut *mut c_void,
    ) -> i32 {
        -1
    }
    table.peer_dial_phone = Some(dial);
    assert!(
        unsafe { HostServices::open(&table) }.is_err(),
        "telephone factory requires the existing async, timed and source media group"
    );
    let (phone, _) = crate::link::session::tests::phone_source_peer();
    let mut complete = phone.services.0;
    complete.peer_dial_phone = Some(dial);
    assert!(unsafe { HostServices::open(&complete) }.is_ok());
    let mut missing = complete;
    missing.peer_read_source = None;
    assert!(unsafe { HostServices::open(&missing) }.is_err());
    missing = complete;
    missing.peer_send_prepared_timed = None;
    assert!(unsafe { HostServices::open(&missing) }.is_err());
}

#[test]
fn outgoing_phone_factory_validates_exact_media_owner_and_final_cancellation() {
    unsafe extern "C" fn dial(
        context: *mut c_void,
        _: *const c_char,
        _: usize,
        _: *const c_char,
        _: usize,
        _: *const c_char,
        _: usize,
        _: usize,
        _: abi::rptadv_current_v1,
        _: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        let state = unsafe {
            &*context.cast::<std::sync::Arc<std::sync::Mutex<crate::link::session::tests::State>>>()
        };
        unsafe {
            *output = Box::into_raw(Box::new(state.clone())).cast();
        }
        0
    }
    let (phone, state) = crate::link::session::tests::phone_source_peer();
    let mut table = phone.services.0;
    table.context = ptr::from_ref(&state).cast_mut().cast();
    let account = PhoneAccountId::new("provider").unwrap();
    let missing = unsafe { HostServices::open(&table) }.unwrap();
    assert!(
        missing
            .dial_phone(&account, "1000", "5550100", 960, || true)
            .is_err()
    );
    table.peer_dial_phone = Some(dial);
    let services = unsafe { HostServices::open(&table) }.unwrap();
    assert!(
        services
            .dial_phone(&account, "1000", "5550100", 960, || false)
            .is_err()
    );
    assert_eq!(
        state.lock().unwrap().drops,
        0,
        "initial cancellation never invokes factory"
    );
    for (local, label) in [("other", "provider"), ("1000", "other")] {
        assert!(
            services
                .dial_phone(
                    &PhoneAccountId::new(label).unwrap(),
                    local,
                    "5550100",
                    960,
                    || true
                )
                .is_err()
        );
    }
    assert_eq!(
        state.lock().unwrap().drops,
        2,
        "mismatched owners retire exactly once"
    );
    let mut calls = 0;
    assert!(
        services
            .dial_phone(&account, "1000", "5550100", 960, || {
                calls += 1;
                calls == 1
            })
            .is_err()
    );
    assert_eq!(
        state.lock().unwrap().drops,
        3,
        "late cancellation owns returned handle"
    );
    let returned = services
        .dial_phone(&account, "1000", "5550100", 960, || true)
        .expect("exact PHONE/account/local source contract must be usable");
    assert_eq!(returned.phone_account(), Some(&account));
    drop(returned);
    assert_eq!(state.lock().unwrap().drops, 4);
    state.lock().unwrap().capabilities = 11;
    assert!(
        services
            .dial_phone(&account, "1000", "5550100", 960, || true)
            .is_err()
    );
    assert_eq!(
        state.lock().unwrap().drops,
        5,
        "ordinary transport cannot become called-party media"
    );
}

#[test]
fn phone_progress_tail_retains_established_factory_prefix_and_rejects_every_partial() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    let prior = std::mem::offset_of!(abi::rptadv_host_services_v10, peer_dial_phone_progress);
    assert_eq!(prior, 432);
    table.struct_size = prior as u32;
    let old = unsafe { HostServices::open(&table) }.unwrap();
    assert!(old.0.peer_dial_phone_progress.is_none());
    for size in prior + 1..std::mem::size_of_val(&table) {
        table.struct_size = size as u32;
        assert!(
            unsafe { HostServices::open(&table) }.is_err(),
            "partial progress tail {size}"
        );
    }
    table.struct_size = std::mem::size_of_val(&table) as u32;
    assert!(unsafe { HostServices::open(&table) }.is_ok());
}
