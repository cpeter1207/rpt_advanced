//! Standalone static and DNS lookup for numeric AllStarLink node identities.

use crate::{
    abi,
    host_services::{boundary, input},
};
use std::{
    collections::hash_map::RandomState,
    ffi::{CStr, CString, c_char, c_void},
    fs,
    hash::{BuildHasher, Hasher},
    io,
    net::{IpAddr, ToSocketAddrs},
};

/// Use the system resolver and its configured DNS servers on the control plane.
fn resolve_srv(service: &str) -> io::Result<Option<(String, u16)>> {
    resolve_srv_with(service, |service, answer| {
        // SAFETY: the query and writable answer buffer remain live for this synchronous call.
        unsafe {
            crate::abi::res_query(
                service.as_ptr(),
                1,
                33,
                answer.as_mut_ptr(),
                answer.len() as i32,
            )
        }
    })
}

fn resolve_srv_with(
    service: &str,
    query: impl FnOnce(&CStr, &mut [u8]) -> i32,
) -> io::Result<Option<(String, u16)>> {
    let service = CString::new(service).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut answer = [0_u8; 65535];
    let length = query(&service, &mut answer);
    if length < 0 {
        return Ok(None);
    }
    let answer = answer
        .get(..length as usize)
        .ok_or(io::ErrorKind::InvalidData)?;
    srv_record(answer, RandomState::new().build_hasher().finish())
}

fn valid_srv_host(host: &str) -> bool {
    !host.is_empty() && host != "."
}

/// Decode with libresolv, then apply SRV priority and weighted selection.
fn srv_record(answer: &[u8], choice: u64) -> io::Result<Option<(String, u16)>> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let mut message = std::mem::MaybeUninit::zeroed();
    // SAFETY: libresolv validates the packet before initializing the message view.
    if unsafe {
        crate::abi::ns_initparse(answer.as_ptr(), answer.len() as i32, message.as_mut_ptr())
    } != 0
    {
        return Err(invalid());
    }
    let mut message = unsafe { message.assume_init() };
    let mut records = Vec::new();
    for index in 0..message._counts[1] {
        // ns_parserr writes only the used prefix of name; initialize its unused bytes too.
        let mut record = std::mem::MaybeUninit::zeroed();
        // SAFETY: the initialized message borrows the live answer buffer.
        if unsafe { crate::abi::ns_parserr(&mut message, 1, i32::from(index), record.as_mut_ptr()) }
            != 0
        {
            return Err(invalid());
        }
        let record = unsafe { record.assume_init() };
        if record.type_ != 33 || record.rr_class != 1 {
            continue;
        }
        if record.rdlength < 7 {
            return Err(invalid());
        }
        // SAFETY: ns_parserr validated the RDATA extent within the answer.
        let data =
            unsafe { std::slice::from_raw_parts(record.rdata, usize::from(record.rdlength)) };
        let priority = u16::from_be_bytes([data[0], data[1]]);
        let weight = u16::from_be_bytes([data[2], data[3]]);
        let port = u16::from_be_bytes([data[4], data[5]]);
        let mut host = [0 as std::ffi::c_char; 1025];
        // SAFETY: dn_expand validates compressed labels against the complete answer bounds.
        let consumed = unsafe {
            crate::abi::dn_expand(
                answer.as_ptr(),
                answer.as_ptr().add(answer.len()),
                data.as_ptr().add(6),
                host.as_mut_ptr(),
                host.len() as i32,
            )
        };
        if consumed < 0 || consumed as usize + 6 != data.len() || port == 0 {
            return Err(invalid());
        }
        let host = unsafe { CStr::from_ptr(host.as_ptr()) }
            .to_str()
            .map_err(|_| invalid())?;
        if !valid_srv_host(host) {
            return Err(invalid());
        }
        records.push((priority, weight, host.to_owned(), port));
    }
    records.sort_by_key(|record| (record.0, record.1));
    let Some(priority) = records.first().map(|record| record.0) else {
        return Ok(None);
    };
    records.retain(|record| record.0 == priority);
    let total: u64 = records.iter().map(|record| u64::from(record.1)).sum();
    let mut selected = choice % (total + 1);
    Ok(records.into_iter().find_map(|(_, weight, host, port)| {
        if selected <= u64::from(weight) {
            Some((host, port))
        } else {
            selected -= u64::from(weight);
            None
        }
    }))
}

fn file_record(path: &str, remote: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return None;
    };
    let mut extnodes = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            extnodes = &line[1..line.len() - 1] == "extnodes";
            continue;
        }
        if !extnodes || line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        let Some((node, value)) = line.split_once('=') else {
            continue;
        };
        if node.trim() == remote {
            return Some(value.trim().to_owned());
        }
    }
    None
}

fn addresses(host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
    format!("{host}:{port}")
        .to_socket_addrs()
        .map(|addresses| addresses.map(|address| address.ip()).collect())
}

fn send_srv(
    record: io::Result<Option<(String, u16)>>,
    sink: unsafe extern "C" fn(*mut c_void, *const c_char, usize, u16),
    context: *mut c_void,
) -> i32 {
    let Ok(record) = record else {
        return 1;
    };
    if let Some((host, port)) = record {
        // SAFETY: the owned host bytes stay live for the synchronous sink call.
        unsafe { sink(context, host.as_ptr().cast(), host.len(), port) };
    }
    0
}

fn send_addresses(
    addresses: io::Result<Vec<IpAddr>>,
    sink: unsafe extern "C" fn(*mut c_void, *const c_char, usize),
    context: *mut c_void,
) -> i32 {
    let Ok(addresses) = addresses else {
        return 1;
    };
    if addresses.is_empty() {
        return 1;
    }
    for address in addresses {
        let address = address.to_string();
        // SAFETY: the owned address bytes stay live for the synchronous sink call.
        unsafe { sink(context, address.as_ptr().cast(), address.len()) };
    }
    0
}

/// Read the existing extnodes file format without interpreting directory policy.
pub(crate) unsafe extern "C" fn directory_record(
    _: *mut c_void,
    path: *const c_char,
    path_length: usize,
    node: *const c_char,
    node_length: usize,
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(path), Some(node), Some(sink)) = (
            unsafe { input(path, path_length) },
            unsafe { input(node, node_length) },
            sink,
        ) else {
            return -1;
        };
        if let Some(record) = file_record(path, node) {
            unsafe { sink(context, record.as_ptr().cast(), record.len()) };
        }
        0
    })
}
/// Preserve standalone DNS-error fallback; malformed SRV never uses the default port.
pub(crate) unsafe extern "C" fn directory_srv(
    _: *mut c_void,
    service: *const c_char,
    length: usize,
    sink: abi::rptadv_directory_srv_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(service), Some(sink)) = (unsafe { input(service, length) }, sink) else {
            return -1;
        };
        send_srv(resolve_srv(service), sink, context)
    })
}
/// Resolve addresses on the control plane; unavailable and empty answers permit fallback.
pub(crate) unsafe extern "C" fn directory_addresses(
    _: *mut c_void,
    host: *const c_char,
    length: usize,
    port: u16,
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(host), Some(sink)) = (unsafe { input(host, length) }, sink) else {
            return -1;
        };
        send_addresses(addresses(host, port), sink, context)
    })
}
/// Retain standalone's existing directory failure wording outside the product.
pub(crate) unsafe extern "C" fn directory_notice(_: *mut c_void, reason: u32) {
    boundary((), || {
        let reason = match reason {
            1 => "InvalidRecord",
            2 => "SourceMismatch",
            3 => "NotFound",
            4 => "DnsFailed",
            _ => return,
        };
        eprintln!("rpt-advanced: ASL directory lookup failed: {reason}");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    unsafe extern "C" fn collect(context: *mut c_void, text: *const c_char, length: usize) {
        unsafe { &mut *context.cast::<Vec<String>>() }
            .push(unsafe { input(text, length) }.unwrap().into());
    }

    unsafe extern "C" fn unexpected_srv(_: *mut c_void, _: *const c_char, _: usize, _: u16) {
        panic!("invalid DNS input must not deliver a target");
    }

    unsafe extern "C" fn collect_srv(
        context: *mut c_void,
        text: *const c_char,
        length: usize,
        port: u16,
    ) {
        unsafe { &mut *context.cast::<Vec<(String, u16)>>() }
            .push((unsafe { input(text, length) }.unwrap().into(), port));
    }

    fn srv_packet() -> Vec<u8> {
        // Two IN/SRV answers: priority20/weight1/4569 and priority10/weight10/4571.
        // The second target points to the first target's a.test label at byte29.
        vec![
            0, 1, 0x81, 0x80, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 33, 0, 1, 0, 0, 0, 60, 0, 14, 0, 20, 0,
            1, 0x11, 0xd9, 1, b'a', 4, b't', b'e', b's', b't', 0, 0, 0, 33, 0, 1, 0, 0, 0, 60, 0,
            8, 0, 10, 0, 10, 0x11, 0xdb, 0xc0, 29,
        ]
    }

    #[test]
    fn file_backend_keeps_raw_records_and_existing_unavailable_file_behavior() {
        let path =
            std::env::temp_dir().join(format!("rpt-directory-backend-{}.conf", std::process::id()));
        fs::write(&path, "[other]\n123=wrong\n[extnodes]\n\n; comment\n# comment\n[not-a-section\nnot-a-record\n999=ignored\n123 = raw-record\n").unwrap();
        let path = path.to_str().unwrap();
        let mut results = Vec::<String>::new();
        let context = ptr::from_mut(&mut results).cast();
        assert_eq!(
            unsafe {
                directory_record(
                    ptr::null_mut(),
                    path.as_ptr().cast(),
                    path.len(),
                    c"123".as_ptr(),
                    3,
                    Some(collect),
                    context,
                )
            },
            0
        );
        assert_eq!(results, ["raw-record"]);
        assert_eq!(file_record(path, "456"), None);
        assert_eq!(
            unsafe {
                directory_record(
                    ptr::null_mut(),
                    path.as_ptr().cast(),
                    path.len(),
                    c"456".as_ptr(),
                    3,
                    Some(collect),
                    context,
                )
            },
            0
        );
        assert_eq!(file_record("", "123"), None);
        fs::remove_file(path).unwrap();
        assert_eq!(file_record(path, "123"), None);
    }

    #[test]
    fn backend_callbacks_reject_bad_borrows_and_resolve_numeric_addresses_without_network() {
        let null = ptr::null_mut();
        assert_eq!(
            unsafe {
                directory_record(
                    null,
                    ptr::null(),
                    1,
                    c"123".as_ptr(),
                    3,
                    Some(collect),
                    null,
                )
            },
            -1
        );
        assert_eq!(
            unsafe { directory_record(null, ptr::null(), 0, ptr::null(), 1, Some(collect), null) },
            -1
        );
        assert_eq!(
            unsafe { directory_record(null, ptr::null(), 0, ptr::null(), 0, None, null) },
            -1
        );
        assert_eq!(
            unsafe { directory_srv(null, ptr::null(), 1, None, null) },
            -1
        );
        assert_eq!(
            unsafe {
                directory_srv(
                    null,
                    b"bad\0service".as_ptr().cast(),
                    11,
                    Some(unexpected_srv),
                    null,
                )
            },
            1
        );
        assert_eq!(
            unsafe { directory_addresses(null, ptr::null(), 1, 4569, Some(collect), null) },
            -1
        );
        assert_eq!(
            unsafe { directory_addresses(null, ptr::null(), 0, 4569, None, null) },
            -1
        );
        let mut results = Vec::<String>::new();
        assert_eq!(
            unsafe {
                directory_addresses(
                    null,
                    c"127.0.0.1".as_ptr(),
                    9,
                    4569,
                    Some(collect),
                    ptr::from_mut(&mut results).cast(),
                )
            },
            0
        );
        assert_eq!(results, ["127.0.0.1"]);
        assert_eq!(
            unsafe {
                directory_addresses(
                    null,
                    b"bad\0host".as_ptr().cast(),
                    8,
                    4569,
                    Some(collect),
                    null,
                )
            },
            1
        );
        for reason in 0..=4 {
            unsafe { directory_notice(null, reason) };
        }
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
    fn resolver_checks_query_lengths_and_parses_a_successful_answer() {
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
    fn directory_sinks_preserve_not_found_errors_and_addresses() {
        let mut srv = Vec::<(String, u16)>::new();
        let context = ptr::from_mut(&mut srv).cast();
        assert_eq!(
            send_srv(Ok(Some(("a.test".into(), 4571))), collect_srv, context),
            0
        );
        assert_eq!(srv, [("a.test".into(), 4571)]);
        assert_eq!(send_srv(Ok(None), unexpected_srv, ptr::null_mut()), 0);
        assert_eq!(
            send_srv(
                Err(io::ErrorKind::InvalidData.into()),
                unexpected_srv,
                ptr::null_mut()
            ),
            1
        );

        let mut results = Vec::<String>::new();
        let context = ptr::from_mut(&mut results).cast();
        assert_eq!(send_addresses(Ok(vec![]), collect, context), 1);
        assert_eq!(
            send_addresses(Err(io::ErrorKind::NotFound.into()), collect, context),
            1
        );
        assert_eq!(
            send_addresses(
                Ok(vec![IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]),
                collect,
                context
            ),
            0
        );
        assert_eq!(results, ["127.0.0.1"]);
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

        assert_eq!(resolve_srv("").unwrap(), None);
    }
}
