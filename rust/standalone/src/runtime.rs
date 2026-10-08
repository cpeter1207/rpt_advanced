//! Process-lifetime owner for the portable product.

use crate::{abi, host_services::HostServicesOwner};
use std::{ffi::c_char, mem::size_of};

/// Product startup, reload, or shutdown failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// The product lifecycle table is incomplete.
    InvalidDescriptor,
    /// A required shared provider is missing or incompatible.
    Provider(crate::providers::ProviderError),
    /// Product start rejected the selected providers or configuration.
    Start,
    /// Product could not prepare the replacement configuration.
    Reload,
    /// Product could not quiesce all runtime owners.
    Stop,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDescriptor => "product lifecycle descriptor is incomplete",
            Self::Start => "standalone product startup failed",
            Self::Reload => "standalone configuration reload failed",
            Self::Stop => "standalone product shutdown failed",
            Self::Provider(error) => return error.fmt(formatter),
        })
    }
}

impl std::error::Error for RuntimeError {}

/// Running product and host context. Product stop precedes host/provider release.
pub struct ProductRuntime {
    product: &'static abi::rptadv_product_descriptor_v1,
    reload: unsafe extern "C" fn(*const c_char, usize) -> i32,
    stop: unsafe extern "C" fn() -> i32,
    host: Option<HostServicesOwner>,
    providers: Option<Box<crate::providers::ProviderSet>>,
}

impl ProductRuntime {
    /// Start the portable product and retain its host callbacks and providers.
    ///
    /// # Safety
    /// The descriptors and their code must stay live until stop; the host table owns its context.
    pub unsafe fn start(
        product: &'static abi::rptadv_product_descriptor_v1,
        host: HostServicesOwner,
        control: *const abi::rptadv_control_descriptor_v1,
        file: *const abi::rptadv_file_descriptor,
        speech: *const abi::rptadv_speech_descriptor,
        configuration: &str,
    ) -> Result<Self, RuntimeError> {
        let Some(start) = product.start else {
            return Err(RuntimeError::InvalidDescriptor);
        };
        let (Some(reload), Some(stop)) = (product.reload, product.stop) else {
            return Err(RuntimeError::InvalidDescriptor);
        };
        if product.struct_size < size_of::<abi::rptadv_product_descriptor_v1>() as u32
            || product.abi_version != 3
            || product.capability != *b"rptadv.prod3\0\0\0\0"
        {
            return Err(RuntimeError::InvalidDescriptor);
        }
        let api = host.descriptor();
        let result = unsafe {
            start(
                api,
                control,
                file,
                speech,
                configuration.as_ptr().cast(),
                configuration.len(),
            )
        };
        if result != 0 {
            return Err(RuntimeError::Start);
        }
        Ok(Self {
            product,
            reload,
            stop,
            host: Some(host),
            providers: None,
        })
    }

    /// Load and start the full released-provider composition for one standalone process.
    pub fn start_with_providers(
        configuration: &str,
        radios: Vec<crate::ResolvedRadioNode>,
        secrets: crate::secrets::SecretsFile,
        providers: crate::providers::ProviderSet,
    ) -> Result<Self, RuntimeError> {
        let providers = Box::new(providers);
        let provider_ref: &'static crate::providers::ProviderSet =
            unsafe { &*(providers.as_ref() as *const crate::providers::ProviderSet) };
        provider_ref
            .validate_runtime()
            .map_err(RuntimeError::Provider)?;
        let descriptors = provider_ref
            .product_runtime_descriptors()
            .map_err(RuntimeError::Provider)?;
        let host = HostServicesOwner::new(radios, Some(provider_ref), secrets);
        // SAFETY: provider_ref points into the boxed owner moved into ProductRuntime below. The
        // product stops before host and provider boxes are dropped, including startup rollback.
        let mut runtime = unsafe {
            Self::start(
                descriptors.product,
                host,
                descriptors.control,
                descriptors.file,
                descriptors.speech,
                configuration,
            )?
        };
        runtime.providers = Some(providers);
        Ok(runtime)
    }

    /// Replace configuration without stopping the process; failed preparation keeps old state.
    pub fn reload(&self, configuration: &str) -> Result<(), RuntimeError> {
        (unsafe { (self.reload)(configuration.as_ptr().cast::<c_char>(), configuration.len()) }
            == 0)
            .then_some(())
            .ok_or(RuntimeError::Reload)
    }

    /// Reload product and host radio settings as one control-plane transaction.
    pub fn reload_with_radios(
        &mut self,
        configuration: &str,
        radios: Vec<crate::ResolvedRadioNode>,
    ) -> Result<(), RuntimeError> {
        let host = self.host.as_ref().ok_or(RuntimeError::InvalidDescriptor)?;
        if !host.stage_radios(radios) {
            return Err(RuntimeError::Reload);
        }
        match self.reload(configuration) {
            Ok(()) => {
                host.commit_radios();
                Ok(())
            }
            Err(error) => {
                host.discard_staged_radios();
                Err(error)
            }
        }
    }

    /// Ask current product policy whether one authenticated inbound peer may be admitted.
    pub fn authorize_incoming(&self, local: &str, remote: &str, source: &str) -> bool {
        let Some(authorize) = self.product.authorize_incoming else {
            return false;
        };
        let (Ok(local), Ok(remote), Ok(source)) = (
            std::ffi::CString::new(local),
            std::ffi::CString::new(remote),
            std::ffi::CString::new(source),
        ) else {
            return false;
        };
        unsafe {
            authorize(
                local.as_ptr(),
                local.as_bytes().len(),
                remote.as_ptr(),
                remote.as_bytes().len(),
                source.as_ptr(),
                source.as_bytes().len(),
            ) == 0
        }
    }

    /// Transfer one accepted inbound peer to the product, destroying it if admission is declined.
    pub fn incoming(
        &self,
        local: &str,
        remote: &str,
        source: &str,
        peer: crate::iax::IaxPeer,
    ) -> bool {
        let Some(incoming) = self.product.incoming else {
            return false;
        };
        let (Ok(local), Ok(remote), Ok(source)) = (
            std::ffi::CString::new(local),
            std::ffi::CString::new(remote),
            std::ffi::CString::new(source),
        ) else {
            return false;
        };
        let Some(host) = self.host.as_ref() else {
            return false;
        };
        let handle = host.peer_handle(peer);
        let result = unsafe {
            incoming(
                local.as_ptr(),
                local.as_bytes().len(),
                remote.as_ptr(),
                remote.as_bytes().len(),
                source.as_ptr(),
                source.as_bytes().len(),
                handle,
            )
        };
        if result == 0 || result == -1 {
            true
        } else {
            host.destroy_peer(handle);
            false
        }
    }

    /// Stop product workers before releasing host state and provider libraries.
    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if unsafe { (self.stop)() } != 0 {
            return Err(RuntimeError::Stop);
        }
        self.host.take();
        self.providers.take();
        Ok(())
    }
}

impl Drop for ProductRuntime {
    fn drop(&mut self) {
        let Some(host) = self.host.take() else {
            return;
        };
        if unsafe { (self.stop)() } != 0 {
            // A failed stop may still have callbacks into host state and provider code.
            // Retain both rather than unloading code under live callbacks.
            std::mem::forget(host);
            if let Some(providers) = self.providers.take() {
                std::mem::forget(providers);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProductRuntime, RuntimeError};
    use crate::{abi, host_services::HostServicesOwner, secrets::SecretsFile};
    use std::{
        ffi::c_char,
        mem::size_of,
        sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    };

    static STARTS: AtomicU32 = AtomicU32::new(0);
    static STOPS: AtomicU32 = AtomicU32::new(0);
    static RELOADS: AtomicU32 = AtomicU32::new(0);
    static FAILED_START_STOPS: AtomicU32 = AtomicU32::new(0);
    static CLEAN_DROP_STOPS: AtomicU32 = AtomicU32::new(0);
    static FAILED_STOP_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
    static INCOMING_RESULT: AtomicU32 = AtomicU32::new(0);
    static ACCEPTED_PEER: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn start_without_counts(
        _: *const abi::rptadv_host_services_v4,
        _: *const abi::rptadv_control_descriptor_v1,
        _: *const abi::rptadv_file_descriptor,
        _: *const abi::rptadv_speech_descriptor,
        _: *const c_char,
        _: usize,
    ) -> i32 {
        0
    }

    extern "C" fn stop_without_counts() -> i32 {
        0
    }
    static AUTHORIZED: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn start(
        _: *const abi::rptadv_host_services_v4,
        _: *const abi::rptadv_control_descriptor_v1,
        _: *const abi::rptadv_file_descriptor,
        _: *const abi::rptadv_speech_descriptor,
        configuration: *const c_char,
        length: usize,
    ) -> i32 {
        let bytes = unsafe { std::slice::from_raw_parts(configuration.cast::<u8>(), length) };
        STARTS.fetch_add(u32::from(bytes == b"[general]\n"), Ordering::Relaxed);
        0
    }

    unsafe extern "C" fn fail_start(
        _: *const abi::rptadv_host_services_v4,
        _: *const abi::rptadv_control_descriptor_v1,
        _: *const abi::rptadv_file_descriptor,
        _: *const abi::rptadv_speech_descriptor,
        _: *const c_char,
        _: usize,
    ) -> i32 {
        -1
    }

    unsafe extern "C" fn reload(_: *const c_char, _: usize) -> i32 {
        RELOADS.fetch_add(1, Ordering::Relaxed);
        0
    }

    unsafe extern "C" fn reject_reload(_: *const c_char, _: usize) -> i32 {
        -1
    }

    extern "C" fn stop() -> i32 {
        STOPS.fetch_add(1, Ordering::Relaxed);
        0
    }

    extern "C" fn failed_start_stop() -> i32 {
        FAILED_START_STOPS.fetch_add(1, Ordering::Relaxed);
        0
    }

    extern "C" fn fail_stop() -> i32 {
        FAILED_STOP_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        -1
    }

    extern "C" fn clean_drop_stop() -> i32 {
        CLEAN_DROP_STOPS.fetch_add(1, Ordering::Relaxed);
        0
    }

    unsafe extern "C" fn incoming(
        _: *const c_char,
        _: usize,
        _: *const c_char,
        _: usize,
        _: *const c_char,
        _: usize,
        peer: *mut std::ffi::c_void,
    ) -> i32 {
        let result = INCOMING_RESULT.load(Ordering::Relaxed) as i32;
        if result <= 0 {
            ACCEPTED_PEER.store(peer as usize, Ordering::Relaxed);
        }
        result
    }

    unsafe extern "C" fn authorize(
        local: *const c_char,
        local_length: usize,
        remote: *const c_char,
        remote_length: usize,
        source: *const c_char,
        source_length: usize,
    ) -> i32 {
        let local = unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) };
        let remote = unsafe { std::slice::from_raw_parts(remote.cast::<u8>(), remote_length) };
        let source = unsafe { std::slice::from_raw_parts(source.cast::<u8>(), source_length) };
        let allowed = local == b"524950" && remote == b"506315" && source == b"192.0.2.5";
        AUTHORIZED.store(u32::from(allowed), Ordering::Relaxed);
        if allowed { 0 } else { -1 }
    }

    fn descriptor(
        start: unsafe extern "C" fn(
            *const abi::rptadv_host_services_v4,
            *const abi::rptadv_control_descriptor_v1,
            *const abi::rptadv_file_descriptor,
            *const abi::rptadv_speech_descriptor,
            *const c_char,
            usize,
        ) -> i32,
        stop: extern "C" fn() -> i32,
        authorize: Option<
            unsafe extern "C" fn(
                *const c_char,
                usize,
                *const c_char,
                usize,
                *const c_char,
                usize,
            ) -> i32,
        >,
    ) -> &'static abi::rptadv_product_descriptor_v1 {
        descriptor_with_reload(start, reload, stop, authorize)
    }

    fn descriptor_with_reload(
        start: unsafe extern "C" fn(
            *const abi::rptadv_host_services_v4,
            *const abi::rptadv_control_descriptor_v1,
            *const abi::rptadv_file_descriptor,
            *const abi::rptadv_speech_descriptor,
            *const c_char,
            usize,
        ) -> i32,
        reload: unsafe extern "C" fn(*const c_char, usize) -> i32,
        stop: extern "C" fn() -> i32,
        authorize: Option<
            unsafe extern "C" fn(
                *const c_char,
                usize,
                *const c_char,
                usize,
                *const c_char,
                usize,
            ) -> i32,
        >,
    ) -> &'static abi::rptadv_product_descriptor_v1 {
        Box::leak(Box::new(abi::rptadv_product_descriptor_v1 {
            struct_size: size_of::<abi::rptadv_product_descriptor_v1>() as u32,
            abi_version: 3,
            capability: *b"rptadv.prod3\0\0\0\0",
            start: Some(start),
            reload: Some(reload),
            stop: Some(stop),
            authorize_incoming: authorize,
            incoming: None,
            link_command: None,
            link_status: None,
            digit: None,
        }))
    }

    fn descriptor_with_incoming() -> &'static abi::rptadv_product_descriptor_v1 {
        let mut product = *descriptor(start_without_counts, stop_without_counts, None);
        product.incoming = Some(incoming);
        Box::leak(Box::new(product))
    }

    #[test]
    fn incoming_authorization_passes_node_identity_and_remote_ip_to_product_policy() {
        AUTHORIZED.store(0, Ordering::Relaxed);
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, stop_without_counts, Some(authorize)),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        assert!(process.authorize_incoming("524950", "506315", "192.0.2.5"));
        assert_eq!(AUTHORIZED.load(Ordering::Relaxed), 1);
        assert!(!process.authorize_incoming("524950", "506316", "192.0.2.6"));
        assert_eq!(AUTHORIZED.load(Ordering::Relaxed), 0);
        assert!(!process.authorize_incoming("bad\0node", "506315", "192.0.2.5"));
        process.stop().unwrap();
    }

    #[test]
    fn incoming_is_denied_when_product_has_no_authorization_callback() {
        let process = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, stop_without_counts, None),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        assert!(!process.authorize_incoming("524950", "506315", "192.0.2.5"));
    }

    fn host() -> HostServicesOwner {
        HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap())
    }

    fn host_with_configuration(configuration: &str) -> HostServicesOwner {
        let document = rpt_advanced_core::config::ConfigDocument::parse(configuration).unwrap();
        HostServicesOwner::new(
            crate::resolve_radio_nodes(&document).unwrap().value,
            None,
            SecretsFile::parse("").unwrap(),
        )
    }

    fn radio_channel_available(process: &ProductRuntime, channel: &str) -> bool {
        let api = process.host.as_ref().unwrap().descriptor();
        let mut handle = std::ptr::null_mut();
        let result = unsafe {
            api.radio_open.unwrap()(
                api.context,
                channel.as_ptr().cast(),
                channel.len(),
                960,
                &mut handle,
            )
        };
        if result == 0 {
            unsafe { api.radio_destroy.unwrap()(api.context, handle) };
            true
        } else {
            false
        }
    }

    #[test]
    fn successful_reload_commits_candidate_radio_settings() {
        let active = "[1000]\nradio_channel=old\n[radio 1000]\n";
        let candidate = "[1000]\nradio_channel=new\n[radio 1000]\n";
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, stop_without_counts, None),
                host_with_configuration(active),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                active,
            )
        }
        .unwrap();

        assert!(radio_channel_available(&process, "old"));
        assert!(
            process
                .reload_with_radios(
                    candidate,
                    crate::resolve_radio_nodes(
                        &rpt_advanced_core::config::ConfigDocument::parse(candidate).unwrap()
                    )
                    .unwrap()
                    .value,
                )
                .is_ok()
        );
        assert!(!radio_channel_available(&process, "old"));
        assert!(radio_channel_available(&process, "new"));
        process.stop().unwrap();
    }

    #[test]
    fn rejected_reload_keeps_active_radio_settings() {
        let active = "[1000]\nradio_channel=old\n[radio 1000]\n";
        let candidate = "[1000]\nradio_channel=new\n[radio 1000]\n";
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor_with_reload(
                    start_without_counts,
                    reject_reload,
                    stop_without_counts,
                    None,
                ),
                host_with_configuration(active),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                active,
            )
        }
        .unwrap();

        assert!(matches!(
            process.reload_with_radios(
                candidate,
                crate::resolve_radio_nodes(
                    &rpt_advanced_core::config::ConfigDocument::parse(candidate).unwrap()
                )
                .unwrap()
                .value,
            ),
            Err(RuntimeError::Reload)
        ));
        assert!(radio_channel_available(&process, "old"));
        assert!(!radio_channel_available(&process, "new"));
        process.stop().unwrap();
    }

    #[test]
    fn product_start_reload_and_stop_keep_host_context_until_shutdown() {
        STARTS.store(0, Ordering::Relaxed);
        STOPS.store(0, Ordering::Relaxed);
        RELOADS.store(0, Ordering::Relaxed);
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor(start, stop, None),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        assert_eq!(STARTS.load(Ordering::Relaxed), 1);
        process.reload("[general]\n[524950]\n").unwrap();
        assert_eq!(RELOADS.load(Ordering::Relaxed), 1);
        process.stop().unwrap();
        assert_eq!(STOPS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn released_provider_composition_starts_and_stops_without_radio_hardware() {
        let providers = crate::providers::ProviderSet::load().unwrap();
        let mut process = ProductRuntime::start_with_providers(
            "[general]\nenabled=no\n",
            Vec::new(),
            SecretsFile::parse("").unwrap(),
            providers,
        )
        .unwrap();

        process.stop().unwrap();
    }

    #[test]
    fn failed_product_start_releases_host_without_claiming_running_state() {
        FAILED_START_STOPS.store(0, Ordering::Relaxed);
        assert!(matches!(
            unsafe {
                ProductRuntime::start(
                    descriptor(fail_start, failed_start_stop, None),
                    host(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    "[general]\n",
                )
            },
            Err(RuntimeError::Start)
        ));
        assert_eq!(FAILED_START_STOPS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn runtime_error_messages_are_stable() {
        for (error, expected) in [
            (
                RuntimeError::InvalidDescriptor,
                "product lifecycle descriptor is incomplete",
            ),
            (
                RuntimeError::Provider(crate::providers::ProviderError::Load("test")),
                "cannot load required provider test",
            ),
            (RuntimeError::Start, "standalone product startup failed"),
            (
                RuntimeError::Reload,
                "standalone configuration reload failed",
            ),
            (RuntimeError::Stop, "standalone product shutdown failed"),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn start_rejects_each_invalid_lifecycle_descriptor_field() {
        let valid = *descriptor(start_without_counts, stop_without_counts, None);
        let mut invalid = Vec::new();
        let mut item = valid;
        item.start = None;
        invalid.push(item);
        let mut item = valid;
        item.reload = None;
        invalid.push(item);
        let mut item = valid;
        item.stop = None;
        invalid.push(item);
        let mut item = valid;
        item.struct_size -= 1;
        invalid.push(item);
        let mut item = valid;
        item.abi_version += 1;
        invalid.push(item);
        let mut item = valid;
        item.capability[0] = b'x';
        invalid.push(item);

        for product in invalid {
            let product = Box::leak(Box::new(product));
            assert!(matches!(
                unsafe {
                    ProductRuntime::start(
                        product,
                        host(),
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null(),
                        "[general]\n",
                    )
                },
                Err(RuntimeError::InvalidDescriptor)
            ));
        }
    }

    #[test]
    fn reload_with_radios_rejects_missing_host_and_already_staged_settings() {
        let active = "[1000]\nradio_channel=old\n[radio 1000]\n";
        let candidate = "[1000]\nradio_channel=new\n[radio 1000]\n";
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, stop_without_counts, None),
                host_with_configuration(active),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                active,
            )
        }
        .unwrap();
        process.host.as_ref().unwrap().stage_radios(Vec::new());
        assert!(matches!(
            process.reload_with_radios(candidate, Vec::new()),
            Err(RuntimeError::Reload)
        ));
        process.host.as_ref().unwrap().discard_staged_radios();
        process.stop().unwrap();
        assert!(matches!(
            process.reload_with_radios(candidate, Vec::new()),
            Err(RuntimeError::InvalidDescriptor)
        ));
    }

    #[test]
    fn incoming_transfers_only_accepted_peers_and_rejects_invalid_inputs() {
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor_with_incoming(),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        for result in [0, u32::MAX] {
            INCOMING_RESULT.store(result, Ordering::Relaxed);
            ACCEPTED_PEER.store(0, Ordering::Relaxed);
            assert!(process.incoming(
                "524950",
                "506315",
                "192.0.2.5",
                crate::iax::tests::runtime_test_peer(),
            ));
            let peer = ACCEPTED_PEER.swap(0, Ordering::Relaxed) as *mut std::ffi::c_void;
            process.host.as_ref().unwrap().destroy_peer(peer);
        }
        INCOMING_RESULT.store(1, Ordering::Relaxed);
        assert!(!process.incoming(
            "524950",
            "506315",
            "192.0.2.5",
            crate::iax::tests::runtime_test_peer(),
        ));
        assert!(!process.incoming(
            "bad\0node",
            "506315",
            "192.0.2.5",
            crate::iax::tests::runtime_test_peer(),
        ));
        process.stop().unwrap();
        assert!(!process.incoming(
            "524950",
            "506315",
            "192.0.2.5",
            crate::iax::tests::runtime_test_peer(),
        ));
    }

    #[test]
    fn incoming_is_rejected_when_product_has_no_incoming_callback() {
        let process = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, stop_without_counts, None),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();

        assert!(!process.incoming(
            "524950",
            "506315",
            "192.0.2.5",
            crate::iax::tests::runtime_test_peer(),
        ));
    }

    #[test]
    fn dropping_runtime_stops_or_retains_owners_on_stop_failure() {
        CLEAN_DROP_STOPS.store(0, Ordering::Relaxed);
        let clean = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, clean_drop_stop, None),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        drop(clean);
        assert_eq!(CLEAN_DROP_STOPS.load(Ordering::Relaxed), 1);

        FAILED_STOP_ATTEMPTS.store(0, Ordering::Relaxed);
        let mut failed = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, fail_stop, None),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        failed.providers = Some(Box::new(crate::providers::ProviderSet::load().unwrap()));
        assert_eq!(failed.stop(), Err(RuntimeError::Stop));
        drop(failed);
        let failed_without_providers = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, fail_stop, None),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        drop(failed_without_providers);
        assert_eq!(FAILED_STOP_ATTEMPTS.load(Ordering::Relaxed), 3);
    }
}
