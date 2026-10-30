//! Exercise each host's actual account visitor, stable context and outbound dispatch.
use super::*;

unsafe extern "C" fn inspect_accounts(
    configuration: *const c_char,
    length: usize,
    sink: host_abi::rptadv_configuration_sink,
    context: *mut c_void,
    _: host_abi::rptadv_text_sink_v1,
    _: *mut c_void,
) -> i32 {
    let disabled =
        unsafe { std::slice::from_raw_parts(configuration.cast::<u8>(), length) } == b"disabled";
    let config = echo::AccountConfig {
        enabled: u32::from(!disabled),
        node_number: 7,
        local_node: echo::ByteSpan {
            data: b"524950".as_ptr(),
            length: 6,
        },
        ..echo::AccountConfig::default()
    };
    let mut node: host_abi::rptadv_node_host_configuration = unsafe { std::mem::zeroed() };
    node.struct_size = size_of::<host_abi::rptadv_node_host_configuration>() as u32;
    node.echolink = ptr::from_ref(&config).cast();
    unsafe { sink.unwrap()(context, &node) }
}

pub fn check(mut host: HostServicesOwner) {
    use rptadv_peer_support::echolink::host::Preparation;
    use std::{os::unix::fs::PermissionsExt, sync::mpsc, time::Duration};
    let fixture = support::Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let fault =
        unsafe { library.get::<unsafe extern "C" fn(usize)>(b"fixture_record_fault\0") }.unwrap();
    let directory = fixture.0.parent().unwrap();
    let configuration = directory.join("rpt_advanced.conf");
    let path = directory.join("echolink-secrets.conf");
    std::fs::write(&path, "[524950]\ndirectory_password=fixture-value\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (seen, notifications) = mpsc::channel();
    let resources = echo::RuntimeConfig::default();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let preparation = unsafe {
        Preparation::new(
            &configuration,
            &fixture.0,
            &resources,
            backend.cast(),
            move |peer| {
                incoming_echo(inspect_incoming, peer);
                seen.send(()).unwrap();
            },
        )
    };
    let mut product: host_abi::rptadv_product_descriptor_v1 = unsafe { std::mem::zeroed() };
    let mut visitor = |_: &echo::AccountConfig, _: Option<&std::path::Path>| Ok(());
    assert!(unsafe { visit_accounts(&product, "fixture", &mut visitor) }.is_err());
    product.inspect_configuration = Some(inspect_accounts);
    unsafe { rejected_account_records(&mut visitor) };
    let context = host.descriptor().context;
    let mut status = String::new();
    for (context, local, length, sink, expected) in [
        (
            context,
            c"524950".as_ptr(),
            6,
            Some(status_sink as unsafe extern "C" fn(_, _, _)),
            0,
        ),
        (
            ptr::null_mut(),
            c"524950".as_ptr(),
            6,
            Some(status_sink),
            -1,
        ),
        (context, ptr::null(), 1, Some(status_sink), -1),
        (context, c"524950".as_ptr(), 6, None, -1),
    ] {
        assert_eq!(
            unsafe {
                echo_account_status(
                    context,
                    local,
                    length,
                    sink,
                    ptr::from_mut(&mut status).cast(),
                )
            },
            expected
        );
    }
    assert!(status.is_empty());
    unsafe { host.prepare_echo(&product, "fixture", preparation) }.unwrap();
    assert!(notifications.try_recv().is_err());
    unsafe { host.activate_echo() }.unwrap();
    notifications.recv_timeout(Duration::from_secs(5)).unwrap();
    unsafe extern "C" fn status_sink(context: *mut c_void, text: *const c_char, length: usize) {
        let result = unsafe { &mut *context.cast::<String>() };
        result.push_str(
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) })
                .unwrap(),
        );
    }
    let context = host.descriptor().context;
    let mut status = String::new();
    assert_eq!(
        unsafe {
            echo_account_status(
                context,
                c"524950".as_ptr(),
                6,
                Some(status_sink),
                ptr::from_mut(&mut status).cast(),
            )
        },
        0
    );
    assert!(status.contains("echo-account local=524950"), "{status}");
    assert!(status.contains("state=online"), "{status}");
    assert!(status.contains("directory-failures=7"), "{status}");
    assert_eq!(
        unsafe {
            echo_account_status(
                context,
                c"missing".as_ptr(),
                7,
                Some(status_sink),
                ptr::from_mut(&mut status).cast(),
            )
        },
        0
    );
    unsafe { fault(39) };
    assert_eq!(
        unsafe {
            echo_account_status(
                context,
                c"524950".as_ptr(),
                6,
                Some(status_sink),
                ptr::from_mut(&mut status).cast(),
            )
        },
        -1
    );
    unsafe { fault(0) };
    unsafe extern "C" fn current(context: *mut c_void) -> u32 {
        if context.is_null() {
            return 1;
        }
        let remaining = unsafe { &*context.cast::<std::sync::atomic::AtomicUsize>() };
        u32::from(
            remaining
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |value| value.checked_sub(1),
                )
                .is_ok(),
        )
    }
    for credit in [0, 1] {
        let remaining = std::sync::atomic::AtomicUsize::new(credit);
        let mut rejected = ptr::null_mut();
        assert_eq!(
            unsafe {
                peer_dial(
                    context,
                    c"EchoLink/4294967295".as_ptr(),
                    19,
                    c"524950".as_ptr(),
                    6,
                    960,
                    Some(current),
                    ptr::from_ref(&remaining).cast_mut().cast(),
                    &mut rejected,
                )
            },
            -1
        );
        assert!(rejected.is_null(), "stale dial leaked its acquired QSO");
    }
    let mut peer = ptr::null_mut();
    assert_eq!(
        unsafe {
            peer_dial(
                context,
                c"EchoLink/4294967295".as_ptr(),
                19,
                c"524950".as_ptr(),
                6,
                960,
                Some(current),
                ptr::null_mut(),
                &mut peer,
            )
        },
        0
    );
    assert!(!peer.is_null());
    unsafe { peer_destroy(context, peer) };
    for (configuration_text, enabled) in [("disabled", false), ("fixture", true)] {
        let preparation = unsafe {
            Preparation::new(
                &configuration,
                &fixture.0,
                &resources,
                backend.cast(),
                |_| {},
            )
        };
        let outcome = unsafe {
            host.reload_echo(&product, configuration_text, preparation, || true, || true)
        }
        .unwrap();
        assert!(outcome.retirement.is_none());
        let mut peer = ptr::null_mut();
        let result = unsafe {
            peer_dial(
                context,
                c"EchoLink/4294967295".as_ptr(),
                19,
                c"524950".as_ptr(),
                6,
                960,
                Some(current),
                ptr::null_mut(),
                &mut peer,
            )
        };
        assert_eq!(
            result == 0,
            enabled,
            "account reload did not change host dial admission"
        );
        if !peer.is_null() {
            unsafe { peer_destroy(context, peer) };
        }
    }
    host.stop_echo().unwrap();
    assert!(notifications.try_recv().is_err());
    assert_ne!(
        unsafe {
            peer_dial(
                context,
                c"EchoLink/4294967295".as_ptr(),
                19,
                c"524950".as_ptr(),
                6,
                960,
                Some(current),
                ptr::null_mut(),
                &mut peer,
            )
        },
        0
    );
    assert!(peer.is_null());
    std::fs::remove_file(path).unwrap();
}

/// Check the real visitor boundary before any provider/account is prepared.
unsafe fn rejected_account_records(visit: &mut AccountVisitor<'_>) {
    let mut visit = visit;
    let context = ptr::from_mut(&mut visit).cast();
    assert_eq!(unsafe { prepare_account(context, ptr::null()) }, -1);
    for field in 0..=3 {
        let config = echo::AccountConfig::default();
        let mut node: host_abi::rptadv_node_host_configuration = unsafe { std::mem::zeroed() };
        node.struct_size = size_of::<host_abi::rptadv_node_host_configuration>() as u32;
        node.echolink = ptr::from_ref(&config).cast();
        match field {
            0 => node.struct_size = 0,
            1 => node.echolink = ptr::null(),
            2 => node.echolink_secrets_file_length = 1,
            _ => {
                node.echolink_secrets_file = c"fixture".as_ptr().cast();
                node.echolink_secrets_file_length = 7;
            }
        }
        assert_eq!(
            unsafe { prepare_account(context, &node) },
            if field == 3 { 0 } else { -1 }
        );
    }
}

unsafe extern "C" fn inspect_incoming(
    request: *const host_abi::rptadv_incoming_peer_v1,
    peer: *mut c_void,
) -> i32 {
    let request = unsafe { &*request };
    let text = |span: host_abi::rptadv_peer_bytes_v1| unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(span.data, span.length)).unwrap()
    };
    assert_eq!(request.protocol, 2);
    assert_eq!(text(request.local), "524950");
    assert_eq!(text(request.remote), "4294967295");
    assert_eq!(text(request.source), "127.0.0.2");
    assert_eq!(unsafe { peer_rate(ptr::null_mut(), peer) }, 8000);
    1 // Leave ownership with the adapter to exercise its rejection cleanup.
}
