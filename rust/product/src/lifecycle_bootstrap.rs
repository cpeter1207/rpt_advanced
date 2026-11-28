//! Shared provisioned startup for native and Asterisk service owners.
use super::*;
use crate::management::{
    Coordinator, ManagedStore, ProfileInputs,
    native::NativeRuntime,
    webrtc_session::{
        Transport,
        transport::{Sessions, TurnAccess},
    },
};
use rpt_advanced_core::config::resources::candidate::ResourceId;
use rptadv_peer_support::{
    https::{
        CertificateDer, Listener, OriginPolicy, PrivateKeyDer, ServerIdentity, ServiceId,
        ServiceTrust,
    },
    sealed_records::{KeyId, load_systemd_tls},
    working_files::{CaptureLimits, WorkingFiles},
};
use serde::Deserialize;
use std::{
    net::{IpAddr, SocketAddr},
    path::{Component, PathBuf},
};

const LIMIT: usize = 64 * 1024;
const STORE_LIMITS: CaptureLimits = CaptureLimits {
    max_paths: 32_768,
    max_path_bytes: 1024,
    max_file_bytes: 4 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provisioning {
    store_path: PathBuf,
    store_uuid: String,
    write_key_id: String,
    listen: SocketAddr,
    #[serde(default)]
    webrtc_bind_address: Option<IpAddr>,
    #[serde(default)]
    webrtc_turn_credential: Option<String>,
    authorities: Vec<String>,
    #[serde(default)]
    origins: Vec<String>,
    server_chain: Vec<String>,
    server_key: String,
    client_issuers: Vec<String>,
    clients: Vec<Client>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Client {
    identity: String,
    leaf_certificate: String,
    /// This exact provisioned certificate belongs to a trusted browser proxy.
    #[serde(default)]
    browser_principal_proxy: bool,
}

impl Provisioning {
    fn decode(bytes: &[u8]) -> Result<Self, RuntimeError> {
        if bytes.len() > LIMIT {
            return Err(RuntimeError::Preparation);
        }
        let value = rptadv_json::decode_value(bytes).map_err(|_| RuntimeError::Preparation)?;
        let input: Self = serde_json::from_value(value).map_err(|_| RuntimeError::Preparation)?;
        if !input.store_path.is_absolute()
            || input
                .store_path
                .components()
                .any(|p| p == Component::ParentDir)
            || !input.listen.ip().is_loopback()
            || input
                .webrtc_bind_address
                .is_some_and(|ip| ip.is_unspecified() || ip.is_multicast())
            || (input.webrtc_turn_credential.is_some() && input.webrtc_bind_address.is_none())
            || input.authorities.is_empty()
            || input.server_chain.is_empty()
            || input.client_issuers.is_empty()
            || input.clients.is_empty()
            || [
                input.server_chain.len(),
                input.client_issuers.len(),
                input.clients.len(),
                input.authorities.len(),
                input.origins.len(),
            ]
            .into_iter()
            .any(|n| n > 32)
        {
            return Err(RuntimeError::Preparation);
        }
        Ok(input)
    }

    fn cert(reference: &str) -> Result<CertificateDer<'static>, RuntimeError> {
        let reference = KeyId::new(reference).map_err(|_| RuntimeError::Preparation)?;
        let contents = load_systemd_tls(&reference).map_err(|_| RuntimeError::Preparation)?;
        Ok(CertificateDer::from(contents.to_vec()))
    }

    fn transport(&self) -> Result<(ServerIdentity, ServiceTrust, OriginPolicy), RuntimeError> {
        let certs = |files: &[String]| {
            files
                .iter()
                .map(|f| Self::cert(f))
                .collect::<Result<Vec<_>, _>>()
        };
        let key_name = KeyId::new(&self.server_key).map_err(|_| RuntimeError::Preparation)?;
        let bytes = load_systemd_tls(&key_name).map_err(|_| RuntimeError::Preparation)?;
        let key = PrivateKeyDer::try_from(&bytes[..])
            .map_err(|_| RuntimeError::Preparation)?
            .clone_key();
        let identity = ServerIdentity::new(certs(&self.server_chain)?, key);
        let mut clients = Vec::with_capacity(self.clients.len());
        let mut browser_proxies = Vec::new();
        for client in &self.clients {
            let certificate = Self::cert(&client.leaf_certificate)?;
            let pin = ServiceTrust::leaf_pin(&certificate);
            clients.push((
                pin,
                ServiceId::new(&client.identity).map_err(|_| RuntimeError::Preparation)?,
            ));
            if client.browser_principal_proxy {
                browser_proxies.push(pin);
            }
        }
        let trust = ServiceTrust::new(certs(&self.client_issuers)?, clients)
            .and_then(|trust| trust.with_browser_principal_proxies(browser_proxies))
            .map_err(|_| RuntimeError::Preparation)?;
        let policy = OriginPolicy::new(self.authorities.clone(), self.origins.clone())
            .map_err(|_| RuntimeError::Preparation)?;
        Ok((identity, trust, policy))
    }

    fn webrtc_relay(&self) -> Result<Option<TurnAccess>, RuntimeError> {
        self.webrtc_turn_credential
            .as_deref()
            .map(TurnAccess::load)
            .transpose()
            .map_err(|_| RuntimeError::Preparation)
    }

    fn listener(
        &self,
    ) -> Result<(Listener, rptadv_peer_support::https::CoordinatorInbox), RuntimeError> {
        let (identity, trust, policy) = self.transport()?;
        Listener::start(self.listen, identity, trust, policy).map_err(|_| RuntimeError::Preparation)
    }
}

/// Validate the provisioned service and its verified selected generation without binding
/// the listener, taking a writer lease, editing the store, or opening a physical device.
fn check(bytes: &[u8]) -> Result<crate::management::PreparedConfiguration, RuntimeError> {
    use crate::configuration_transaction::{StartupStatus, Transaction};
    let config = Provisioning::decode(bytes)?;
    let (identity, _, _) = config.transport()?;
    identity.validate().map_err(|_| RuntimeError::Preparation)?;
    config.webrtc_relay()?;
    let expected: ResourceId = config
        .store_uuid
        .parse()
        .map_err(|_| RuntimeError::Preparation)?;
    let key = KeyId::new(&config.write_key_id).map_err(|_| RuntimeError::Preparation)?;
    let store = WorkingFiles::open(&config.store_path)
        .map_err(|_| RuntimeError::Preparation)?
        .with_identity(expected.uuid());
    let guard = store.try_edit().map_err(|_| RuntimeError::Busy)?;
    let selected = Transaction::new(&guard, STORE_LIMITS)
        .with_write_key(&key)
        .inspect()
        .map_err(|_| RuntimeError::Preparation)?;
    if selected.bookkeeping_fault
        || selected.state.journal.is_some()
        || selected.status != StartupStatus::Ready
    {
        return Err(RuntimeError::Preparation);
    }
    let candidate = selected
        .desired
        .assemble_candidate()
        .map_err(|_| RuntimeError::Preparation)?;
    let prepared = crate::management::PreparedConfiguration::prepare(
        &candidate,
        crate::schedule_zone::capture().as_deref(),
        &ProfileInputs::installed(),
    )
    .map_err(|_| RuntimeError::Preparation)?;
    Ok(prepared)
}

/// Start the checked JSON management service with host-owned immutable provider descriptors.
/// An admitted service retains its writer lease through successful stop and drain.
///
/// # Safety
/// The caller owns every descriptor and all code and contexts reachable from them through
/// successful stop. The input is a readable JSON span for this synchronous call.
unsafe extern "C" fn start_management(
    host: *const abi::rptadv_host_services_v11,
    control: *const ControlDescriptor,
    file: *const FileDescriptor,
    speech: *const SpeechDescriptor,
    provisioning: *const std::ffi::c_char,
    length: usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<i32, RuntimeError> {
        if length > LIMIT {
            return Err(RuntimeError::Preparation);
        }
        // SAFETY: the caller retains the complete input span for this call.
        let config = Provisioning::decode(unsafe { input(provisioning, length)? }.as_bytes())?;
        let identity: ResourceId = config
            .store_uuid
            .parse()
            .map_err(|_| RuntimeError::Preparation)?;
        let key = KeyId::new(&config.write_key_id).map_err(|_| RuntimeError::Preparation)?;
        let mut selected = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        if selected.is_some() {
            return Err(RuntimeError::Busy);
        }
        let store = Arc::new(ManagedStore::open(
            &config.store_path,
            identity,
            STORE_LIMITS,
        )?);
        let (listener, mut inbox) = config.listener()?;
        // SAFETY: the frontend retains these matched descriptors until confirmed shutdown.
        let engine =
            unsafe { Engine::start_management(Arc::clone(&store), host, control, file, speech)? };
        let service: Result<(), RuntimeError> = (|| {
            engine.start_ticker()?;
            engine.start_domain_observer();
            let turn = config.webrtc_relay()?;
            let webrtc = config
                .webrtc_bind_address
                .map(|address| {
                    Sessions::new(Arc::downgrade(&engine), address, turn)
                        .map(|sessions| Box::new(sessions) as Box<dyn Transport>)
                })
                .transpose()
                .map_err(|_| RuntimeError::Preparation)?;
            let runtime = Arc::new(NativeRuntime::new(Arc::clone(&engine)));
            let coordinator = if let Some(webrtc) = webrtc {
                Coordinator::for_service_with_webrtc(
                    store,
                    key,
                    runtime,
                    ProfileInputs::installed(),
                    Some(webrtc),
                )
            } else {
                Coordinator::for_service(store, key, runtime, ProfileInputs::installed())
            }
            .map_err(|_| RuntimeError::Preparation)?;
            *engine
                .management_state
                .reload
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(coordinator.host_reload());
            let station = Arc::clone(&engine);
            let worker = std::thread::Builder::new()
                .name("rpt-management-http".into())
                .spawn(move || {
                    while station.accepting.load(Ordering::Acquire) {
                        if !coordinator.poll_listener(&listener, &mut inbox) {
                            std::thread::park_timeout(std::time::Duration::from_millis(2));
                        }
                    }
                    drop(listener);
                    drop(coordinator);
                })
                .map_err(|_| RuntimeError::Preparation)?;
            engine
                .jobs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(worker);
            Ok(())
        })();
        if service.is_ok() {
            *selected = Some(engine);
            Ok(0)
        } else if engine.stop() {
            Ok(-1)
        } else {
            *selected = Some(engine);
            Ok(-2)
        }
    }))
    .ok()
    .and_then(Result::ok)
    .unwrap_or(-1)
}

static MANAGEMENT_DESCRIPTOR: abi::rptadv_product_management_v1 =
    abi::rptadv_product_management_v1 {
        struct_size: size_of::<abi::rptadv_product_management_v1>() as u32,
        abi_version: 1,
        capability: *b"rptadv.mgmt1\0\0\0\0",
        start: Some(start_management),
    };

/// Return immutable management startup metadata. The managed owner uses the matched product lifecycle table.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_product_management_descriptor_v1()
-> *const abi::rptadv_product_management_v1 {
    &MANAGEMENT_DESCRIPTOR
}

/// Validate one managed selection before synchronously visiting its public registered ports.
unsafe extern "C" fn inspect_managed(
    document: *const std::ffi::c_char,
    length: usize,
    port: abi::rptadv_managed_port_sink_v1,
    context: *mut std::ffi::c_void,
    diagnostic: abi::rptadv_text_sink_v1,
    diagnostic_context: *mut std::ffi::c_void,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        if length > LIMIT {
            return Err(RuntimeError::Preparation);
        }
        // SAFETY: the caller retains the complete synchronous document span.
        let prepared = check(unsafe { input(document, length)? }.as_bytes())?;
        for warning in prepared.diagnostics() {
            if let Some(sink) = diagnostic {
                let message = format!("{warning:?}");
                // SAFETY: the diagnostic and its context are borrowed synchronously.
                unsafe {
                    sink(diagnostic_context, message.as_ptr().cast(), message.len());
                }
            }
        }
        if let Some(sink) = port {
            let domain = prepared.runtime();
            for (key, id) in domain.registry.port_keys() {
                let scope =
                    rpt_advanced_core::config::resources::candidate::ResourceScope::Port(id);
                let enabled = domain
                    .scopes
                    .get(&scope)
                    .ok_or(RuntimeError::Preparation)?
                    .metadata
                    .enabled;
                let key = key.as_str();
                // SAFETY: both spans and the supplied callback context remain live for this call.
                if unsafe {
                    sink(
                        context,
                        id.uuid().as_bytes().as_ptr(),
                        key.as_ptr().cast(),
                        key.len(),
                        u32::from(enabled),
                    )
                } != 0
                {
                    return Err(RuntimeError::Rejected);
                }
            }
        }
        Ok(())
    }));
    if matches!(result, Ok(Ok(()))) {
        0
    } else {
        if let Some(sink) = diagnostic {
            let message = if matches!(result, Ok(Err(RuntimeError::Rejected))) {
                "configuration visitor rejected a registered port"
            } else {
                "service provisioning or selected configuration is invalid"
            };
            // SAFETY: this static message and the borrowed diagnostic context are valid through the call.
            unsafe {
                sink(diagnostic_context, message.as_ptr().cast(), message.len());
            }
        }
        -1
    }
}
static INSPECTION: abi::rptadv_product_inspection_v1 = abi::rptadv_product_inspection_v1 {
    struct_size: size_of::<abi::rptadv_product_inspection_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.inspect1\0",
    inspect: Some(inspect_managed),
};

/// Return the read-only managed inspection API independently of the two frozen product tables.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_product_inspection_descriptor_v1()
-> *const abi::rptadv_product_inspection_v1 {
    &INSPECTION
}
