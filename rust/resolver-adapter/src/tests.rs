use super::*;
use std::ptr;

fn srv_packet() -> Vec<u8> {
    // Two IN/SRV answers: priority20/weight1/4569 and priority10/weight10/4571.
    // The second target points to the first target's a.test label at byte29.
    vec![
        0, 1, 0x81, 0x80, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 33, 0, 1, 0, 0, 0, 60, 0, 14, 0, 20, 0, 1,
        0x11, 0xd9, 1, b'a', 4, b't', b'e', b's', b't', 0, 0, 0, 33, 0, 1, 0, 0, 0, 60, 0, 8, 0,
        10, 0, 10, 0x11, 0xdb, 0xc0, 29,
    ]
}

#[test]
fn srv_packet_uses_priority_port_and_compressed_target() {
    let packet = srv_packet();
    assert_eq!(
        super::srv_record(&packet, 0).unwrap(),
        Some(("a.test".into(), 4571))
    );
    let mut weighted = packet.clone();
    weighted[24] = 10;
    assert_eq!(
        super::srv_record(&weighted, 0).unwrap(),
        Some(("a.test".into(), 4569))
    );
    assert_eq!(
        super::srv_record(&weighted, 10).unwrap(),
        Some(("a.test".into(), 4571))
    );
    for end in 0..packet.len() {
        assert!(
            super::srv_record(&packet[..end], 0).is_err(),
            "accepted length {end}"
        );
    }
    let mut unavailable = packet.clone();
    unavailable[54] = 0;
    unavailable[55] = 0;
    assert!(super::srv_record(&unavailable, 0).is_err());
    let mut zero_port = packet.clone();
    zero_port[52] = 0;
    zero_port[53] = 0;
    assert!(super::srv_record(&zero_port, 0).is_err());
    assert_eq!(
        super::srv_record(&[0, 1, 0x81, 0x80, 0, 0, 0, 0, 0, 0, 0, 0], 0).unwrap(),
        None
    );
}

#[test]
fn srv_packet_preserves_escaped_target_octets() {
    for (octet, target) in [
        (0, r"\000.test"),
        (b' ', r"\032.test"),
        (b'.', r"\..test"),
        (b'\\', r"\\.test"),
        (b'Z', "Z.test"),
        (127, r"\127.test"),
        (255, r"\255.test"),
    ] {
        let mut packet = srv_packet();
        packet[30] = octet;
        assert_eq!(srv_record(&packet, 0).unwrap(), Some((target.into(), 4571)));
    }
}

#[test]
fn resolver_checks_query_lengths_and_parses_a_successful_answer() {
    // An overlong label is rejected while constructing the query, before DNS I/O.
    assert_eq!(resolve_srv(&"x".repeat(64)).unwrap(), None);
    let packet = srv_packet();
    let expected = ("a.test".to_owned(), 4571);
    assert_eq!(
        resolve_srv_with("_asl._tcp.test", |service, answer| {
            assert_eq!(service.to_str().unwrap(), "_asl._tcp.test");
            answer[..packet.len()].copy_from_slice(&packet);
            packet.len() as i32
        })
        .unwrap(),
        Some(expected)
    );
    assert!(matches!(
        resolve_srv_with("bad\0service", |_, _| panic!("invalid name reached resolver")),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));
    assert_eq!(resolve_srv_with("missing", |_, _| -1).unwrap(), None);
    assert!(matches!(
        resolve_srv_with("oversized", |_, _| 65536),
        Err(error) if error.kind() == io::ErrorKind::InvalidData
    ));
}

#[test]
fn srv_packet_rejects_other_records_and_invalid_targets() {
    let mut wrong_type = srv_packet();
    wrong_type[39] = 1;
    assert_eq!(
        srv_record(&wrong_type, 0).unwrap(),
        Some(("a.test".into(), 4569))
    );

    let mut wrong_class = srv_packet();
    wrong_class[41] = 3;
    assert_eq!(
        srv_record(&wrong_class, 0).unwrap(),
        Some(("a.test".into(), 4569))
    );

    let mut short_data = srv_packet();
    short_data[22] = 6;
    short_data.drain(29..37);
    assert!(srv_record(&short_data, 0).is_err());

    let mut empty_host = srv_packet();
    empty_host[22] = 7;
    empty_host.drain(29..36);
    assert!(srv_record(&empty_host, 0).is_err());

    let mut extra_target_data = srv_packet();
    extra_target_data[22] = 15;
    extra_target_data.insert(37, 0);
    assert!(srv_record(&extra_target_data, 0).is_err());

    let mut invalid_pointer = srv_packet();
    invalid_pointer[54] = 0xff;
    invalid_pointer[55] = 0xff;
    assert!(srv_record(&invalid_pointer, 0).is_err());

    assert!(!valid_srv_host(""));
    assert!(!valid_srv_host("."));
    assert!(valid_srv_host("a.test"));

    let mut looping_owner = srv_packet();
    looping_owner[37] = 0xc0;
    looping_owner.insert(38, 37);
    assert!(srv_record(&looping_owner, 0).is_err());
}

#[test]
fn descriptor_lifecycle_and_numeric_addresses() {
    let api = unsafe { &*rptadv_resolver_adapter_descriptor_v1() };
    assert_eq!(api.abi_version, 1);
    assert_eq!(api.capability, *b"rptadv.resolver\0");
    assert_eq!(api.struct_size as usize, std::mem::size_of_val(api));
    let mut context = ptr::null_mut();
    unsafe {
        assert_eq!(api.create.unwrap()(&mut context), 0);
        assert!(!context.is_null());
        let mut result = Vec::<String>::new();
        for endpoint in ["127.0.0.1:4569", "[::1]:4570"] {
            assert_eq!(
                api.resolve_addresses.unwrap()(
                    context,
                    endpoint.as_ptr(),
                    endpoint.len(),
                    Some(collect),
                    ptr::from_mut(&mut result).cast()
                ),
                0
            );
        }
        assert_eq!(result, ["127.0.0.1:4569", "[::1]:4570"]);
        api.destroy.unwrap()(context);
        api.destroy.unwrap()(ptr::null_mut());
        assert_eq!(api.create.unwrap()(ptr::null_mut()), -1);
    }
}
unsafe extern "C" fn collect(context: *mut c_void, text: *const u8, length: usize) {
    unsafe { &mut *context.cast::<Vec<String>>() }
        .push(unsafe { input(text, length) }.unwrap().into());
}
unsafe extern "C" fn collect_srv(context: *mut c_void, text: *const u8, length: usize, port: u16) {
    unsafe { &mut *context.cast::<Vec<(String, u16)>>() }
        .push((unsafe { input(text, length) }.unwrap().into(), port));
}
#[test]
fn abi_rejects_bad_borrows_and_reports_unavailable() {
    let api = unsafe { &*rptadv_resolver_adapter_descriptor_v1() };
    let mut context = ptr::null_mut();
    unsafe { api.create.unwrap()(&mut context) };
    for (text, length) in [
        (ptr::null(), 1),
        (b"x".as_ptr(), usize::MAX),
        (b"\xff".as_ptr(), 1),
    ] {
        unsafe {
            assert_eq!(
                api.resolve_addresses.unwrap()(
                    context,
                    text,
                    length,
                    Some(collect),
                    ptr::null_mut()
                ),
                -1
            );
            assert_eq!(
                api.resolve_srv.unwrap()(context, text, length, Some(collect_srv), ptr::null_mut()),
                -1
            );
        }
    }
    unsafe {
        assert_eq!(
            api.resolve_srv.unwrap()(context, ptr::null(), 0, None, ptr::null_mut()),
            -1
        );
        assert_eq!(
            api.resolve_srv.unwrap()(
                ptr::null_mut(),
                b"x".as_ptr(),
                1,
                Some(collect_srv),
                ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            api.resolve_addresses.unwrap()(context, ptr::null(), 0, None, ptr::null_mut()),
            -1
        );
        assert_eq!(
            api.resolve_addresses.unwrap()(
                ptr::null_mut(),
                b"x".as_ptr(),
                1,
                Some(collect),
                ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            api.resolve_addresses.unwrap()(
                context,
                b"bad\0host:1".as_ptr(),
                10,
                Some(collect),
                ptr::null_mut()
            ),
            1
        );
        assert_eq!(
            api.resolve_srv.unwrap()(
                context,
                b"bad\0service".as_ptr(),
                11,
                Some(collect_srv),
                ptr::null_mut()
            ),
            1
        );
        api.destroy.unwrap()(context);
    }
    assert_eq!(boundary(|| panic!("contained")), -1);
    assert_eq!(unsafe { input(ptr::null(), 0) }, Some(""));
}
#[test]
fn result_status_preserves_absence_errors_and_order() {
    let mut srv = Vec::<(String, u16)>::new();
    let context = ptr::from_mut(&mut srv).cast();
    assert_eq!(send_srv(Ok(None), collect_srv, context), 0);
    assert!(srv.is_empty());
    assert_eq!(
        send_srv(Err(io::ErrorKind::InvalidData.into()), collect_srv, context),
        1
    );
    assert_eq!(
        send_srv(Ok(Some(("a.test".into(), 4571))), collect_srv, context),
        0
    );
    assert_eq!(srv, [("a.test".into(), 4571)]);
    let mut addresses = Vec::<String>::new();
    let context = ptr::from_mut(&mut addresses).cast();
    assert_eq!(send_addresses(Ok(vec![]), collect, context), 1);
    assert_eq!(
        send_addresses(Err(io::ErrorKind::NotFound.into()), collect, context),
        1
    );
    assert_eq!(
        send_addresses(
            Ok(vec![
                "[::1]:4571".parse().unwrap(),
                "127.0.0.1:4569".parse().unwrap()
            ]),
            collect,
            context
        ),
        0
    );
    assert_eq!(addresses, ["[::1]:4571", "127.0.0.1:4569"]);
}
