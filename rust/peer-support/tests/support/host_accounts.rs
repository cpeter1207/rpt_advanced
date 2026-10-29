//! Exercise each host's actual account visitor, stable context and outbound dispatch.
use super::*;

unsafe extern "C" fn inspect_accounts(
    _: *const c_char,
    _: usize,
    sink: host_abi::rptadv_configuration_sink,
    context: *mut c_void,
    _: host_abi::rptadv_text_sink_v1,
    _: *mut c_void,
) -> i32 {
    let config = echo::AccountConfig {
        enabled: 1,
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
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    let fixture = support::Fixture::new("network");
    let directory = fixture.0.parent().unwrap();
    let configuration = directory.join("rpt_advanced.conf");
    let path = directory.join("echolink-secrets.conf");
    std::fs::write(&path, "[524950]\ndirectory_password=fixture-value\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let notifications = Arc::new(AtomicUsize::new(0));
    let seen = notifications.clone();
    let resources = echo::RuntimeConfig::default();
    let backend = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let preparation = unsafe {
        Preparation::new(
            &configuration,
            &fixture.0,
            &resources,
            backend.cast(),
            move |_| {
                seen.fetch_add(1, Ordering::Release);
            },
        )
    };
    let mut product: host_abi::rptadv_product_descriptor_v1 = unsafe { std::mem::zeroed() };
    product.inspect_configuration = Some(inspect_accounts);
    unsafe { host.prepare_echo(&product, "fixture", preparation) }.unwrap();
    assert_eq!(notifications.load(Ordering::Acquire), 0);
    unsafe { host.activate_echo() }.unwrap();
    unsafe extern "C" fn current(_: *mut c_void) -> u32 {
        1
    }
    let mut peer = ptr::null_mut();
    let context = host.descriptor().context;
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
    host.stop_echo().unwrap();
    assert_eq!(notifications.load(Ordering::Acquire), 1);
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
