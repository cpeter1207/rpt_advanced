//! Process-lifetime owner for the portable product.

use crate::{abi, host_services::HostServicesOwner};
use rptadv_peer_support::reload::ReloadOutcome;
use std::{
    ffi::c_char,
    mem::size_of,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Product startup, reload, or shutdown failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// The product lifecycle table is incomplete.
    InvalidDescriptor,
    /// A required shared provider is missing or incompatible.
    Provider(crate::providers::ProviderError),
    /// EchoLink account preparation or notification drainage failed.
    Echo(rptadv_peer_support::echolink::host::Error),
    /// Account preparation was rejected before candidate publication.
    EchoUpdate(rptadv_peer_support::echolink::host::UpdateError),
    /// Product start rejected the selected providers or configuration.
    Start,
    /// Product could not prepare the replacement configuration.
    Reload,
    /// Product publication failed operationally or is indeterminate; raw status is retained.
    ReloadOutcome(ReloadOutcome),
    /// Product published, but account follow-up failed with retained callback ownership.
    EchoCommitted(rptadv_peer_support::echolink::host::UpdateOutcome),
    /// Product could not quiesce all runtime owners.
    Stop,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDescriptor => "product lifecycle descriptor is incomplete",
            Self::Start => "standalone product startup failed",
            Self::Reload => "standalone configuration reload failed",
            Self::ReloadOutcome(outcome) => {
                return write!(formatter, "reload outcome: {outcome:?}");
            }
            Self::EchoCommitted(outcome) => {
                return write!(formatter, "committed EchoLink update: {outcome:?}");
            }
            Self::Stop => "standalone product shutdown failed",
            Self::Provider(error) => return error.fmt(formatter),
            Self::Echo(error) => {
                return write!(formatter, "EchoLink account operation failed: {error:?}");
            }
            Self::EchoUpdate(error) => {
                return write!(formatter, "EchoLink reload failed: {error:?}");
            }
        })
    }
}

impl std::error::Error for RuntimeError {}
impl RuntimeError {
    /// Publication classification for outer radio/listener ownership decisions.
    pub(crate) fn publication(self) -> ReloadOutcome {
        match self {
            Self::ReloadOutcome(outcome) => outcome,
            Self::EchoCommitted(outcome) => outcome.product,
            _ => ReloadOutcome::Rejected,
        }
    }
}

/// Running product and host context. Product stop precedes host/provider release.
pub struct ProductRuntime {
    product: &'static abi::rptadv_product_descriptor_v1,
    reload: unsafe extern "C" fn(*const c_char, usize) -> i32,
    stop: unsafe extern "C" fn() -> i32,
    host: Option<HostServicesOwner>,
    providers: Option<Box<crate::providers::ProviderSet>>,
    composition: Option<crate::providers::ProductRuntimeDescriptors>,
    configuration: Mutex<String>,
    configuration_path: Option<PathBuf>,
    reload_fault: std::sync::atomic::AtomicI32,
}

impl ProductRuntime {
    /// Borrow the retained immutable product table for local composition adapters.
    pub(crate) fn product_descriptor(&self) -> &abi::rptadv_product_descriptor_v1 {
        self.product
    }

    /// Copy the last successfully committed document, never a staged candidate.
    pub(crate) fn configuration_text(&self) -> String {
        self.active_configuration().clone()
    }

    fn active_configuration(&self) -> std::sync::MutexGuard<'_, String> {
        self.configuration
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Fence a published configuration whose outer listener follow-up failed.
    pub(crate) fn fence_reload(&self) {
        self.reload_fault
            .store(-3, std::sync::atomic::Ordering::Release);
    }

    /// Fatal reloads close foreground admission until orderly shutdown.
    pub(crate) fn reload_fatal(&self) -> bool {
        self.reload_fault.load(std::sync::atomic::Ordering::Acquire) != 0
    }

    pub(crate) fn reload_failure(&self) -> Option<RuntimeError> {
        let status = self.reload_fault.load(std::sync::atomic::Ordering::Acquire);
        (status != 0).then_some(RuntimeError::ReloadOutcome(ReloadOutcome::Fatal(status)))
    }

    /// Fixed service configuration path; a maintenance caller cannot replace it.
    pub(crate) fn fixed_configuration_path(&self) -> Option<&Path> {
        self.configuration_path.as_deref()
    }

    /// Activate readiness routing while keeping listener admission on control.
    pub(crate) fn enable_listener(
        &self,
        listener: &mut crate::iax::IaxServer,
    ) -> Result<(), crate::iax::IaxError> {
        self.host
            .as_ref()
            .ok_or(crate::iax::IaxError::Operation)?
            .enable_listener(listener)
    }

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
        let (Some(reload), Some(stop), Some(_)) =
            (product.reload, product.stop, product.reload_poll)
        else {
            return Err(RuntimeError::InvalidDescriptor);
        };
        if product.struct_size < size_of::<abi::rptadv_product_descriptor_v1>() as u32
            || product.abi_version != 8
            || product.capability != *b"rptadv.prod8\0\0\0\0"
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
            composition: None,
            configuration: Mutex::new(configuration.to_owned()),
            configuration_path: None,
            reload_fault: std::sync::atomic::AtomicI32::new(0),
        })
    }

    /// Load and start the full released-provider composition for one standalone process.
    pub fn start_with_providers(
        configuration_path: &std::path::Path,
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
        let mut host = HostServicesOwner::new(radios, Some(provider_ref), secrets)
            .map_err(RuntimeError::Provider)?;
        with_echo_preparation(configuration_path, &descriptors, |preparation| {
            unsafe { host.prepare_echo(descriptors.product, configuration, preparation) }
                .map_err(RuntimeError::Echo)
        })?;
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
        runtime.composition = Some(descriptors);
        runtime.configuration_path = Some(configuration_path.to_owned());
        unsafe { runtime.host.as_ref().unwrap().activate_echo() }.map_err(RuntimeError::Echo)?;
        Ok(runtime)
    }

    /// Replace configuration without stopping the process; failed preparation keeps old state.
    pub fn reload(&self, configuration: &str) -> Result<(), RuntimeError> {
        let fault = self.reload_fault.load(std::sync::atomic::Ordering::Acquire);
        if fault != 0 {
            return Err(RuntimeError::ReloadOutcome(ReloadOutcome::Fatal(fault)));
        }
        let mut active = self.active_configuration();
        let host = self.host.as_ref().ok_or(RuntimeError::InvalidDescriptor)?;
        let apply = || self.reload_product(configuration);
        let outcome = if let Some(providers) = &self.providers {
            providers
                .validate_runtime()
                .map_err(RuntimeError::Provider)?;
            let descriptors = self
                .composition
                .as_ref()
                .ok_or(RuntimeError::InvalidDescriptor)?;
            let path = self
                .configuration_path
                .as_deref()
                .ok_or(RuntimeError::InvalidDescriptor)?;
            let outcome = with_echo_preparation(path, descriptors, |preparation| {
                unsafe { host.reload_echo(self.product, configuration, preparation, apply) }
                    .map_err(RuntimeError::EchoUpdate)
            })?;
            if outcome.product.published() {
                *active = configuration.to_owned();
            }
            if let ReloadOutcome::Fatal(status) = outcome.product {
                self.reload_fault
                    .store(status, std::sync::atomic::Ordering::Release);
            }
            if outcome.failure.is_some() || outcome.retirement.is_some() {
                return Err(RuntimeError::EchoCommitted(outcome));
            }
            outcome.product
        } else {
            apply()
        };
        if outcome.published() {
            *active = configuration.to_owned();
        }
        match outcome {
            ReloadOutcome::Committed => Ok(()),
            ReloadOutcome::Rejected => Err(RuntimeError::Reload),
            ReloadOutcome::Fatal(status) => {
                self.reload_fault
                    .store(status, std::sync::atomic::Ordering::Release);
                Err(RuntimeError::ReloadOutcome(outcome))
            }
            ReloadOutcome::CommittedWithError => Err(RuntimeError::ReloadOutcome(outcome)),
        }
    }
    fn reload_product(&self, configuration: &str) -> ReloadOutcome {
        // The caller retains staged owners throughout every deferred poll.
        let mut status =
            unsafe { (self.reload)(configuration.as_ptr().cast(), configuration.len()) };
        loop {
            if let Some(outcome) = ReloadOutcome::from_status(status) {
                return outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            status = unsafe { self.product.reload_poll.unwrap()() };
        }
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
                match error.publication() {
                    ReloadOutcome::Committed | ReloadOutcome::CommittedWithError => {
                        host.commit_radios()
                    }
                    ReloadOutcome::Rejected => host.discard_staged_radios(),
                    ReloadOutcome::Fatal(_) => {}
                }
                Err(error)
            }
        }
    }

    /// Ask current product policy whether one authenticated inbound peer may be admitted.
    pub fn authorize_incoming(&self, local: &str, remote: &str, source: &str) -> bool {
        let Some(authorize) = self.product.authorize_incoming else {
            return false;
        };
        if [local, remote, source]
            .iter()
            .any(|value| value.as_bytes().contains(&0))
        {
            return false;
        }
        unsafe {
            authorize(
                &incoming_request(local, remote, source),
                std::ptr::null_mut(),
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
        if [local, remote, source]
            .iter()
            .any(|value| value.as_bytes().contains(&0))
        {
            return false;
        }
        let Some(host) = self.host.as_ref() else {
            return false;
        };
        let handle = host.peer_handle(peer, local, remote);
        let result = unsafe { incoming(&incoming_request(local, remote, source), handle) };
        if result == 0 || result == -1 {
            true
        } else {
            host.destroy_peer(handle);
            false
        }
    }

    /// Stop product workers before releasing host state and provider libraries.
    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if let Some(host) = &self.host {
            host.stop_echo().map_err(RuntimeError::Echo)?;
        }
        if unsafe { (self.stop)() } != 0 {
            return Err(RuntimeError::Stop);
        }
        self.host.take();
        self.composition.take();
        self.providers.take();
        Ok(())
    }
}

fn with_echo_preparation<T>(
    path: &Path,
    descriptors: &crate::providers::ProductRuntimeDescriptors,
    operation: impl FnOnce(
        rptadv_peer_support::echolink::host::Preparation<'_>,
    ) -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    let jitter = Path::new(env!("RPT_ADVANCED_LIBDIR")).join("librptadv_jitter_adapter.so.1");
    let span = |value: &str| rptadv_peer_support::echolink::abi::ByteSpan {
        data: value.as_ptr(),
        length: value.len(),
    };
    let resources = rptadv_peer_support::echolink::abi::RuntimeConfig {
        gsm_library: span("librptadv_codec_gsm.so.1"),
        speex_library: span("librptadv_codec_speex.so.1"),
        jitter_library: span(
            jitter
                .to_str()
                .expect("UTF-8 build path with an ASCII filename"),
        ),
        ..Default::default()
    };
    let incoming = descriptors
        .product
        .incoming
        .expect("validated product composition retains incoming admission");
    operation(unsafe {
        rptadv_peer_support::echolink::host::Preparation::new(
            path,
            Path::new("librptadv_echolink.so.1"),
            &resources,
            std::ptr::from_ref(descriptors.control).cast(),
            move |peer| {
                crate::host_services::incoming_echo(incoming, peer);
            },
        )
        .with_notice(|message| eprintln!("rpt-advanced: EchoLink: {message}"))
    })
}

fn incoming_request(
    local: &str,
    remote: &str,
    source: &str,
) -> crate::abi::rptadv_incoming_peer_v1 {
    crate::abi::rptadv_incoming_peer_v1 {
        struct_size: std::mem::size_of::<crate::abi::rptadv_incoming_peer_v1>() as u32,
        abi_version: 1,
        protocol: 1,
        local: crate::abi::rptadv_peer_bytes_v1 {
            data: local.as_ptr(),
            length: local.len(),
        },
        remote: crate::abi::rptadv_peer_bytes_v1 {
            data: remote.as_ptr(),
            length: remote.len(),
        },
        source: crate::abi::rptadv_peer_bytes_v1 {
            data: source.as_ptr(),
            length: source.len(),
        },
    }
}

impl Drop for ProductRuntime {
    fn drop(&mut self) {
        if self.host.is_some() && self.stop().is_err() {
            // A failed stop may still have callbacks into host state and provider code.
            // Retain both rather than unloading code under live callbacks.
            std::mem::forget(self.host.take());
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
        ffi::{c_char, c_void},
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
        _: *const abi::rptadv_host_services_v10,
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
        _: *const abi::rptadv_host_services_v10,
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
        _: *const abi::rptadv_host_services_v10,
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
    unsafe extern "C" fn reload_ready() -> i32 {
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
        _: *const abi::rptadv_incoming_peer_v1,
        peer: *mut std::ffi::c_void,
    ) -> i32 {
        let result = INCOMING_RESULT.load(Ordering::Relaxed) as i32;
        if result <= 0 {
            ACCEPTED_PEER.store(peer as usize, Ordering::Relaxed);
        }
        result
    }

    unsafe extern "C" fn authorize(
        request: *const abi::rptadv_incoming_peer_v1,
        _: *mut c_void,
    ) -> i32 {
        let request = unsafe { &*request };
        let local = unsafe { std::slice::from_raw_parts(request.local.data, request.local.length) };
        let remote =
            unsafe { std::slice::from_raw_parts(request.remote.data, request.remote.length) };
        let source =
            unsafe { std::slice::from_raw_parts(request.source.data, request.source.length) };
        let allowed = local == b"524950" && remote == b"506315" && source == b"192.0.2.5";
        AUTHORIZED.store(u32::from(allowed), Ordering::Relaxed);
        if allowed { 0 } else { -1 }
    }

    fn descriptor(
        start: unsafe extern "C" fn(
            *const abi::rptadv_host_services_v10,
            *const abi::rptadv_control_descriptor_v1,
            *const abi::rptadv_file_descriptor,
            *const abi::rptadv_speech_descriptor,
            *const c_char,
            usize,
        ) -> i32,
        stop: extern "C" fn() -> i32,
        authorize: Option<
            unsafe extern "C" fn(*const abi::rptadv_incoming_peer_v1, *mut c_void) -> i32,
        >,
    ) -> &'static abi::rptadv_product_descriptor_v1 {
        descriptor_with_reload(start, reload, stop, authorize)
    }

    fn descriptor_with_reload(
        start: unsafe extern "C" fn(
            *const abi::rptadv_host_services_v10,
            *const abi::rptadv_control_descriptor_v1,
            *const abi::rptadv_file_descriptor,
            *const abi::rptadv_speech_descriptor,
            *const c_char,
            usize,
        ) -> i32,
        reload: unsafe extern "C" fn(*const c_char, usize) -> i32,
        stop: extern "C" fn() -> i32,
        authorize: Option<
            unsafe extern "C" fn(*const abi::rptadv_incoming_peer_v1, *mut c_void) -> i32,
        >,
    ) -> &'static abi::rptadv_product_descriptor_v1 {
        Box::leak(Box::new(abi::rptadv_product_descriptor_v1 {
            struct_size: size_of::<abi::rptadv_product_descriptor_v1>() as u32,
            abi_version: 8,
            capability: *b"rptadv.prod8\0\0\0\0",
            start: Some(start),
            reload: Some(reload),
            stop: Some(stop),
            authorize_incoming: authorize,
            incoming: None,
            link_command: None,
            site_command: None,
            radio_control: None,
            link_status: None,
            digit: None,
            inspect_configuration: None,
            inspect_secrets: None,
            reload_poll: Some(reload_ready),
        }))
    }

    fn descriptor_with_incoming() -> &'static abi::rptadv_product_descriptor_v1 {
        let mut product = *descriptor(start_without_counts, stop_without_counts, None);
        product.incoming = Some(incoming);
        Box::leak(Box::new(product))
    }

    #[test]
    fn local_tuner_uses_the_fixed_path_and_typed_product_operation() {
        use serde_json::json;
        unsafe extern "C" fn maintenance(
            node: *const c_char,
            length: usize,
            request: *mut abi::rptadv_radio_control_v1,
            sink: abi::rptadv_text_sink_v1,
            context: *mut c_void,
        ) -> i32 {
            // SAFETY: runtime retains its exclusively writable sized request.
            let request = unsafe { &mut *request };
            let node = unsafe { std::slice::from_raw_parts(node.cast::<u8>(), length) };
            if (request.operation == 3 && request.keyed != 0) || node == b"9999" {
                return -6;
            }
            request.command.value = 777;
            if let Some(sink) = sink {
                let text = c"physical status";
                unsafe { sink(context, text.as_ptr(), text.to_bytes().len()) };
            }
            0
        }
        let mut api = *descriptor(start_without_counts, stop_without_counts, None);
        api.radio_control = Some(maintenance);
        let source = "[1000]\nradio_channel=usb\n";
        let mut process = unsafe {
            ProductRuntime::start(
                Box::leak(Box::new(api)),
                host(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                source,
            )
        }
        .unwrap();
        assert_eq!(process.configuration_text(), source);
        assert!(
            process
                .local_radio_request(&json!({"operation":"configuration-path"}))
                .is_err()
        );
        let path = std::env::temp_dir().join(format!("rpt-radio-config-{}", std::process::id()));
        std::fs::write(&path, source).unwrap();
        process.configuration_path = Some(path.clone());
        assert_eq!(
            process
                .local_radio_request(&json!({"operation":"configuration-path"}))
                .unwrap(),
            json!(path.canonicalize().unwrap().to_str().unwrap())
        );
        assert_eq!(
            process
                .local_radio_request(&json!({"operation":"nodes"}))
                .unwrap(),
            json!(["1000"])
        );
        *process.configuration.lock().unwrap() = "malformed configuration".into();
        assert!(
            process
                .local_radio_request(&json!({"operation":"nodes"}))
                .is_err()
        );
        *process.configuration.lock().unwrap() = source.into();
        assert!(
            process
                .local_radio_request(&json!({"operation":"status"}))
                .is_err()
        );
        assert!(process.local_radio_request(&json!({})).is_err());
        for operation in ["status", "processing-stats", "active-configuration"] {
            assert_eq!(
                process
                    .local_radio_request(&json!({"operation":operation,"node":"1000"}))
                    .unwrap(),
                json!("physical status")
            );
        }
        let reply = process
            .local_radio_request(&json!({"operation":"command","node":"1000",
            "command":1,"target":1,"value":0,"flags":0}))
            .unwrap();
        assert_eq!(reply["value"], 777);
        assert!(
            process
                .local_radio_request(&json!({"operation":"transmit","node":"1000","keyed":true}))
                .unwrap_err()
                .contains("-6")
        );
        process.clear_local_manual("1000");
        process.clear_local_manual("9999");
        assert_eq!(
            process
                .local_radio_request(&json!({"operation":"transmit","node":"1000","keyed":false}))
                .unwrap(),
            serde_json::Value::Null
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            process
                .local_radio_request(&json!({"operation":"configuration-path"}))
                .is_err()
        );
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = process.configuration.lock().unwrap();
            panic!("failed control preparation");
        }));
        assert_eq!(process.configuration_text(), source);
        process.stop().unwrap();
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
        HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap()
    }

    fn host_with_configuration(configuration: &str) -> HostServicesOwner {
        HostServicesOwner::new(
            crate::resolve_radio_nodes(configuration).unwrap(),
            None,
            SecretsFile::parse("").unwrap(),
        )
        .unwrap()
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
                .reload_with_radios(candidate, crate::resolve_radio_nodes(candidate).unwrap(),)
                .is_ok()
        );
        assert!(!radio_channel_available(&process, "old"));
        assert!(radio_channel_available(&process, "new"));
        process.stop().unwrap();
    }

    #[test]
    fn pending_reload_retains_staged_radios_until_ready_or_rollback() {
        use std::cell::Cell;
        fn nodes(channel: &str) -> Vec<crate::ResolvedRadioNode> {
            // No hardware is opened: this test exercises host transaction ownership.
            let mut radio: abi::rptadv_radio_session_config = unsafe { std::mem::zeroed() };
            radio.struct_size = std::mem::size_of_val(&radio) as u32;
            radio.abi_version = 5;
            let mut config: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
            config.struct_size = std::mem::size_of_val(&config) as u32;
            config.abi_version = abi::URP_NATIVE_CONFIG_ABI_VERSION;
            config.radio = &radio;
            vec![crate::ResolvedRadioNode {
                node: "1000".into(),
                channel: channel.into(),
                enabled: true,
                iax_port: 4569,
                registration_url: String::new(),
                registration_interval_seconds: 300,
                radio: unsafe {
                    crate::native_configuration::NativeRadioConfiguration::copy_from(&config)
                }
                .unwrap(),
            }]
        }
        thread_local! {
            static POLLS: Cell<u32> = const { Cell::new(0) };
            static FAIL: Cell<bool> = const { Cell::new(false) };
        }
        unsafe extern "C" fn pending(_: *const c_char, _: usize) -> i32 {
            1
        }
        unsafe extern "C" fn poll() -> i32 {
            let count = POLLS.get();
            POLLS.set(count + 1);
            if count < 2 {
                1
            } else if FAIL.get() {
                -1
            } else {
                0
            }
        }
        let active = "[1000]\nradio_channel=old\n[radio 1000]\n";
        let candidate = "[1000]\nradio_channel=new\n[radio 1000]\n";
        for fail in [false, true] {
            POLLS.set(0);
            FAIL.set(fail);
            let mut api =
                *descriptor_with_reload(start_without_counts, pending, stop_without_counts, None);
            api.reload_poll = Some(poll);
            let mut process = unsafe {
                ProductRuntime::start(
                    Box::leak(Box::new(api)),
                    HostServicesOwner::new(nodes("old"), None, SecretsFile::parse("").unwrap())
                        .unwrap(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    active,
                )
            }
            .unwrap();
            let result = process.reload_with_radios(candidate, nodes("new"));
            assert_eq!(result.is_err(), fail);
            assert_eq!(radio_channel_available(&process, "old"), fail);
            assert_eq!(radio_channel_available(&process, "new"), !fail);
            assert!(POLLS.get() >= 3);
            process.stop().unwrap();
        }
    }

    #[test]
    fn reload_terminal_disposition_never_replays_the_previous_document() {
        use std::cell::{Cell, RefCell};
        thread_local! {
            static SEEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
            static STATUS: Cell<i32> = const { Cell::new(0) };
            static DEFERRED: Cell<bool> = const { Cell::new(false) };
        }
        unsafe extern "C" fn reload(configuration: *const c_char, length: usize) -> i32 {
            let text = unsafe {
                std::str::from_utf8(std::slice::from_raw_parts(
                    configuration.cast::<u8>(),
                    length,
                ))
            }
            .unwrap();
            SEEN.with_borrow_mut(|seen| seen.push(text.to_owned()));
            if DEFERRED.get() { 1 } else { STATUS.get() }
        }
        unsafe extern "C" fn poll() -> i32 {
            STATUS.get()
        }
        for deferred in [false, true] {
            for status in [0, -1, -2, -3, 73] {
                SEEN.with_borrow_mut(Vec::clear);
                STATUS.set(status);
                DEFERRED.set(deferred);
                let mut api = *descriptor_with_reload(
                    start_without_counts,
                    reload,
                    stop_without_counts,
                    None,
                );
                api.reload_poll = Some(poll);
                let mut process = unsafe {
                    ProductRuntime::start(
                        Box::leak(Box::new(api)),
                        host(),
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null(),
                        "original",
                    )
                }
                .unwrap();
                let result = process.reload("candidate");
                assert_eq!(result.is_ok(), status == 0);
                assert_eq!(SEEN.with_borrow(Clone::clone), ["candidate"]);
                assert_eq!(
                    process.configuration_text(),
                    if matches!(status, 0 | -2) {
                        "candidate"
                    } else {
                        "original"
                    }
                );
                if matches!(status, -3 | 73) {
                    assert!(process.reload("must-not-run").is_err());
                    assert_eq!(SEEN.with_borrow(Clone::clone), ["candidate"]);
                }
                process.stop().unwrap();
            }
        }
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
            process.reload_with_radios(candidate, crate::resolve_radio_nodes(candidate).unwrap(),),
            Err(RuntimeError::Reload)
        ));
        assert!(radio_channel_available(&process, "old"));
        assert!(!radio_channel_available(&process, "new"));
        process.stop().unwrap();
    }

    #[test]
    fn incompatible_provider_reload_preserves_active_configuration() {
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
        process.providers = Some(Box::new(crate::providers::tests::empty_provider_set()));
        assert!(matches!(
            process.reload_with_radios(candidate, crate::resolve_radio_nodes(candidate).unwrap()),
            Err(RuntimeError::Provider(_))
        ));
        assert!(radio_channel_available(&process, "old"));
        assert!(!radio_channel_available(&process, "new"));
        process.stop().unwrap();
    }

    #[test]
    fn provider_composition_propagates_product_start_failure() {
        use crate::providers::{ProviderSet, tests};
        let mut providers = ProviderSet::load().unwrap();
        let inspection = crate::providers::ProductInspection::open().unwrap();
        let mut product = *inspection.api();
        product.start = Some(fail_start);
        tests::replace_descriptor(&mut providers, "librptadv_product.so.3", product);
        assert!(matches!(
            ProductRuntime::start_with_providers(
                std::path::Path::new("rpt_advanced.conf"),
                "[general]\n",
                Vec::new(),
                SecretsFile::parse("").unwrap(),
                providers,
            ),
            Err(RuntimeError::Start)
        ));
    }

    #[test]
    fn provider_startup_reports_manifest_and_resolver_initialization_failures() {
        use crate::providers::{ProviderError, ProviderSet, tests};
        unsafe extern "C" fn reject_create(_: *mut *mut std::ffi::c_void) -> i32 {
            -1
        }
        let mut providers = ProviderSet::load().unwrap();
        let mut resolver = *providers.resolver_descriptor().unwrap();
        resolver.create = Some(reject_create);
        tests::replace_descriptor(&mut providers, "librptadv_resolver_adapter.so.1", resolver);
        for (providers, expected) in [
            (
                tests::empty_provider_set(),
                ProviderError::MissingDescriptor("librptadv_product.so.3"),
            ),
            (
                providers,
                ProviderError::Initialize("librptadv_resolver_adapter.so.1"),
            ),
        ] {
            assert_eq!(
                ProductRuntime::start_with_providers(
                    std::path::Path::new("rpt_advanced.conf"),
                    "[general]\n",
                    Vec::new(),
                    SecretsFile::parse("").unwrap(),
                    providers,
                )
                .err(),
                Some(RuntimeError::Provider(expected))
            );
        }
    }

    #[test]
    fn resolver_context_is_destroyed_only_after_product_stop() {
        use std::ffi::c_void;
        use std::sync::atomic::AtomicBool;
        static LIVE: AtomicBool = AtomicBool::new(false);
        unsafe extern "C" fn create(context: *mut *mut c_void) -> i32 {
            unsafe { *context = Box::into_raw(Box::new(())).cast() };
            LIVE.store(true, Ordering::Relaxed);
            0
        }
        unsafe extern "C" fn destroy(context: *mut c_void) {
            unsafe { drop(Box::from_raw(context.cast::<()>())) };
            LIVE.store(false, Ordering::Relaxed);
        }
        extern "C" fn stop() -> i32 {
            assert!(
                LIVE.load(Ordering::Relaxed),
                "resolver released before product callbacks stop"
            );
            0
        }
        let providers = crate::providers::ProviderSet::load().unwrap();
        let mut resolver = *providers.resolver_descriptor().unwrap();
        resolver.create = Some(create);
        resolver.destroy = Some(destroy);
        let host = host();
        // No callbacks have started; replace only this fixture's resolver context.
        unsafe {
            (*host
                .descriptor()
                .context
                .cast::<crate::host_services::StandaloneHostContext>())
            .resolver =
                Some(crate::directory::Resolver::new(Box::leak(Box::new(resolver))).unwrap());
        }
        let mut process = unsafe {
            ProductRuntime::start(
                descriptor(start_without_counts, stop, None),
                host,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        process.providers = Some(Box::new(providers));
        process.stop().unwrap();
        assert!(!LIVE.load(Ordering::Relaxed));
        assert!(process.providers.is_none());
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
        process.stop().unwrap();
        assert_eq!(STOPS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn released_provider_composition_starts_and_stops_without_radio_hardware() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            os::unix::fs::PermissionsExt,
            time::Duration,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (requests, observed) = std::sync::mpsc::channel();
        let directory = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket.write_all(b"12345678").unwrap();
            socket.read_exact(&mut [0; 24]).unwrap();
            let receive = |socket: &mut std::net::TcpStream| {
                let mut header = [0; 9];
                socket.read_exact(&mut header).unwrap();
                let mut payload =
                    vec![0; u32::from_le_bytes(header[5..].try_into().unwrap()) as usize];
                socket.read_exact(&mut payload).unwrap();
                (header[0], payload)
            };
            let send = |socket: &mut std::net::TcpStream, kind, payload: &[u8]| {
                let mut packet = vec![kind, 0, 0, 0, 0];
                packet.extend((payload.len() as u32).to_le_bytes());
                packet.extend(payload);
                socket.write_all(&packet).unwrap();
            };
            for response in [b"OK".as_slice(), b"@@@\n0\n+++\n", b"OK", b"OK"] {
                assert_eq!(receive(&mut socket).0, 1);
                send(&mut socket, 4, &[0; 4]);
                let (kind, request) = receive(&mut socket);
                assert_eq!(kind, 2);
                send(&mut socket, 2, response);
                assert_eq!(receive(&mut socket).0, 3);
                send(&mut socket, 3, &[]);
                requests.send(request).unwrap();
            }
        });
        let root =
            std::env::temp_dir().join(format!("rpt-echo-host-{}-{}", std::process::id(), port));
        std::fs::create_dir(&root).unwrap();
        let secrets_path = root.join("echolink-secrets.conf");
        std::fs::write(
            &secrets_path,
            "[1000]\ndirectory_password=fixture-value\nproxy_password=fixture-proxy\n",
        )
        .unwrap();
        std::fs::set_permissions(&secrets_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let configuration = format!(
            "[1000]\nnode_enabled=no\n[echolink 1000]\nenabled=yes\ncallsign=K1ABC-R\nnode_number=123456\nname=Before\ntransport=proxy\nproxy_host=127.0.0.1\nproxy_port={port}\ndirectory_servers=127.0.0.1\naprs_server=\n"
        );
        let providers = crate::providers::ProviderSet::load().unwrap();
        let mut process = ProductRuntime::start_with_providers(
            &root.join("rpt_advanced.conf"),
            &configuration,
            Vec::new(),
            SecretsFile::parse("").unwrap(),
            providers,
        )
        .unwrap();
        let login = observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let snapshot = observed.recv_timeout(Duration::from_secs(5)).unwrap();
        process
            .reload(&configuration.replace("name=Before", "name=After"))
            .unwrap();
        let renewed = observed.recv_timeout(Duration::from_secs(5)).unwrap();
        process.stop().unwrap();
        let logout = observed.recv_timeout(Duration::from_secs(5)).unwrap();
        directory.join().unwrap();
        std::fs::remove_file(secrets_path).unwrap();
        std::fs::remove_dir(root).unwrap();
        assert!(login.starts_with(b"lK1ABC-R\xac\xacfixture-value\rONLINE"));
        assert!(login.windows(8).any(|part| part == b"\rBefore\r"));
        assert_eq!(snapshot, b"F0\r");
        assert!(renewed.windows(7).any(|part| part == b"\rAfter\r"));
        assert!(logout.windows(9).any(|part| part == b"OFF-V3.40"));
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
            (
                RuntimeError::Echo(rptadv_peer_support::echolink::host::Error::Control),
                "EchoLink account operation failed: Control",
            ),
            (
                RuntimeError::EchoUpdate(rptadv_peer_support::echolink::host::UpdateError {
                    cause: rptadv_peer_support::echolink::host::Error::Product,
                }),
                "EchoLink reload failed: UpdateError { cause: Product }",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn fixture_echo_retirement_retains_failed_owner_until_a_successful_stop() {
        use rptadv_peer_support::reload::ReloadOutcome;
        use std::os::unix::fs::{PermissionsExt, symlink};
        const CHILD: &str = "RPTADV_TEST_ECHO_RUNTIME_FIXTURE";
        if let Some(directory) = std::env::var_os(CHILD) {
            let directory = std::path::PathBuf::from(directory);
            let secrets = directory.join("echolink-secrets.conf");
            std::fs::write(&secrets, "[524950]\ndirectory_password=fixture-value\n").unwrap();
            std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
            let providers = crate::providers::ProviderSet::load().unwrap();
            let active = "[524950]\nnode_enabled=no\n[echolink 524950]\nenabled=yes\ncallsign=K1ABC-R\nnode_number=7\n";
            let mut process = ProductRuntime::start_with_providers(
                &directory.join("rpt_advanced.conf"),
                active,
                Vec::new(),
                SecretsFile::parse("").unwrap(),
                providers,
            )
            .unwrap();
            let library =
                unsafe { libloading::Library::new(directory.join("librptadv_echolink.so.1")) }
                    .unwrap();
            let validations = unsafe {
                library.get::<unsafe extern "C" fn() -> usize>(b"fixture_pending_validations\0")
            }
            .unwrap();
            let peers =
                unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_peers\0") }
                    .unwrap();
            // Activation queues a real incoming QSO. Let its control callback
            // claim and return the declined peer before disabling that account.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while unsafe { validations() == 0 || peers() != 0 } {
                assert!(
                    std::time::Instant::now() < deadline,
                    "incoming QSO did not drain: validations={}, peers={}",
                    unsafe { validations() },
                    unsafe { peers() },
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(unsafe { validations() }, 1);
            process
                .reload("[524950]\nnode_enabled=no\n[echolink 524950]\nenabled=no\n")
                .unwrap();
            let fail = unsafe {
                library.get::<unsafe extern "C" fn(u32)>(b"fixture_disabled_stop_failure\0")
            }
            .unwrap();
            unsafe { fail(1) };
            // A disabled node still owns its disabled Echo account. Remove the
            // node itself to exercise retirement, not just final shutdown.
            let candidate = "[general]\nenabled=no\n";
            let error = process.reload(candidate).unwrap_err();
            assert_eq!(
                error,
                RuntimeError::EchoCommitted(rptadv_peer_support::echolink::host::UpdateOutcome {
                    product: ReloadOutcome::Committed,
                    failure: None,
                    retirement: Some(rptadv_peer_support::echolink::host::Error::Status(
                        rptadv_peer_support::echolink::abi::Status::Failed,
                    )),
                })
            );
            assert_eq!(error.publication(), ReloadOutcome::Committed);
            assert_eq!(process.configuration_text(), candidate);
            assert!(matches!(process.stop(), Err(RuntimeError::Echo(_))));
            unsafe { fail(0) };
            process.stop().unwrap();
            return;
        }
        let fixture = crate::host_services::echo_tests::support::Fixture::new("network");
        let directory = fixture.0.parent().unwrap();
        symlink(&fixture.0, directory.join("librptadv_echolink.so.1")).unwrap();
        let mut search = vec![directory.to_owned()];
        search.extend(std::env::split_paths(
            &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
        ));
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::tests::fixture_echo_retirement_retains_failed_owner_until_a_successful_stop", "--nocapture", "--test-threads=1"])
            .env(CHILD, directory)
            .env("LD_LIBRARY_PATH", std::env::join_paths(search).unwrap())
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        // The child asserts the exact returned retirement diagnostic before
        // shutdown. The typed API reports errors to its caller instead of
        // requiring a duplicate internal stderr message.
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
    #[test]
    fn product_mixed_reload_cohorts_are_rejected_before_start() {
        for wrong_version in [true, false] {
            let mut api = *descriptor(start_without_counts, stop_without_counts, None);
            if wrong_version {
                api.abi_version = 7;
            } else {
                api.capability = *b"rptadv.prod7\0\0\0\0";
            }
            let result = unsafe {
                ProductRuntime::start(
                    Box::leak(Box::new(api)),
                    host(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    "[general]\n",
                )
            };
            assert!(matches!(result, Err(RuntimeError::InvalidDescriptor)));
        }
    }
}
