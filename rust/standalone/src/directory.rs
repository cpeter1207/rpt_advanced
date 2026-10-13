//! Filesystem records and neutral resolver forwarding; directory policy lives in the product.
use crate::{
    abi,
    host_services::{boundary, input},
};
use std::{
    ffi::{c_char, c_void},
    fs,
    net::SocketAddr,
};

/// Retains one resolver context until the product has stopped every host callback.
pub(crate) struct Resolver {
    api: &'static abi::rptadv_resolver_descriptor_v1,
    context: std::ptr::NonNull<c_void>,
}

impl Resolver {
    pub(crate) fn new(
        api: &'static abi::rptadv_resolver_descriptor_v1,
    ) -> Result<Self, crate::providers::ProviderError> {
        let mut context = std::ptr::null_mut();
        let status = unsafe { api.create.expect("validated resolver")(&mut context) };
        if status != 0 {
            if !context.is_null() {
                unsafe { api.destroy.expect("validated resolver")(context) };
            }
            return Err(crate::providers::ProviderError::Initialize(
                "librptadv_resolver_adapter.so.1",
            ));
        }
        let context = std::ptr::NonNull::new(context).ok_or(
            crate::providers::ProviderError::Initialize("librptadv_resolver_adapter.so.1"),
        )?;
        Ok(Self { api, context })
    }

    /// Preserve the provider's ordered SocketAddr results; do not resolve again in the host.
    pub(crate) fn addresses(&self, endpoint: &str) -> Result<Vec<SocketAddr>, i32> {
        unsafe extern "C" fn collect(context: *mut c_void, value: *const u8, length: usize) {
            let results = unsafe { &mut *context.cast::<Result<Vec<SocketAddr>, i32>>() };
            let parsed = unsafe { input(value.cast(), length) }.and_then(|text| text.parse().ok());
            match (results.as_mut(), parsed) {
                (Ok(results), Some(address)) => results.push(address),
                _ => *results = Err(-1),
            }
        }
        let mut results: Result<Vec<SocketAddr>, i32> = Ok(Vec::new());
        let status = unsafe {
            self.api.resolve_addresses.expect("validated resolver")(
                self.context.as_ptr(),
                endpoint.as_ptr(),
                endpoint.len(),
                Some(collect),
                std::ptr::from_mut(&mut results).cast(),
            )
        };
        if status == 0 { results } else { Err(status) }
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        // Product shutdown joins users before the host drops; the provider DSO is still retained.
        unsafe { self.api.destroy.expect("validated resolver")(self.context.as_ptr()) };
    }
}

unsafe fn resolver<'a>(context: *mut c_void) -> Option<&'a Resolver> {
    // Borrow only this immutable field: radio staging in other host callbacks is independent.
    if context.is_null() {
        return None;
    }
    unsafe {
        &*std::ptr::addr_of!(
            (*context.cast::<crate::host_services::StandaloneHostContext>()).resolver
        )
    }
    .as_ref()
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
    host_context: *mut c_void,
    service: *const c_char,
    length: usize,
    sink: abi::rptadv_directory_srv_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(resolver), Some(service), Some(sink)) = (
            unsafe { resolver(host_context) },
            unsafe { input(service, length) },
            sink,
        ) else {
            return -1;
        };
        let mut bridge = (sink, context);
        unsafe extern "C" fn result(
            context: *mut c_void,
            host: *const u8,
            length: usize,
            port: u16,
        ) {
            let (sink, context) = unsafe {
                &*context.cast::<(
                    unsafe extern "C" fn(*mut c_void, *const c_char, usize, u16),
                    *mut c_void,
                )>()
            };
            unsafe { sink(*context, host.cast(), length, port) };
        }
        unsafe {
            resolver.api.resolve_srv.expect("validated resolver")(
                resolver.context.as_ptr(),
                service.as_ptr(),
                service.len(),
                Some(result),
                std::ptr::from_mut(&mut bridge).cast(),
            )
        }
    })
}
/// Forward IP text through the unchanged directory ABI, preserving address order.
pub(crate) unsafe extern "C" fn directory_addresses(
    host_context: *mut c_void,
    host: *const c_char,
    length: usize,
    port: u16,
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(resolver), Some(host), Some(sink)) = (
            unsafe { resolver(host_context) },
            unsafe { input(host, length) },
            sink,
        ) else {
            return -1;
        };
        let addresses = match resolver.addresses(&format!("{host}:{port}")) {
            Ok(addresses) if !addresses.is_empty() => addresses,
            Ok(_) => return 1,
            Err(status) => return status,
        };
        for address in addresses {
            let address = address.ip().to_string();
            unsafe { sink(context, address.as_ptr().cast(), address.len()) };
        }
        0
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DESTROYED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn fake_create(context: *mut *mut c_void) -> i32 {
        unsafe { *context = Box::into_raw(Box::new(7_u8)).cast() };
        0
    }
    unsafe extern "C" fn failed_create(context: *mut *mut c_void) -> i32 {
        unsafe { fake_create(context) };
        1
    }
    unsafe extern "C" fn empty_create(_: *mut *mut c_void) -> i32 {
        0
    }
    unsafe extern "C" fn fake_destroy(context: *mut c_void) {
        assert_eq!(unsafe { *Box::from_raw(context.cast::<u8>()) }, 7);
        DESTROYED.fetch_add(1, Ordering::Relaxed);
    }
    unsafe extern "C" fn fake_addresses(
        _: *mut c_void,
        endpoint: *const u8,
        length: usize,
        sink: abi::rptadv_resolver_address_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        let endpoint = unsafe { input(endpoint.cast(), length) }.unwrap();
        let values: &[&str] = match endpoint {
            "ordered:4569" => &["[::ffff:192.0.2.1]:4569", "192.0.2.2:4569"],
            "invalid:4569" => &["not an address", "192.0.2.2:4569"],
            "empty:4569" => &[],
            _ => return 1,
        };
        for value in values {
            unsafe { sink.unwrap()(context, value.as_ptr(), value.len()) };
        }
        0
    }

    #[test]
    fn resolver_owner_preserves_results_and_destroys_each_created_context() {
        let providers = crate::providers::ProviderSet::load().unwrap();
        let mut api = *providers.resolver_descriptor().unwrap();
        api.create = Some(fake_create);
        api.destroy = Some(fake_destroy);
        api.resolve_addresses = Some(fake_addresses);
        let api = Box::leak(Box::new(api));
        DESTROYED.store(0, Ordering::Relaxed);
        let resolver = Resolver::new(api).unwrap();
        assert_eq!(
            resolver.addresses("ordered:4569").unwrap(),
            [
                "[::ffff:192.0.2.1]:4569".parse::<SocketAddr>().unwrap(),
                "192.0.2.2:4569".parse::<SocketAddr>().unwrap(),
            ]
        );
        assert_eq!(resolver.addresses("invalid:4569"), Err(-1));
        assert_eq!(resolver.addresses("missing:4569"), Err(1));
        assert_eq!(resolver.addresses("empty:4569"), Ok(Vec::new()));
        drop(resolver);
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 1);
        let mut rejected = *api;
        rejected.create = Some(failed_create);
        assert!(Resolver::new(Box::leak(Box::new(rejected))).is_err());
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 2);
        rejected.create = Some(empty_create);
        assert!(Resolver::new(Box::leak(Box::new(rejected))).is_err());
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 2);
    }
    unsafe extern "C" fn collect(context: *mut c_void, text: *const c_char, length: usize) {
        unsafe { &mut *context.cast::<Vec<String>>() }
            .push(unsafe { input(text, length) }.unwrap().into());
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
    fn callbacks_forward_through_the_packaged_resolver() {
        let providers = Box::leak(Box::new(crate::providers::ProviderSet::load().unwrap()));
        let owner = crate::host_services::HostServicesOwner::new(
            Vec::new(),
            Some(providers),
            crate::secrets::SecretsFile::parse("").unwrap(),
        )
        .unwrap();
        let host = owner.descriptor();
        let mut results = Vec::<String>::new();
        let context = ptr::from_mut(&mut results).cast();
        assert_eq!(
            unsafe {
                directory_addresses(
                    host.context,
                    c"127.0.0.1".as_ptr(),
                    9,
                    4569,
                    Some(collect),
                    context,
                )
            },
            0
        );
        assert_eq!(results, ["127.0.0.1"]);
        assert_eq!(
            unsafe {
                directory_addresses(
                    host.context,
                    b"bad\0host".as_ptr().cast(),
                    8,
                    4569,
                    Some(collect),
                    context,
                )
            },
            1
        );
        unsafe extern "C" fn srv(_: *mut c_void, _: *const c_char, _: usize, _: u16) {
            unreachable!()
        }
        assert_eq!(
            unsafe {
                directory_srv(
                    host.context,
                    b"bad\0service".as_ptr().cast(),
                    11,
                    Some(srv),
                    ptr::null_mut(),
                )
            },
            1
        );
        for reason in 0..=4 {
            unsafe { directory_notice(ptr::null_mut(), reason) };
        }
    }
    #[test]
    fn callbacks_reject_missing_contexts_sinks_and_bad_borrows() {
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
            unsafe { directory_addresses(null, ptr::null(), 1, 4569, Some(collect), null) },
            -1
        );
        assert_eq!(
            unsafe { directory_addresses(null, ptr::null(), 0, 4569, None, null) },
            -1
        );
    }
}
