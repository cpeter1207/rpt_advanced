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
        let providers = crate::providers::ProviderSet::load().map_err(ForegroundError::Provider)?;
        let mut iax_listeners = bind_iax_listeners(&text, &radios)?;
        let mut runtime =
            crate::runtime::ProductRuntime::start_with_providers(&text, radios, secrets, providers)
                .map_err(ForegroundError::Runtime)?;
        let mut registration = crate::registration::RegistrationWorker::start(registrations)
            .map_err(ForegroundError::Registration)?;
        install_signal_handlers()?;
        eprintln!("rpt-advanced standalone runtime started");
        while !STOP.load(Ordering::Acquire) {
            poll_iax_listeners(&mut iax_listeners, &runtime);
            if RELOAD.swap(false, Ordering::AcqRel) {
                match read_configuration(configuration).and_then(|updated| {
                    validate_configuration(&updated)?;
                    Ok(updated)
                }) {
                    Ok(updated) => match resolve_radios(&updated).and_then(|radios| {
                        let nodes = resolve_listener_nodes(&updated, &radios)?;
                        update_iax_listeners(&mut iax_listeners, nodes, || {
                            runtime
                                .reload_with_radios(&updated, radios)
                                .map_err(ForegroundError::Runtime)
                        })
                    }) {
                        Ok(()) => {
                            match read_secrets(secrets_path)
                                .and_then(|secrets| registration_targets(&updated, &secrets))
                            {
                                Ok(targets) => registration.replace(targets),
                                Err(error) => {
                                    eprintln!("registration configuration reload rejected: {error}")
                                }
                            }
                            eprintln!("rpt-advanced configuration reloaded");
                        }
                        Err(error) => eprintln!("configuration reload rejected: {error}"),
                    },
                    Err(error) => eprintln!("configuration reload rejected: {error}"),
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        runtime.stop().map_err(ForegroundError::Runtime)?;
        iax_listeners.clear();
        registration.stop();
        eprintln!("rpt-advanced standalone runtime stopped");
        Ok(())
    }
}

fn resolve_listener_nodes(
    text: &str,
    radios: &[crate::ResolvedRadioNode],
) -> Result<BTreeMap<u16, Vec<String>>, ForegroundError> {
    let document = rpt_advanced_core::config::ConfigDocument::parse(text)
        .map_err(|error| ForegroundError::Configuration(error.to_string()))?;
    let mut listeners = BTreeMap::<u16, Vec<String>>::new();
    for radio in radios.iter().filter(|radio| radio.enabled) {
        let node = rpt_advanced_core::config::ResolvedNodeSettings::resolve(&document, &radio.node)
            .map_err(|error| ForegroundError::Configuration(error.to_string()))?;
        listeners
            .entry(node.value.iax_local_port)
            .or_default()
            .push(radio.node.as_str().to_owned());
    }
    Ok(listeners)
}

fn bind_iax_listeners(
    text: &str,
    radios: &[crate::ResolvedRadioNode],
) -> Result<BTreeMap<u16, crate::iax::IaxServer>, ForegroundError> {
    resolve_listener_nodes(text, radios)?
        .into_iter()
        .map(|(port, nodes)| Ok((port, bind_iax_listener(port, &nodes)?)))
        .collect()
}

fn bind_iax_listener(
    port: u16,
    nodes: &[String],
) -> Result<crate::iax::IaxServer, ForegroundError> {
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    crate::iax::IaxServer::bind(&iax_library_path(), address, nodes).map_err(ForegroundError::Iax)
}

fn update_iax_listeners(
    listeners: &mut BTreeMap<u16, crate::iax::IaxServer>,
    nodes: BTreeMap<u16, Vec<String>>,
    reload_product: impl FnOnce() -> Result<(), ForegroundError>,
) -> Result<(), ForegroundError> {
    let mut added = BTreeMap::new();
    for (port, local_nodes) in &nodes {
        if !listeners.contains_key(port) {
            added.insert(*port, bind_iax_listener(*port, local_nodes)?);
        }
    }
    reload_product()?;
    for (port, local_nodes) in &nodes {
        if let Some(listener) = listeners.get_mut(port) {
            listener
                .set_local_nodes(local_nodes)
                .map_err(ForegroundError::Iax)?;
        }
    }
    listeners.retain(|port, _| nodes.contains_key(port));
    listeners.extend(added);
    Ok(())
}

fn iax_library_path() -> PathBuf {
    PathBuf::from("librptadviax2.so.1")
}

fn poll_iax_listeners(
    listeners: &mut BTreeMap<u16, crate::iax::IaxServer>,
    runtime: &crate::runtime::ProductRuntime,
) {
    let mut context = InboundContext {
        runtime,
        library: iax_library_path(),
    };
    let pointer = ptr::from_mut(&mut context).cast::<c_void>();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as u32);
    for listener in listeners.values_mut() {
        report_iax_poll(drain_iax_datagrams(|| unsafe {
            listener.poll(now, authorize_inbound, accept_inbound, pointer)
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
    library: PathBuf,
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
    let Ok(client) = crate::iax::IaxClient::load(&context.library) else {
        return -1;
    };
    // SAFETY: the matching library server transfers this unique inbound IaxPeer handle here.
    let Ok(peer) = (unsafe { client.adopt_inbound(peer) }) else {
        return -1;
    };
    let _ = context.runtime.incoming(local, remote, source, peer);
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
    let document = rpt_advanced_core::config::ConfigDocument::parse(text)
        .map_err(|error| ForegroundError::Configuration(error.to_string()))?;
    crate::resolve_radio_nodes(&document)
        .map(|resolution| resolution.value)
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
    use super::ForegroundError;
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
        let error = match super::bind_iax_listener(port, &["506315".to_owned()]) {
            Err(error) => error,
            Ok(_) => panic!("binding an occupied IAX port must fail"),
        };
        assert!(matches!(
            error,
            ForegroundError::Iax(crate::iax::IaxError::Bind)
        ));

        drop(occupied);
        let listener = super::bind_iax_listener(0, &["506315".to_owned()]).unwrap();
        drop(listener);
    }

    #[test]
    fn listener_reload_commits_new_nodes_only_after_product_reload() {
        let reservation = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        let current_port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let stale = super::bind_iax_listener(0, &["506310".to_owned()]).unwrap();
        let current = super::bind_iax_listener(current_port, &["506315".to_owned()]).unwrap();
        let mut listeners =
            std::collections::BTreeMap::from([(current_port, current), (65000, stale)]);
        let nodes = std::collections::BTreeMap::from([
            (current_port, vec!["506316".to_owned()]),
            (0, vec!["55553".to_owned()]),
        ]);
        let mut reloaded = false;

        super::update_iax_listeners(&mut listeners, nodes, || {
            reloaded = true;
            Ok(())
        })
        .unwrap();

        assert!(reloaded);
        assert_eq!(
            listeners.keys().copied().collect::<Vec<_>>(),
            [0, current_port]
        );
    }

    #[test]
    fn listener_reload_keeps_current_set_when_product_reload_fails() {
        let current = super::bind_iax_listener(0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(4569, current)]);
        let nodes = std::collections::BTreeMap::from([(0, vec!["55553".to_owned()])]);

        let error = super::update_iax_listeners(&mut listeners, nodes, || {
            Err(ForegroundError::Runtime(
                crate::runtime::RuntimeError::Reload,
            ))
        })
        .unwrap_err();

        assert!(matches!(
            error,
            ForegroundError::Runtime(crate::runtime::RuntimeError::Reload)
        ));
        assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569]);
    }

    #[test]
    fn listener_reload_rejects_invalid_existing_node_assignment() {
        let current = super::bind_iax_listener(0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(4569, current)]);
        let nodes = std::collections::BTreeMap::from([(4569, Vec::new())]);

        let error = super::update_iax_listeners(&mut listeners, nodes, || Ok(())).unwrap_err();

        assert!(matches!(
            error,
            ForegroundError::Iax(crate::iax::IaxError::Operation)
        ));
        assert_eq!(listeners.keys().copied().collect::<Vec<_>>(), [4569]);
    }

    #[cfg(unix)]
    #[test]
    fn inbound_callbacks_reject_invalid_inputs_and_apply_product_policy() {
        use std::{
            ffi::{c_char, c_void},
            mem::size_of,
            path::PathBuf,
        };

        unsafe extern "C" fn start(
            _: *const crate::abi::rptadv_host_services_v4,
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
        extern "C" fn stop() -> i32 {
            0
        }
        unsafe extern "C" fn authorize(
            _: *const c_char,
            _: usize,
            remote: *const c_char,
            remote_length: usize,
            _: *const c_char,
            _: usize,
        ) -> i32 {
            let remote = unsafe { std::slice::from_raw_parts(remote.cast::<u8>(), remote_length) };
            i32::from(remote != b"506315")
        }
        unsafe extern "C" fn incoming(
            _: *const c_char,
            _: usize,
            _: *const c_char,
            _: usize,
            _: *const c_char,
            _: usize,
            _: *mut c_void,
        ) -> i32 {
            1
        }

        let descriptor = Box::leak(Box::new(crate::abi::rptadv_product_descriptor_v1 {
            struct_size: size_of::<crate::abi::rptadv_product_descriptor_v1>() as u32,
            abi_version: 3,
            capability: *b"rptadv.prod3\0\0\0\0",
            start: Some(start),
            reload: Some(reload),
            stop: Some(stop),
            authorize_incoming: Some(authorize),
            incoming: Some(incoming),
            link_command: None,
            link_status: None,
            digit: None,
        }));
        let host = crate::host_services::HostServicesOwner::new(
            Vec::new(),
            None,
            crate::secrets::SecretsFile::parse("").unwrap(),
        );
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
            library: PathBuf::from("missing-librptadviax2.so"),
        };
        let context = std::ptr::from_mut(&mut context).cast::<c_void>();
        let listener = super::bind_iax_listener(0, &["506315".to_owned()]).unwrap();
        let mut listeners = std::collections::BTreeMap::from([(0, listener)]);
        super::poll_iax_listeners(&mut listeners, &runtime);
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

        unsafe { context.cast::<super::InboundContext<'_>>().as_mut() }
            .unwrap()
            .library = PathBuf::from("librptadviax2.so.1");
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

        let library = Path::new("librptadviax2.so.1");
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut server =
            crate::iax::IaxServer::bind(library, address, &["506315".to_owned()]).unwrap();
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
                        allow_inbound_for_test,
                        capture_inbound_for_test,
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
    #[test]
    fn foreground_child_entry() {
        let Some(configuration) = std::env::var_os("RPT_ADVANCED_FOREGROUND_TEST_CONFIG") else {
            return;
        };
        let secrets = std::env::var_os("RPT_ADVANCED_FOREGROUND_TEST_SECRETS").map(PathBuf::from);
        super::run(Path::new(&configuration), secrets.as_deref()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn foreground_reloads_and_shuts_down_without_restarting() {
        use std::{
            io::{BufRead, BufReader},
            os::unix::fs::PermissionsExt,
            os::unix::process::CommandExt,
            sync::mpsc,
            thread,
        };

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

        let mut child = ChildGuard(command.spawn().unwrap());
        let stderr = child.0.stderr.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        wait_for_log(&receiver, "standalone runtime started");
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
        wait_for_log(&receiver, "configuration reload rejected");
        fs::write(
            &path,
            "[general]\nnode_enabled=no\n[524950]\nlink_command_disconnect=3\n",
        )
        .unwrap();
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        wait_for_log(&receiver, "configuration reload rejected");
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
        if std::env::var_os("CARGO_LLVM_COV").is_some() {
            let profile = fs::read_dir(std::env::temp_dir())
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|profile| {
                    profile
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(&profile_prefix))
                })
                .expect("foreground child must write its instrumented coverage profile");
            let target = std::env::var_os("LLVM_PROFILE_FILE").unwrap();
            let target = Path::new(&target)
                .parent()
                .unwrap()
                .join(profile.file_name().unwrap());
            fs::copy(&profile, target).unwrap();
            fs::remove_file(profile).unwrap();
        }
        fs::remove_file(path).unwrap();
        fs::remove_file(secrets_path).unwrap();
    }

    #[test]
    fn inbound_listener_settings_group_nodes_by_effective_iax_port() {
        let text = "[general]\nnode_enabled=yes\niax_local_port=4569\n[1000]\n[radio 1000]\n[2000]\niax_local_port=4570\n[radio 2000]\n";
        let radios = super::resolve_radios(text).unwrap();
        let listeners = super::resolve_listener_nodes(text, &radios).unwrap();

        assert_eq!(listeners.get(&4569).unwrap(), &["1000"]);
        assert_eq!(listeners.get(&4570).unwrap(), &["2000"]);
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
