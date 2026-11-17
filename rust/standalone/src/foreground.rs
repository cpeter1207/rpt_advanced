//! Standalone process startup, SIGHUP reload, and graceful signal shutdown.

use std::{
    collections::BTreeMap,
    ffi::c_void,
    net::SocketAddr,
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

static STOP: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);
const MAX_IAX_DATAGRAMS_PER_POLL: usize = 256;

/// A foreground process failed startup, reload, or shutdown.
#[derive(Debug)]
pub enum ForegroundError {
    /// The daemon must run under its dedicated unprivileged service account.
    RootUser,
    /// Configuration or secure IAX credential loading failed.
    Configuration(String),
    /// Required standalone shared libraries are unavailable or incompatible.
    Provider(crate::providers::ProviderError),
    /// The portable product could not start or stop cleanly.
    Runtime(crate::runtime::RuntimeError),
    /// The standalone HTTPS registration worker could not start.
    Registration(std::io::Error),
    /// The standalone IAX2 listener could not bind or operate.
    Iax(crate::iax::IaxError),
    /// The service-only local maintenance socket could not bind.
    LocalControl(std::io::Error),
    /// The host cannot provide Unix signal and privilege semantics.
    UnsupportedPlatform,
}

impl std::fmt::Display for ForegroundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootUser => formatter.write_str("rpt-advanced must run as an unprivileged user"),
            Self::Configuration(error) => formatter.write_str(error),
            Self::Provider(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::Registration(error) => {
                write!(formatter, "cannot start registration worker: {error}")
            }
            Self::Iax(error) => error.fmt(formatter),
            Self::LocalControl(error) => {
                write!(formatter, "cannot start local radio control: {error}")
            }
            Self::UnsupportedPlatform => {
                formatter.write_str("foreground mode requires a Unix system")
            }
        }
    }
}

impl std::error::Error for ForegroundError {}

/// Run the product until SIGINT/SIGTERM; SIGHUP validates and replaces its configuration.
pub fn run(configuration: &Path, secrets_path: Option<&Path>) -> Result<(), ForegroundError> {
    #[cfg(not(unix))]
    {
        let _ = (configuration, secrets_path);
        return Err(ForegroundError::UnsupportedPlatform);
    }
    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } == 0 {
            return Err(ForegroundError::RootUser);
        }
        let text = read_configuration(configuration)?;
        validate_configuration(&text)?;
        let radios = resolve_radios(&text)?;
        let secrets = read_secrets(secrets_path)?;
        let registrations = crate::registration::targets(&text, &secrets)
            .map_err(|error| ForegroundError::Configuration(error.to_string()))?;
        let mut accounts = ForegroundAccounts::default();
        accounts.active = accounts.prepare(configuration, &text, &radios)?;
        let providers = crate::providers::ProviderSet::load().map_err(ForegroundError::Provider)?;
        let client = providers.iax_client().map_err(ForegroundError::Provider)?;
        let mut iax_listeners = bind_iax_listeners(&client, &radios, &accounts.active)?;
        let runtime = crate::runtime::ProductRuntime::start_with_providers(
            configuration,
            &text,
            radios,
            secrets,
            providers,
        )
        .map_err(ForegroundError::Runtime)?;
        enable_iax_listeners(&mut iax_listeners, &runtime)?;
        let registration = crate::registration::RegistrationWorker::start(registrations)
            .map_err(ForegroundError::Registration)?;
        run_runtime(
            configuration,
            secrets_path,
            client,
            iax_listeners,
            runtime,
            registration,
            accounts,
        )
    }
}

/// Poll prepared process owners, then quiesce them before releasing their callbacks.
/// Parameters retain startup's reverse drop order if signal/socket setup fails.
#[cfg(unix)]
fn run_runtime(
    configuration: &Path,
    secrets_path: Option<&Path>,
    client: crate::iax::IaxClient,
    mut iax_listeners: BTreeMap<u16, crate::iax::IaxServer>,
    mut runtime: crate::runtime::ProductRuntime,
    mut registration: crate::registration::RegistrationWorker,
    mut accounts: ForegroundAccounts,
) -> Result<(), ForegroundError> {
    install_signal_handlers()?;
    let mut local_control =
        crate::local_control::Server::bind(Path::new(crate::local_control::SOCKET_PATH))
            .map_err(ForegroundError::LocalControl)?;
    eprintln!("rpt-advanced standalone runtime started");
    while !STOP.load(Ordering::Acquire) {
        poll_iax_listeners(&mut iax_listeners, &runtime, &client, &accounts.active);
        let mut control = ForegroundControl {
            runtime: &mut runtime,
            configuration,
            secrets_path,
            listeners: &mut iax_listeners,
            client: &client,
            registration: &mut registration,
            accounts: &mut accounts,
        };
        local_control.poll(&mut control);
        if RELOAD.swap(false, Ordering::AcqRel) {
            local_control.close(&mut control);
            if let Err(error) = control.reload() {
                eprintln!("configuration reload failed: {error}");
            }
        }
        if control.runtime.reload_fatal() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    local_control.close(&mut ForegroundControl {
        runtime: &mut runtime,
        configuration,
        secrets_path,
        listeners: &mut iax_listeners,
        client: &client,
        registration: &mut registration,
        accounts: &mut accounts,
    });
    let fatal = runtime.reload_failure();
    if let Err(error) = runtime.stop() {
        // A failed quiescence cannot release listener callbacks underneath the
        // retained product/provider owners. Process shutdown remains fatal.
        std::mem::forget(iax_listeners);
        registration.stop();
        return Err(ForegroundError::Runtime(error));
    }
    iax_listeners.clear();
    registration.stop();
    eprintln!("rpt-advanced standalone runtime stopped");
    shutdown_result(fatal, Ok(()))
}

#[cfg(unix)]
fn shutdown_result(
    fatal: Option<crate::runtime::RuntimeError>,
    stopped: Result<(), crate::runtime::RuntimeError>,
) -> Result<(), ForegroundError> {
    stopped
        .and_then(|()| fatal.map_or(Ok(()), Err))
        .map_err(ForegroundError::Runtime)
}

/// One generation allocator and its published configuration snapshot.
/// Preparation is synchronous and numeric-only: it performs no name service,
/// registration or admission while the foreground owns the listener cohort.
#[derive(Default)]
struct ForegroundAccounts {
    preparer: crate::phone_accounts::AccountPreparer,
    active: crate::phone_accounts::PreparedAccounts,
}

impl ForegroundAccounts {
    fn prepare(
        &mut self,
        configuration: &Path,
        text: &str,
        radios: &[crate::ResolvedRadioNode],
    ) -> Result<crate::phone_accounts::PreparedAccounts, ForegroundError> {
        use crate::configuration::PhoneAccountProtocol;
        let mut records = crate::configuration::resolve_phone_accounts(text)
            .map_err(|error| ForegroundError::Configuration(error.to_string()))?;
        records.retain(|record| {
            record.enabled
                && radios
                    .iter()
                    .any(|node| node.enabled && node.node == record.local_node)
        });
        for record in &records {
            let unsupported = if record.protocol != PhoneAccountProtocol::Iax {
                Some("standalone phone account activation currently requires IAX")
            } else if record.register {
                Some(
                    "standalone phone account registration is not yet available; configure register=no",
                )
            } else if record.server.parse::<SocketAddr>().is_err() {
                Some("standalone phone account activation currently requires a numeric endpoint")
            } else {
                None
            };
            if let Some(reason) = unsupported {
                return Err(ForegroundError::Configuration(reason.into()));
            }
        }
        self.preparer
            .prepare(configuration, records, &self.active, |_| {
                Err(std::io::Error::other("numeric phone endpoint required"))
            })
            .map_err(|error| ForegroundError::Configuration(error.to_string()))
    }
}

#[cfg(unix)]
struct ForegroundControl<'a> {
    runtime: &'a mut crate::runtime::ProductRuntime,
    configuration: &'a Path,
    secrets_path: Option<&'a Path>,
    listeners: &'a mut BTreeMap<u16, crate::iax::IaxServer>,
    client: &'a crate::iax::IaxClient,
    registration: &'a mut crate::registration::RegistrationWorker,
    accounts: &'a mut ForegroundAccounts,
}

#[cfg(unix)]
impl ForegroundControl<'_> {
    /// Signal reload and tuner reload use exactly the same foreground transaction.
    fn reload(&mut self) -> Result<(), ForegroundError> {
        let updated = read_configuration(self.configuration)?;
        validate_configuration(&updated)?;
        let radios = resolve_radios(&updated)?;
        let candidate = self
            .accounts
            .prepare(self.configuration, &updated, &radios)?;
        let profiles = crate::iax::listeners::resolve_profiles(&radios, &candidate)
            .map_err(ForegroundError::Iax)?;
        let result = update_iax_listeners(
            self.listeners,
            profiles,
            self.client,
            self.runtime,
            |runtime, listener| runtime.enable_listener(listener),
            |runtime| runtime.reload_with_radios(&updated, radios),
        );
        if let Err(error) = &result {
            if !matches!(error, ForegroundError::Runtime(error) if error.publication().published())
            {
                return result;
            }
        }
        // The provider batch and product now expose this exact generation,
        // including a published product outcome that reports a later error.
        self.accounts.active = candidate;
        match read_secrets(self.secrets_path)
            .and_then(|secrets| registration_targets(&updated, &secrets))
        {
            Ok(targets) => self.registration.replace(targets),
            Err(error) => eprintln!("registration configuration reload rejected: {error}"),
        }
        if result.is_ok() {
            eprintln!("rpt-advanced configuration reloaded");
        }
        result
    }
}

#[cfg(unix)]
impl crate::local_control::Backend for ForegroundControl<'_> {
    fn execute(&mut self, request: &serde_json::Value) -> Result<serde_json::Value, String> {
        if request["operation"] == "reload" {
            self.reload().map_err(|error| error.to_string())?;
            Ok(serde_json::Value::Null)
        } else {
            self.runtime.local_radio_request(request)
        }
    }
    fn clear_manual(&mut self, node: &str) {
        self.runtime.clear_local_manual(node);
    }
}

/// Bind complete account profiles before any product or listener is activated.
fn bind_iax_listeners(
    client: &crate::iax::IaxClient,
    radios: &[crate::ResolvedRadioNode],
    accounts: &crate::phone_accounts::PreparedAccounts,
) -> Result<BTreeMap<u16, crate::iax::IaxServer>, ForegroundError> {
    crate::iax::listeners::resolve_profiles(radios, accounts)
        .map_err(ForegroundError::Iax)?
        .into_iter()
        .map(|(port, profile)| {
            let address = SocketAddr::from(([0, 0, 0, 0], port));
            crate::iax::IaxServer::bind_with_accounts(
                client,
                address,
                &profile.local_nodes,
                &profile.accounts,
            )
            .map(|listener| (port, listener))
            .map_err(ForegroundError::Iax)
        })
        .collect()
}

fn update_iax_listeners<C>(
    listeners: &mut BTreeMap<u16, crate::iax::IaxServer>,
    profiles: BTreeMap<u16, crate::iax::listeners::ListenerProfile<'_>>,
    client: &crate::iax::IaxClient,
    context: &mut C,
    enable_added: impl FnMut(&mut C, &mut crate::iax::IaxServer) -> Result<(), crate::iax::IaxError>,
    reload_product: impl FnOnce(&mut C) -> Result<(), crate::runtime::RuntimeError>,
) -> Result<(), ForegroundError> {
    use rptadv_peer_support::reload::ReloadOutcome;
    let mut result = Ok(());
    crate::iax::listeners::update(
        listeners,
        &profiles,
        client,
        context,
        enable_added,
        |context| {
            result = reload_product(context);
            match &result {
                Ok(()) => ReloadOutcome::Committed,
                Err(error) => error.publication(),
            }
        },
    )
    .map_err(ForegroundError::Iax)?;
    // Keep the exact original product/host error, including its raw fatal status.
    result.map_err(ForegroundError::Runtime)
}

/// Start prepared listeners before exposing their generation to control polling.
fn enable_iax_listeners(
    listeners: &mut BTreeMap<u16, crate::iax::IaxServer>,
    runtime: &crate::runtime::ProductRuntime,
) -> Result<(), ForegroundError> {
    listeners.values_mut().try_for_each(|listener| {
        runtime
            .enable_listener(listener)
            .map_err(ForegroundError::Iax)
    })
}

fn poll_iax_listeners(
    listeners: &mut BTreeMap<u16, crate::iax::IaxServer>,
    runtime: &crate::runtime::ProductRuntime,
    client: &crate::iax::IaxClient,
    accounts: &crate::phone_accounts::PreparedAccounts,
) {
    let mut context = InboundContext {
        runtime,
        client: client.clone(),
        accounts,
        listener: SocketAddr::from(([0, 0, 0, 0], 0)),
    };
    let pointer = ptr::from_mut(&mut context).cast::<c_void>();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as u32);
    for (port, listener) in listeners {
        context.listener = SocketAddr::from(([0, 0, 0, 0], *port));
        report_iax_poll(drain_iax_datagrams(|| unsafe {
            listener.poll_accounts(
                now,
                Some(authorize_inbound),
                Some(accept_inbound),
                Some(authorize_provider),
                Some(accept_provider),
                pointer,
            )
        }));
    }
}

fn report_iax_poll(result: Result<(), crate::iax::IaxError>) {
    if let Err(error) = result {
        eprintln!("IAX2 listener poll failed: {error}");
    }
}

fn drain_iax_datagrams(
    mut poll: impl FnMut() -> Result<bool, crate::iax::IaxError>,
) -> Result<(), crate::iax::IaxError> {
    for _ in 0..MAX_IAX_DATAGRAMS_PER_POLL {
        if !poll()? {
            break;
        }
    }
    Ok(())
}

struct InboundContext<'a> {
    runtime: &'a crate::runtime::ProductRuntime,
    client: crate::iax::IaxClient,
    accounts: &'a crate::phone_accounts::PreparedAccounts,
    listener: SocketAddr,
}

unsafe fn callback_text<'a>(pointer: *const u8, length: usize) -> Option<&'a str> {
    if pointer.is_null() && length != 0 {
        return None;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer, length) }
    };
    std::str::from_utf8(bytes).ok()
}

unsafe extern "C" fn authorize_inbound(
    context: *mut c_void,
    local: *const u8,
    local_length: usize,
    remote: *const u8,
    remote_length: usize,
    source: *const u8,
    source_length: usize,
) -> i32 {
    let Some(context) = (unsafe { context.cast::<InboundContext<'_>>().as_ref() }) else {
        return -1;
    };
    let (Some(local), Some(remote), Some(source)) = (
        unsafe { callback_text(local, local_length) },
        unsafe { callback_text(remote, remote_length) },
        unsafe { callback_text(source, source_length) },
    ) else {
        return -1;
    };
    i32::from(!context.runtime.authorize_incoming(local, remote, source))
}

unsafe extern "C" fn accept_inbound(
    context: *mut c_void,
    local: *const u8,
    local_length: usize,
    remote: *const u8,
    remote_length: usize,
    source: *const u8,
    source_length: usize,
    peer: *mut c_void,
) -> i32 {
    let Some(context) = (unsafe { context.cast::<InboundContext<'_>>().as_ref() }) else {
        return -1;
    };
    let (Some(local), Some(remote), Some(source)) = (
        unsafe { callback_text(local, local_length) },
        unsafe { callback_text(remote, remote_length) },
        unsafe { callback_text(source, source_length) },
    ) else {
        return -1;
    };
    // SAFETY: the matching library server transfers this unique inbound IaxPeer handle here.
    let Ok(peer) = (unsafe { context.client.clone().adopt_inbound(peer) }) else {
        return -1;
    };
    let _ = context.runtime.incoming(local, remote, source, peer);
    0
}

/// Validate only provider-authenticated transport provenance here. The real
/// product authorization runs with the actual owned host handle at acceptance;
/// this earlier provider callback exposes metadata only, never a peer handle.
unsafe fn provider_admission<'a>(
    context: &'a InboundContext<'_>,
    metadata: *const crate::abi::rptadv_iax2_provider_incoming_v1,
) -> Option<(
    &'a crate::phone_accounts::PreparedAccount,
    crate::iax::accounts::CallReceipt,
)> {
    use crate::{abi, configuration::PhoneAccountProtocol, iax::accounts::call_receipt};
    if context.runtime.reload_fatal()
        || metadata.is_null()
        || unsafe { ptr::addr_of!((*metadata).struct_size).read() }
            < std::mem::size_of::<abi::rptadv_iax2_provider_incoming_v1>() as u32
        || unsafe { ptr::addr_of!((*metadata).abi_version).read() } != 1
    {
        return None;
    }
    let metadata = unsafe { &*metadata };
    let label = unsafe { callback_text(metadata.account.data, metadata.account.length) }?;
    let accounts = context.accounts.as_slice();
    let index = accounts
        .binary_search_by(|account| account.settings().id.as_str().cmp(label))
        .ok()?;
    let account = accounts[index].as_ref();
    let settings = account.settings();
    let receipt = call_receipt(metadata.receipt, account.generation()).ok()?;
    (settings.enabled
        && settings.protocol == PhoneAccountProtocol::Iax
        && receipt.local == context.listener
        && receipt.remote == account.remote()
        && matches!(receipt.format, 4 | 8)
        && unsafe { callback_text(metadata.local_binding.data, metadata.local_binding.length) }?
            == settings.local_node
        && unsafe { callback_text(metadata.username.data, metadata.username.length) }?
            == settings.username)
        .then_some((account, receipt))
}

unsafe extern "C" fn authorize_provider(
    context: *mut c_void,
    metadata: *const crate::abi::rptadv_iax2_provider_incoming_v1,
) -> i32 {
    let Some(context) = (unsafe { context.cast::<InboundContext<'_>>().as_ref() }) else {
        return -1;
    };
    if unsafe { provider_admission(context, metadata) }.is_some() {
        0
    } else {
        -1
    }
}

unsafe extern "C" fn accept_provider(
    context: *mut c_void,
    metadata: *const crate::abi::rptadv_iax2_provider_incoming_v1,
    raw: *mut c_void,
) -> i32 {
    let Some(context) = (unsafe { context.cast::<InboundContext<'_>>().as_ref() }) else {
        return -1;
    };
    let Some((account, receipt)) = (unsafe { provider_admission(context, metadata) }) else {
        return -1;
    };
    // Returning nonzero before adoption leaves destruction with the provider.
    let Ok(peer) = (unsafe { context.client.clone().adopt_inbound(raw) }) else {
        return -1;
    };
    if let Err(error) = context.runtime.incoming_provider(
        crate::iax::accounts::ProviderCall { peer, receipt },
        account,
        context.listener,
    ) {
        eprintln!("IAX2 phone admission rejected: {error}");
    }
    // The adopted peer was consumed or destroyed locally, even on rejection.
    0
}

fn read_configuration(path: &Path) -> Result<String, ForegroundError> {
    std::fs::read_to_string(path)
        .map_err(|error| ForegroundError::Configuration(format!("{}: {error}", path.display())))
}

fn validate_configuration(text: &str) -> Result<(), ForegroundError> {
    crate::check_config(text)
        .map(|warnings| {
            for warning in warnings {
                eprintln!("warning: {warning}");
            }
        })
        .map_err(|error| ForegroundError::Configuration(error.to_string()))
}

fn registration_targets(
    configuration: &str,
    secrets: &crate::secrets::SecretsFile,
) -> Result<Vec<crate::registration::RegistrationTarget>, ForegroundError> {
    crate::registration::targets(configuration, secrets)
        .map_err(|error| ForegroundError::Configuration(error.to_string()))
}

fn resolve_radios(text: &str) -> Result<Vec<crate::ResolvedRadioNode>, ForegroundError> {
    crate::resolve_radio_nodes(text)
        .map_err(|error| ForegroundError::Configuration(error.to_string()))
}

#[cfg(unix)]
fn read_secrets(path: Option<&Path>) -> Result<crate::secrets::SecretsFile, ForegroundError> {
    let path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/etc/rpt_advanced/iax-secrets.conf"));
    if path.exists() {
        crate::secrets::SecretsFile::load(&path)
            .map_err(|error| ForegroundError::Configuration(error.to_string()))
    } else if path == Path::new("/etc/rpt_advanced/iax-secrets.conf") {
        crate::secrets::SecretsFile::parse("")
            .map_err(|error| ForegroundError::Configuration(error.to_string()))
    } else {
        Err(ForegroundError::Configuration(format!(
            "{}: secrets file does not exist",
            path.display()
        )))
    }
}

#[cfg(unix)]
extern "C" fn handle_signal(signal: libc::c_int) {
    if signal == libc::SIGHUP {
        RELOAD.store(true, Ordering::Release);
    } else {
        STOP.store(true, Ordering::Release);
    }
}

#[cfg(unix)]
fn install_signal_handlers() -> Result<(), ForegroundError> {
    // SAFETY: handler only stores lock-free atomics; all file/product work runs on main thread.
    install_signal_handlers_with(|signal| unsafe {
        libc::signal(signal, handle_signal as *const () as libc::sighandler_t) != libc::SIG_ERR
    })
}

#[cfg(unix)]
fn install_signal_handlers_with(
    mut install: impl FnMut(libc::c_int) -> bool,
) -> Result<(), ForegroundError> {
    STOP.store(false, Ordering::Release);
    RELOAD.store(false, Ordering::Release);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        if !install(signal) {
            return Err(ForegroundError::Configuration(
                "cannot install process signal handler".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn test_client() -> crate::iax::IaxClient {
        crate::iax::IaxClient::load(std::path::Path::new("librptadviax2.so.2")).unwrap()
    }
    use super::ForegroundError;

    fn bind_iax_listener(
        client: &crate::iax::IaxClient,
        port: u16,
        nodes: &[String],
    ) -> Result<crate::iax::IaxServer, ForegroundError> {
        crate::iax::IaxServer::bind(client, ([0, 0, 0, 0], port).into(), nodes)
            .map_err(ForegroundError::Iax)
    }

    fn numeric_profiles(
        nodes: std::collections::BTreeMap<u16, Vec<String>>,
    ) -> std::collections::BTreeMap<u16, crate::iax::listeners::ListenerProfile<'static>> {
        nodes
            .into_iter()
            .map(|(port, local_nodes)| {
                (
                    port,
                    crate::iax::listeners::ListenerProfile {
                        local_nodes,
                        accounts: Vec::new(),
                    },
                )
            })
            .collect()
    }
    use std::{
        fs,
        net::UdpSocket,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn foreground_error_formats_each_public_failure() {
        let errors = [
            (ForegroundError::RootUser, "unprivileged user"),
            (
                ForegroundError::Configuration("bad config".into()),
                "bad config",
            ),
            (
                ForegroundError::Provider(crate::providers::ProviderError::Load("radio")),
                "radio",
            ),
            (
                ForegroundError::Runtime(crate::runtime::RuntimeError::Start),
                "product start",
            ),
            (
                ForegroundError::Registration(std::io::Error::other("offline")),
                "offline",
            ),
            (
                ForegroundError::Iax(crate::iax::IaxError::Dial),
                "call setup failed",
            ),
            (
                ForegroundError::LocalControl(std::io::Error::other("occupied socket")),
                "cannot start local radio control: occupied socket",
            ),
            (
                ForegroundError::UnsupportedPlatform,
                "requires a Unix system",
            ),
        ];
        for (error, expected) in errors {
            assert!(error.to_string().contains(expected));
        }
    }

    #[test]
    fn callback_text_accepts_empty_utf8_and_rejects_invalid_buffers() {
        let valid = b"506315";
        let invalid = [0xff];
        assert_eq!(
            unsafe { super::callback_text(valid.as_ptr(), valid.len()) },
            Some("506315")
        );
        assert_eq!(
            unsafe { super::callback_text(std::ptr::null(), 0) },
            Some("")
        );
        assert_eq!(unsafe { super::callback_text(std::ptr::null(), 1) }, None);
        assert_eq!(unsafe { super::callback_text(invalid.as_ptr(), 1) }, None);
    }

    #[test]
    fn configuration_helpers_report_valid_invalid_and_missing_inputs() {
        let text = "[general]\nnode_enabled=no\n";
        super::validate_configuration(text).unwrap();
        super::validate_configuration("[524950]\nunknown_setting=value\n").unwrap();
        assert!(super::validate_configuration("[general\n").is_err());
        assert!(super::resolve_radios("[general\n").is_err());

        let path =
            std::env::temp_dir().join(format!("rpt-advanced-config-{}.conf", std::process::id()));
        fs::write(&path, text).unwrap();
        assert_eq!(super::read_configuration(&path).unwrap(), text);
        fs::remove_file(&path).unwrap();
        assert!(super::read_configuration(&path).is_err());
    }

    #[test]
    fn registration_target_errors_are_reported_as_configuration_errors() {
        let secrets = crate::secrets::SecretsFile::parse("").unwrap();
        let error = match super::registration_targets("[general\n", &secrets) {
            Err(error) => error,
            Ok(_) => panic!("malformed registration configuration must be rejected"),
        };
        assert!(matches!(error, ForegroundError::Configuration(_)));
    }

    #[cfg(unix)]
    #[test]
    fn listener_poll_and_signal_install_errors_are_handled() {
        super::report_iax_poll(Ok(()));
        super::report_iax_poll(Err(crate::iax::IaxError::Operation));
        super::install_signal_handlers_with(|signal| signal != libc::SIGTERM).unwrap_err();
        super::install_signal_handlers_with(|_| true).unwrap();
    }

    #[test]
    fn standalone_listener_binds_and_reports_port_conflicts() {
        let occupied = UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], 0))).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let error = match bind_iax_listener(&test_client(), port, &["506315".to_owned()]) {
            Err(error) => error,
            Ok(_) => panic!("binding an occupied IAX port must fail"),
        };
        assert!(matches!(
            error,
            ForegroundError::Iax(crate::iax::IaxError::Bind)
        ));

        drop(occupied);
        let listener = bind_iax_listener(&test_client(), 0, &["506315".to_owned()]).unwrap();
        drop(listener);
    }

    #[test]
    fn listener_reload_commits_new_nodes_only_after_product_reload() {
        let client = test_client();
        let reservation = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        let current_port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let stale = bind_iax_listener(&client, 0, &["506310".to_owned()]).unwrap();
        let current = bind_iax_listener(&client, current_port, &["506315".to_owned()]).unwrap();
        let mut listeners =
            std::collections::BTreeMap::from([(current_port, current), (65000, stale)]);
        let nodes = std::collections::BTreeMap::from([
            (current_port, vec!["506316".to_owned()]),
            (0, vec!["55553".to_owned()]),
        ]);
        let mut reloaded = false;

        super::update_iax_listeners(
            &mut listeners,
            numeric_profiles(nodes),
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| {
                reloaded = true;
                Ok(())
            },
        )
        .unwrap();

        assert!(reloaded);
        assert_eq!(
            listeners.keys().copied().collect::<Vec<_>>(),
            [0, current_port]
        );
    }

    #[test]
    fn fatal_reload_shutdown_preserves_failure_after_successful_cleanup() {
        use crate::runtime::RuntimeError;
        use rptadv_peer_support::reload::ReloadOutcome;
        let fatal = RuntimeError::ReloadOutcome(ReloadOutcome::Fatal(73));
        assert!(matches!(
            super::shutdown_result(Some(fatal), Ok(())),
            Err(ForegroundError::Runtime(RuntimeError::ReloadOutcome(
                ReloadOutcome::Fatal(73)
            )))
        ));
        assert!(super::shutdown_result(None, Ok(())).is_ok());
        assert!(matches!(
            super::shutdown_result(Some(fatal), Err(RuntimeError::Stop)),
            Err(ForegroundError::Runtime(RuntimeError::Stop))
        ));
    }

    #[test]
    fn listener_reload_keeps_current_set_when_product_reload_fails() {
        let client = test_client();
        let current = bind_iax_listener(&client, 0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(4569, current)]);
        let nodes = std::collections::BTreeMap::from([(0, vec!["55553".to_owned()])]);

        let error = super::update_iax_listeners(
            &mut listeners,
            numeric_profiles(nodes),
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| Err(crate::runtime::RuntimeError::Reload),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ForegroundError::Runtime(crate::runtime::RuntimeError::Reload)
        ));
        assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569]);
    }

    #[test]
    fn listener_reload_retains_candidate_owners_for_committed_error_and_fatal_shutdown() {
        let client = test_client();
        use rptadv_peer_support::reload::ReloadOutcome;
        for outcome in [ReloadOutcome::CommittedWithError, ReloadOutcome::Fatal(73)] {
            let current = bind_iax_listener(&client, 0, &["506315".to_owned()]).unwrap();
            let mut listeners = std::collections::BTreeMap::from([(4569, current)]);
            let nodes = std::collections::BTreeMap::from([(0, vec!["55553".to_owned()])]);
            let result = super::update_iax_listeners(
                &mut listeners,
                numeric_profiles(nodes),
                &client,
                &mut (),
                |_, _| Ok(()),
                |_| Err(crate::runtime::RuntimeError::ReloadOutcome(outcome)),
            );
            assert!(result.is_err());
            assert!(
                listeners.contains_key(&0),
                "candidate callbacks must remain owned"
            );
            assert_eq!(listeners.contains_key(&4569), !outcome.published());
        }
    }

    #[test]
    fn listener_reload_prepares_async_candidates_before_committing() {
        let client = test_client();
        let current = bind_iax_listener(&client, 0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(4569, current)]);
        let nodes = std::collections::BTreeMap::from([(0, vec!["55553".to_owned()])]);
        let enabled = std::cell::Cell::new(0);
        let result = super::update_iax_listeners(
            &mut listeners,
            numeric_profiles(nodes),
            &client,
            &mut (),
            |_, _| {
                enabled.set(enabled.get() + 1);
                Err(crate::iax::IaxError::Operation)
            },
            |_| panic!("enable failure reached product publication"),
        );
        assert_eq!(enabled.get(), 1);
        assert!(matches!(result, Err(ForegroundError::Iax(_))));
        assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569]);
    }

    #[test]
    fn listener_reload_rejects_invalid_existing_node_assignment() {
        let client = test_client();
        let current = bind_iax_listener(&client, 0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(4569, current)]);
        let nodes = std::collections::BTreeMap::from([(4569, vec!["invalid".to_owned()])]);

        let error = super::update_iax_listeners(
            &mut listeners,
            numeric_profiles(nodes),
            &client,
            &mut (),
            |_, _| Ok(()),
            |_| Ok(()),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ForegroundError::Iax(crate::iax::IaxError::InvalidParameters)
        ));
        assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569]);
    }

    #[cfg(unix)]
    #[test]
    fn foreground_reload_adopts_only_published_configuration_and_listener_cohorts() {
        use crate::{abi, host_services::HostServicesOwner, runtime::ProductRuntime};
        use rptadv_peer_support::reload::ReloadOutcome;
        use std::{cell::Cell, ffi::c_char};
        thread_local! {
            static STATUS: Cell<i32> = const { Cell::new(0) };
            static CALLS: Cell<u32> = const { Cell::new(0) };
        }
        unsafe extern "C" fn start(
            _: *const abi::rptadv_host_services_v10,
            _: *const abi::rptadv_control_descriptor_v1,
            _: *const abi::rptadv_file_descriptor,
            _: *const abi::rptadv_speech_descriptor,
            _: *const c_char,
            _: usize,
        ) -> i32 {
            0
        }
        unsafe extern "C" fn reload(_: *const c_char, _: usize) -> i32 {
            CALLS.set(CALLS.get() + 1);
            STATUS.get()
        }
        extern "C" fn stop() -> i32 {
            0
        }
        let path =
            std::env::temp_dir().join(format!("rpt-foreground-cohort-{}.conf", std::process::id()));
        let secrets_path = path.with_extension("secrets");
        fs::write(&secrets_path, "").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, fs::Permissions::from_mode(0o600)).unwrap();
        let phone_path = path.with_extension("phone");
        fs::write(&phone_path, "fixture-secret").unwrap();
        fs::set_permissions(&phone_path, fs::Permissions::from_mode(0o600)).unwrap();
        let active = "[general]\nnode_enabled=no\n";
        let inspection = crate::providers::ProductInspection::open().unwrap();
        let mut product = *inspection.api();
        product.start = Some(start);
        product.reload = Some(reload);
        product.stop = Some(stop);
        let product = Box::leak(Box::new(product));
        let client = test_client();
        for (status, outcome, published, fatal, retain_listener) in [
            (0, ReloadOutcome::Committed, true, false, true),
            (-1, ReloadOutcome::Rejected, false, false, false),
            (-2, ReloadOutcome::CommittedWithError, true, false, true),
            (-3, ReloadOutcome::Fatal(-3), false, true, true),
            (73, ReloadOutcome::Fatal(73), false, true, true),
        ] {
            STATUS.set(status);
            CALLS.set(0);
            fs::write(&phone_path, "fixture-secret").unwrap();
            let reserved = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
            let port = reserved.local_addr().unwrap().port();
            let candidate = format!(
                "[general]\nnode_enabled=yes\niax_local_port={port}\n[524950]\nradio_channel=fixture\n[phone_account office]\nlocal_node=524950\nprotocol=iax\nenabled=yes\nserver=127.0.0.1:4569\nusername=fixture-user\nsecrets_file={}\nregister=no\n",
                phone_path.display()
            );
            fs::write(&path, &candidate).unwrap();
            let mut runtime = unsafe {
                ProductRuntime::start(
                    product,
                    HostServicesOwner::new(
                        Vec::new(),
                        None,
                        crate::secrets::SecretsFile::parse("").unwrap(),
                    )
                    .unwrap(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    active,
                )
            }
            .unwrap();
            let mut listeners = std::collections::BTreeMap::new();
            let mut registration =
                crate::registration::RegistrationWorker::start(Vec::new()).unwrap();
            drop(reserved);
            let mut accounts = super::ForegroundAccounts::default();
            let result = super::ForegroundControl {
                runtime: &mut runtime,
                configuration: &path,
                secrets_path: Some(&secrets_path),
                listeners: &mut listeners,
                client: &client,
                registration: &mut registration,
                accounts: &mut accounts,
            }
            .reload();
            assert_eq!(CALLS.get(), 1);
            assert_eq!(result.is_ok(), status == 0);
            if let Err(ForegroundError::Runtime(error)) = result {
                assert_eq!(
                    error,
                    if status == -1 {
                        crate::runtime::RuntimeError::Reload
                    } else {
                        crate::runtime::RuntimeError::ReloadOutcome(outcome)
                    }
                );
            } else {
                assert!(result.is_ok(), "unexpected foreground error: {result:?}");
            }
            assert_eq!(
                runtime.configuration_text(),
                if published { &candidate } else { active }
            );
            assert_eq!(listeners.contains_key(&port), retain_listener);
            assert_eq!(runtime.reload_fatal(), fatal);
            assert_eq!(accounts.active.as_slice().len(), usize::from(published));
            if published {
                let conflicting = crate::iax::accounts::ProviderAccount {
                    account: "office",
                    generation: 1,
                    local_binding: "524950",
                    remote: "127.0.0.1:4569".parse().unwrap(),
                    username: "fixture-user",
                    secret: "changed-secret",
                    authentication_timeout_ms: 1000,
                };
                assert_eq!(
                    listeners
                        .get_mut(&port)
                        .unwrap()
                        .configure_accounts(&[conflicting]),
                    Err(crate::iax::IaxError::InvalidParameters),
                    "published configuration must retain its provider account"
                );
            }
            if status == 0 {
                let original = std::sync::Arc::clone(&accounts.active.as_slice()[0]);
                let mut control = super::ForegroundControl {
                    runtime: &mut runtime,
                    configuration: &path,
                    secrets_path: Some(&secrets_path),
                    listeners: &mut listeners,
                    client: &client,
                    registration: &mut registration,
                    accounts: &mut accounts,
                };
                control.reload().unwrap();
                assert!(std::sync::Arc::ptr_eq(
                    &control.accounts.active.as_slice()[0],
                    &original
                ));
                fs::write(&phone_path, "replacement-secret").unwrap();
                STATUS.set(-1);
                assert!(control.reload().is_err());
                assert!(std::sync::Arc::ptr_eq(
                    &control.accounts.active.as_slice()[0],
                    &original
                ));
                STATUS.set(0);
                control.reload().unwrap();
                assert_eq!(control.accounts.active.as_slice()[0].generation(), 3);
                assert_eq!(original.generation(), 1);
                assert_eq!(original.secret(), "fixture-secret");
                fs::write(
                    &path,
                    candidate.replace("\nenabled=yes\n", "\nenabled=no\n"),
                )
                .unwrap();
                control.reload().unwrap();
                assert!(control.accounts.active.as_slice().is_empty());
            }
            runtime.stop().unwrap();
            listeners.clear();
            registration.stop();
        }
        fs::remove_file(path).unwrap();
        fs::remove_file(secrets_path).unwrap();
        fs::remove_file(phone_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn inbound_callbacks_reject_invalid_inputs_and_apply_product_policy() {
        use std::{
            ffi::{c_char, c_void},
            mem::size_of,
        };

        unsafe extern "C" fn start(
            _: *const crate::abi::rptadv_host_services_v10,
            _: *const crate::abi::rptadv_control_descriptor_v1,
            _: *const crate::abi::rptadv_file_descriptor,
            _: *const crate::abi::rptadv_speech_descriptor,
            _: *const c_char,
            _: usize,
        ) -> i32 {
            0
        }
        unsafe extern "C" fn reload(_: *const c_char, _: usize) -> i32 {
            0
        }
        unsafe extern "C" fn reload_ready() -> i32 {
            0
        }
        extern "C" fn stop() -> i32 {
            0
        }
        unsafe extern "C" fn authorize(
            request: *const crate::abi::rptadv_incoming_peer_v1,
            _: *mut c_void,
        ) -> i32 {
            let remote = unsafe {
                std::slice::from_raw_parts((*request).remote.data, (*request).remote.length)
            };
            i32::from(remote != b"506315")
        }
        unsafe extern "C" fn incoming(
            _: *const crate::abi::rptadv_incoming_peer_v1,
            _: *mut c_void,
        ) -> i32 {
            1
        }

        let descriptor = Box::leak(Box::new(crate::abi::rptadv_product_descriptor_v1 {
            struct_size: size_of::<crate::abi::rptadv_product_descriptor_v1>() as u32,
            abi_version: 8,
            capability: *b"rptadv.prod8\0\0\0\0",
            start: Some(start),
            reload: Some(reload),
            stop: Some(stop),
            authorize_incoming: Some(authorize),
            incoming: Some(incoming),
            link_command: None,
            site_command: None,
            radio_control: None,
            link_status: None,
            digit: None,
            inspect_configuration: None,
            inspect_secrets: None,
            reload_poll: Some(reload_ready),
        }));
        let host = crate::host_services::HostServicesOwner::new(
            Vec::new(),
            None,
            crate::secrets::SecretsFile::parse("").unwrap(),
        )
        .unwrap();
        let runtime = unsafe {
            crate::runtime::ProductRuntime::start(
                descriptor,
                host,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                "[general]\n",
            )
        }
        .unwrap();
        let mut context = super::InboundContext {
            runtime: &runtime,
            client: test_client(),
            accounts: &crate::phone_accounts::PreparedAccounts::default(),
            listener: std::net::SocketAddr::from(([0, 0, 0, 0], 4569)),
        };
        let context = std::ptr::from_mut(&mut context).cast::<c_void>();
        let listener = bind_iax_listener(&test_client(), 0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(0, listener)]);
        super::enable_iax_listeners(&mut listeners, &runtime).unwrap();
        super::poll_iax_listeners(
            &mut listeners,
            &runtime,
            &test_client(),
            &crate::phone_accounts::PreparedAccounts::default(),
        );
        let local = c"524950";
        let allowed_remote = c"506315";
        let denied_remote = c"506316";
        let source = c"192.0.2.5";
        let invalid = [0xff_u8];

        assert_eq!(
            unsafe {
                super::authorize_inbound(
                    std::ptr::null_mut(),
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::authorize_inbound(
                    context,
                    std::ptr::null(),
                    1,
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::authorize_inbound(
                    context,
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                super::authorize_inbound(
                    context,
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    denied_remote.as_ptr().cast(),
                    denied_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                )
            },
            1
        );

        assert_eq!(
            unsafe {
                super::accept_inbound(
                    std::ptr::null_mut(),
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                    std::ptr::null_mut(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::accept_inbound(
                    context,
                    invalid.as_ptr().cast(),
                    invalid.len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                    std::ptr::null_mut(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::accept_inbound(
                    context,
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                    std::ptr::null_mut(),
                )
            },
            -1
        );

        assert_eq!(
            unsafe {
                super::accept_inbound(
                    context,
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                    std::ptr::null_mut(),
                )
            },
            -1
        );

        let library = Path::new("librptadviax2.so.2");
        // Keep the port unavailable to the suite's loopback and wildcard allocators.
        // The IAX listener uses a separate loopback address without releasing this guard.
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address =
            std::net::SocketAddr::from(([127, 0, 0, 2], reservation.local_addr().unwrap().port()));
        for host in ["127.0.0.1", "0.0.0.0"] {
            assert_eq!(
                UdpSocket::bind((host, address.port())).unwrap_err().kind(),
                std::io::ErrorKind::AddrInUse,
                "the fixture port must remain reserved against {host} binds"
            );
        }
        let mut server = crate::iax::IaxServer::bind(
            &crate::iax::IaxClient::load(library).unwrap(),
            address,
            &["506315".to_owned()],
        )
        .unwrap();
        let mut accepted_peer = 0_usize;
        let server_context = std::ptr::from_mut(&mut accepted_peer).cast::<c_void>();
        let dial = std::thread::spawn(move || {
            let peer = crate::iax::IaxClient::load(library)
                .unwrap()
                .dial(address, "524950", "506315", "", 4000)
                .unwrap();
            drop(peer);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while accepted_peer == 0 && std::time::Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(
                        now,
                        Some(allow_inbound_for_test),
                        Some(capture_inbound_for_test),
                        server_context,
                    )
                    .unwrap();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        assert_ne!(accepted_peer, 0, "loopback IAX call must be accepted");
        assert_eq!(
            unsafe {
                super::accept_inbound(
                    context,
                    local.as_ptr().cast(),
                    local.to_bytes().len(),
                    allowed_remote.as_ptr().cast(),
                    allowed_remote.to_bytes().len(),
                    source.as_ptr().cast(),
                    source.to_bytes().len(),
                    accepted_peer as *mut c_void,
                )
            },
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn secrets_loader_accepts_default_fallback_and_explicit_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let default = Path::new("/etc/rpt_advanced/iax-secrets.conf");
        if !default.exists() {
            super::read_secrets(None).unwrap();
        }

        let path = std::env::temp_dir().join(format!(
            "rpt-advanced-secrets-{}.conf",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "[general]\niax_secret=test-secret\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        super::read_secrets(Some(&path)).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(super::read_secrets(Some(&path)).is_err());
    }

    #[cfg(unix)]
    struct ChildGuard(Child);

    #[cfg(unix)]
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    #[cfg(unix)]
    fn wait_for_log(receiver: &std::sync::mpsc::Receiver<String>, expected: &str) {
        let mut observed = Vec::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(10)) {
                Ok(line) if line.contains(expected) => return,
                Ok(line) => observed.push(line),
                Err(error) => panic!(
                    "did not observe {expected:?}; got {error}; logs: {}",
                    observed.join(" | ")
                ),
            }
        }
    }

    #[cfg(unix)]
    unsafe extern "C" fn allow_inbound_for_test(
        _: *mut std::ffi::c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) -> i32 {
        0
    }

    #[cfg(unix)]
    unsafe extern "C" fn capture_inbound_for_test(
        context: *mut std::ffi::c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        peer: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe { *context.cast::<usize>() = peer as usize };
        0
    }

    #[cfg(unix)]
    fn run_fatal_reload_child(
        configuration: &Path,
        secrets_path: Option<&Path>,
    ) -> Result<(), ForegroundError> {
        use crate::{abi, host_services::HostServicesOwner, runtime::ProductRuntime};
        use std::{cell::Cell, ffi::c_void, sync::OnceLock};

        type Submit = unsafe extern "C" fn(*mut c_void, abi::rptadv_control_task_v1) -> i32;
        static SUBMIT: OnceLock<Submit> = OnceLock::new();
        thread_local! {
            // Only the foreground caller is armed; the real ticker keeps running.
            static REJECT_AFTER: Cell<usize> = const { Cell::new(0) };
        }
        unsafe extern "C" fn submit(
            context: *mut c_void,
            task: abi::rptadv_control_task_v1,
        ) -> i32 {
            let reject = REJECT_AFTER.with(|remaining| {
                let value = remaining.get();
                remaining.set(value.saturating_sub(1));
                value == 1
            });
            if reject {
                // Queue-full rejection leaves the payload with its real caller.
                return 2;
            }
            unsafe { SUBMIT.get().unwrap()(context, task) }
        }

        assert_ne!(unsafe { libc::geteuid() }, 0);
        // The child retains actual provider code even if a failed assertion leaves owners live.
        let providers = Box::leak(Box::new(crate::providers::ProviderSet::load().unwrap()));
        let descriptors = providers.product_runtime_descriptors().unwrap();
        let mut control = *descriptors.control;
        SUBMIT.set(control.submit.unwrap()).unwrap();
        control.submit = Some(submit);
        let control = Box::leak(Box::new(control));
        let text = super::read_configuration(configuration).unwrap();
        let radios = super::resolve_radios(&text).unwrap();
        assert!(radios.iter().all(|radio| !radio.enabled));
        let host = HostServicesOwner::new(radios, None, super::read_secrets(secrets_path).unwrap())
            .unwrap();
        let runtime = unsafe {
            ProductRuntime::start(
                descriptors.product,
                host,
                control,
                descriptors.file,
                descriptors.speech,
                &text,
            )
        }
        .unwrap();
        let client = providers.iax_client().unwrap();
        let registration = crate::registration::RegistrationWorker::start(Vec::new()).unwrap();
        // Accept site_basis, then reject the first mutating handoff submission.
        // Later cleanup submissions continue through the original serialized executor.
        REJECT_AFTER.set(2);
        super::run_runtime(
            configuration,
            secrets_path,
            client,
            Default::default(),
            runtime,
            registration,
            Default::default(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn foreground_child_entry() {
        let Some(configuration) = std::env::var_os("RPT_ADVANCED_FOREGROUND_TEST_CONFIG") else {
            return;
        };
        let secrets = std::env::var_os("RPT_ADVANCED_FOREGROUND_TEST_SECRETS").map(PathBuf::from);
        if std::env::var_os("RPT_ADVANCED_FOREGROUND_EXPECT_FATAL_RELOAD").is_some() {
            assert!(matches!(
                run_fatal_reload_child(Path::new(&configuration), secrets.as_deref()),
                Err(ForegroundError::Runtime(
                    crate::runtime::RuntimeError::ReloadOutcome(
                        rptadv_peer_support::reload::ReloadOutcome::Fatal(-3)
                    )
                ))
            ));
            eprintln!("foreground stopped after fatal reload");
            return;
        }
        let result = super::run(Path::new(&configuration), secrets.as_deref());
        if std::env::var_os("RPT_ADVANCED_FOREGROUND_EXPECT_BIND_ERROR").is_some() {
            let error = result.unwrap_err();
            assert!(
                matches!(error, ForegroundError::LocalControl(_)),
                "{error:?}"
            );
            eprintln!("{error}");
        } else if let Some(path) =
            std::env::var_os("RPT_ADVANCED_FOREGROUND_EXPECT_ECHO_STOP_ERROR")
        {
            assert!(matches!(
                result,
                Err(ForegroundError::Runtime(
                    crate::runtime::RuntimeError::Echo(
                        rptadv_peer_support::echolink::host::Error::Status(
                            rptadv_peer_support::echolink::abi::Status::Failed
                        )
                    )
                ))
            ));
            let library = unsafe { libloading::Library::new(path) }.unwrap();
            for symbol in [
                b"fixture_accounts\0".as_slice(),
                b"fixture_contexts\0".as_slice(),
            ] {
                let retained =
                    unsafe { library.get::<unsafe extern "C" fn() -> usize>(symbol) }.unwrap();
                assert_eq!(unsafe { retained() }, 1);
            }
            eprintln!("foreground retained failed EchoLink owner");
        } else {
            result.unwrap();
        }
    }

    #[cfg(unix)]
    fn spawn_foreground(
        command: &mut Command,
    ) -> (
        ChildGuard,
        std::sync::mpsc::Receiver<String>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{BufRead, BufReader};
        let mut child = ChildGuard(command.spawn().unwrap());
        let stderr = child.0.stderr.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        (child, receiver, reader)
    }

    #[cfg(unix)]
    fn local_request(
        stream: &mut std::os::unix::net::UnixStream,
        request: serde_json::Value,
    ) -> serde_json::Value {
        use std::io::{Read, Write};
        let bytes = serde_json::to_vec(&request).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
        let mut size = [0_u8; 4];
        stream.read_exact(&mut size).unwrap();
        let length = u32::from_be_bytes(size) as usize;
        assert!(length <= 65_536);
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn foreground_reloads_and_shuts_down_without_restarting() {
        use std::{os::unix::fs::PermissionsExt, os::unix::process::CommandExt, thread};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile_prefix = format!("rpt-advanced-foreground-{stamp}-");
        let path = std::env::temp_dir().join(format!("rpt-advanced-{stamp}.conf"));
        let secrets_path = std::env::temp_dir().join(format!("rpt-advanced-secrets-{stamp}.conf"));
        fs::write(&path, "[general]\nnode_enabled=no\n[524950]\n").unwrap();
        fs::write(&secrets_path, "[general]\niax_secret=test-secret\n").unwrap();
        fs::set_permissions(&secrets_path, fs::Permissions::from_mode(0o600)).unwrap();
        let owner = if unsafe { libc::geteuid() } == 0 {
            65534
        } else {
            unsafe { libc::geteuid() }
        };
        let group = if unsafe { libc::getegid() } == 0 {
            65534
        } else {
            unsafe { libc::getegid() }
        };
        // Reproduce systemd's private RuntimeDirectory inside the test container.
        // Never replace or change ownership of an existing runtime directory.
        struct SocketFixture {
            created_directory: bool,
            collision: bool,
        }
        impl Drop for SocketFixture {
            fn drop(&mut self) {
                // A killed child can leave its socket in this fixture's private directory.
                if self.collision || self.created_directory {
                    let _ = fs::remove_file(crate::local_control::SOCKET_PATH);
                }
                if self.created_directory {
                    let _ = fs::remove_dir(
                        Path::new(crate::local_control::SOCKET_PATH)
                            .parent()
                            .unwrap(),
                    );
                }
            }
        }
        let directory = Path::new(crate::local_control::SOCKET_PATH)
            .parent()
            .unwrap();
        let created_directory = match fs::create_dir(directory) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => panic!("cannot prepare test RuntimeDirectory: {error}"),
        };
        let mut socket = SocketFixture {
            created_directory,
            collision: false,
        };
        if created_directory {
            let path = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::chown(path.as_ptr(), owner, group) }, 0);
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let secrets_path_c =
            std::ffi::CString::new(secrets_path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::chown(secrets_path_c.as_ptr(), owner, group) },
            0
        );

        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "foreground::tests::foreground_child_entry",
                "--nocapture",
            ])
            .env("RPT_ADVANCED_FOREGROUND_TEST_CONFIG", &path)
            .env("RPT_ADVANCED_FOREGROUND_TEST_SECRETS", &secrets_path)
            .env(
                "LLVM_PROFILE_FILE",
                std::env::temp_dir()
                    .join(format!("{profile_prefix}%p.profraw"))
                    .display()
                    .to_string(),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::geteuid() == 0
                    && (libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(65534) != 0
                        || libc::setuid(65534) != 0)
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        // A pre-existing entry must survive failed startup without replacement.
        use std::io::Write;
        let mut collision = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(crate::local_control::SOCKET_PATH)
            .unwrap();
        socket.collision = true;
        collision.write_all(b"preserve existing entry").unwrap();
        drop(collision);
        command.env("RPT_ADVANCED_FOREGROUND_EXPECT_BIND_ERROR", "1");
        let (mut rejected, rejected_logs, rejected_reader) = spawn_foreground(&mut command);
        wait_for_log(&rejected_logs, "cannot start local radio control");
        assert!(rejected.0.wait().unwrap().success());
        rejected_reader.join().unwrap();
        assert_eq!(
            fs::read(crate::local_control::SOCKET_PATH).unwrap(),
            b"preserve existing entry"
        );
        fs::remove_file(crate::local_control::SOCKET_PATH).unwrap();
        socket.collision = false;
        command.env_remove("RPT_ADVANCED_FOREGROUND_EXPECT_BIND_ERROR");
        let (mut child, receiver, reader) = spawn_foreground(&mut command);

        wait_for_log(&receiver, "standalone runtime started");
        let mut local =
            std::os::unix::net::UnixStream::connect(crate::local_control::SOCKET_PATH).unwrap();
        local
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(
            local_request(
                &mut local,
                serde_json::json!({"version":1,"operation":"nodes"})
            )["result"],
            serde_json::json!([])
        );
        assert_eq!(
            local_request(
                &mut local,
                serde_json::json!({"version":1,"operation":"reload","node":"524950"})
            )["ok"],
            true
        );
        wait_for_log(&receiver, "configuration reloaded");
        // The disabled fixture node cannot key hardware, but a rejected manual
        // request must still be cleared through the foreground backend on EOF.
        assert_eq!(
            local_request(
                &mut local,
                serde_json::json!({"version":1,"operation":"transmit","node":"524950","keyed":true})
            )["ok"],
            false
        );
        drop(local);
        wait_for_log(&receiver, "manual radio cleanup for 524950 failed");
        // Exercise ordinary idle polling before driving the reload requests.
        thread::sleep(Duration::from_millis(100));
        fs::write(
            &path,
            "[general]\nnode_enabled=no\nstatus_snapshot_interval_ms=100\n[524950]\n",
        )
        .unwrap();
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        wait_for_log(&receiver, "configuration reloaded");
        fs::set_permissions(&secrets_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        wait_for_log(&receiver, "registration configuration reload rejected");
        fs::set_permissions(&secrets_path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, "[general\n").unwrap();
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        wait_for_log(
            &receiver,
            "configuration reload failed: line 1: malformed section or option",
        );
        let mut local =
            std::os::unix::net::UnixStream::connect(crate::local_control::SOCKET_PATH).unwrap();
        local
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(
            local_request(
                &mut local,
                serde_json::json!({"version":1,"operation":"reload","node":"524950"})
            )["ok"],
            false
        );
        drop(local);
        // Pass configuration inspection, then reject listener preparation before
        // any radio can activate. A parse error exercises a different reload arm.
        let occupied = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let candidate = format!(
            "[general]\nnode_enabled=yes\niax_local_port={port}\n[524950]\nradio_channel=fixture\n"
        );
        super::validate_configuration(&candidate).unwrap();
        assert_eq!(super::resolve_radios(&candidate).unwrap()[0].iax_port, port);
        fs::write(&path, candidate).unwrap();
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        wait_for_log(
            &receiver,
            "configuration reload failed: cannot bind the IAX2 listener",
        );
        drop(occupied);
        fs::write(&path, "[general]\nnode_enabled=no\n[524950]\n").unwrap();
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        wait_for_log(&receiver, "configuration reloaded");
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        wait_for_log(&receiver, "runtime stopped");
        assert!(child.0.wait().unwrap().success());
        reader.join().unwrap();

        // A documented control-provider failure during mutating reload must stop
        // the foreground loop itself, without a signal or a second lifecycle owner.
        command.env("RPT_ADVANCED_FOREGROUND_EXPECT_FATAL_RELOAD", "1");
        let (mut fatal, fatal_logs, fatal_reader) = spawn_foreground(&mut command);
        wait_for_log(&fatal_logs, "standalone runtime started");
        let mut local =
            std::os::unix::net::UnixStream::connect(crate::local_control::SOCKET_PATH).unwrap();
        let request = br#"{"version":1,"operation":"reload","node":"524950"}"#;
        local
            .write_all(&(request.len() as u32).to_be_bytes())
            .unwrap();
        local.write_all(request).unwrap();
        // Fatal cleanup can close the session before its queued reply is flushed.
        wait_for_log(&fatal_logs, "foreground stopped after fatal reload");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while fatal.0.try_wait().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "fatal reload child timed out"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(fatal.0.wait().unwrap().success());
        fatal_reader.join().unwrap();
        drop(local);
        assert!(!Path::new(crate::local_control::SOCKET_PATH).exists());
        command.env_remove("RPT_ADVANCED_FOREGROUND_EXPECT_FATAL_RELOAD");

        // The native fixture's account 99 cannot quiesce. A disabled radio
        // exercises real process shutdown without opening hardware or IAX listeners.
        let fixture = crate::host_services::echo_tests::support::Fixture::new("network");
        let directory = fixture.0.parent().unwrap();
        std::os::unix::fs::symlink(&fixture.0, directory.join("librptadv_echolink.so.1")).unwrap();
        let echo_secrets = directory.join("echolink-secrets.conf");
        fs::write(
            &echo_secrets,
            "[524950]\ndirectory_password=fixture-value\n",
        )
        .unwrap();
        fs::set_permissions(&echo_secrets, fs::Permissions::from_mode(0o600)).unwrap();
        let echo_secrets_c =
            std::ffi::CString::new(echo_secrets.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::chown(echo_secrets_c.as_ptr(), owner, group) },
            0
        );
        let candidate = format!(
            "[524950]\nnode_enabled=no\n[echolink 524950]\nenabled=yes\ncallsign=K1ABC-R\nnode_number=99\nsecrets_file={}\n",
            echo_secrets.display()
        );
        let radios = super::resolve_radios(&candidate).unwrap();
        assert!(
            crate::iax::listeners::resolve_profiles(
                &radios,
                &crate::phone_accounts::PreparedAccounts::default()
            )
            .unwrap()
            .is_empty()
        );
        fs::write(&path, candidate).unwrap();
        let mut search = vec![directory.to_owned()];
        search.extend(std::env::split_paths(
            &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
        ));
        command
            .env("LD_LIBRARY_PATH", std::env::join_paths(search).unwrap())
            .env("RPT_ADVANCED_FOREGROUND_EXPECT_ECHO_STOP_ERROR", &fixture.0);
        let (mut failed_stop, stop_logs, stop_reader) = spawn_foreground(&mut command);
        wait_for_log(&stop_logs, "standalone runtime started");
        assert_eq!(
            unsafe { libc::kill(failed_stop.0.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        wait_for_log(&stop_logs, "foreground retained failed EchoLink owner");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = failed_stop.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shutdown child timed out"
            );
            thread::sleep(Duration::from_millis(5));
        }
        stop_reader.join().unwrap();
        if std::env::var_os("CARGO_LLVM_COV").is_some() {
            let profiles: Vec<_> = fs::read_dir(std::env::temp_dir())
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|profile| {
                    profile
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(&profile_prefix))
                })
                .collect();
            assert!(
                !profiles.is_empty(),
                "foreground children must write coverage profiles"
            );
            for profile in profiles {
                let target = std::env::var_os("LLVM_PROFILE_FILE").unwrap();
                let target = Path::new(&target)
                    .parent()
                    .unwrap()
                    .join(profile.file_name().unwrap());
                fs::copy(&profile, target).unwrap();
                fs::remove_file(profile).unwrap();
            }
        }
        fs::remove_file(path).unwrap();
        fs::remove_file(secrets_path).unwrap();
    }

    #[test]
    fn inbound_listener_settings_group_nodes_by_effective_iax_port() {
        let text = "[general]\nnode_enabled=yes\niax_local_port=4569\n[1000]\n[radio 1000]\n[2000]\niax_local_port=4570\n[radio 2000]\n";
        let radios = super::resolve_radios(text).unwrap();
        let accounts = crate::phone_accounts::PreparedAccounts::default();
        let listeners = crate::iax::listeners::resolve_profiles(&radios, &accounts).unwrap();

        assert_eq!(listeners.get(&4569).unwrap().local_nodes, &["1000"]);
        assert_eq!(listeners.get(&4570).unwrap().local_nodes, &["2000"]);
    }

    #[test]
    fn inbound_listener_drains_queued_packets_but_bounds_each_cycle() {
        let mut received = 0;
        super::drain_iax_datagrams(|| {
            received += 1;
            Ok(received < 3)
        })
        .unwrap();
        assert_eq!(received, 3);

        received = 0;
        super::drain_iax_datagrams(|| {
            received += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(received, super::MAX_IAX_DATAGRAMS_PER_POLL);
    }

    #[cfg(unix)]
    #[test]
    fn foreground_mode_refuses_root_before_loading_runtime_providers() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let error = super::run(std::path::Path::new("/missing/config"), None).unwrap_err();
        assert!(matches!(error, ForegroundError::RootUser));
    }
}

#[cfg(all(test, unix))]
#[path = "foreground_accounts_tests.rs"]
mod accounts_tests;
