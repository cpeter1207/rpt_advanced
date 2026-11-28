//! Standalone process startup, SIGHUP reload, and graceful signal shutdown.

#[cfg(target_os = "linux")]
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[cfg(target_os = "linux")]
static STOP: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "linux")]
static RELOAD: AtomicBool = AtomicBool::new(false);

/// A foreground process failed startup, reload, or shutdown.
#[derive(Debug)]
pub enum ForegroundError {
    /// The daemon must run under its dedicated unprivileged service account.
    RootUser,
    /// The protected service document or signal initialization failed.
    Configuration(String),
    /// The portable product could not start or stop cleanly.
    Runtime(crate::runtime::RuntimeError),
    /// The host cannot provide Unix signal and privilege semantics.
    UnsupportedPlatform,
}

impl std::fmt::Display for ForegroundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootUser => formatter.write_str("rpt-advanced must run as an unprivileged user"),
            Self::Configuration(error) => formatter.write_str(error),
            Self::Runtime(error) => error.fmt(formatter),
            Self::UnsupportedPlatform => {
                formatter.write_str("foreground mode requires a Unix system")
            }
        }
    }
}

impl std::error::Error for ForegroundError {}

/// Run the modular service until signal shutdown, retaining all owners while stop is pending.
#[cfg(target_os = "linux")]
pub fn run_managed(path: &Path) -> Result<(), ForegroundError> {
    // SAFETY: geteuid takes no arguments and returns the process identity.
    if unsafe { libc::geteuid() } == 0 {
        return Err(ForegroundError::RootUser);
    }
    let provisioning = rptadv_peer_support::working_files::read_service_provisioning(path)
        .map_err(|e| ForegroundError::Configuration(e.to_string()))?;
    install_signal_handlers()?;
    let (mut runtime, started) = crate::runtime::ProductRuntime::start_management(&provisioning)
        .map_err(ForegroundError::Runtime)?;
    if started {
        eprintln!("rpt-advanced management service started");
        let mut pending = false;
        while !STOP.load(Ordering::Acquire) {
            if pending || RELOAD.swap(false, Ordering::AcqRel) {
                match runtime.working_reload(pending) {
                    Ok(next) => pending = next,
                    Err(error) => {
                        pending = false;
                        eprintln!("working configuration reload rejected ({error})");
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // The product may have accepted callbacks before an unconfirmed startup
    // failure. DSO/context ownership survives each unsuccessful drain attempt.
    while runtime.stop().is_err() {
        std::thread::sleep(Duration::from_millis(20));
    }
    if started {
        Ok(())
    } else {
        Err(ForegroundError::Runtime(
            crate::runtime::RuntimeError::Start,
        ))
    }
}

#[cfg(target_os = "linux")]
extern "C" fn handle_signal(signal: libc::c_int) {
    if signal == libc::SIGHUP {
        RELOAD.store(true, Ordering::Release);
    } else {
        STOP.store(true, Ordering::Release);
    }
}

#[cfg(target_os = "linux")]
fn install_signal_handlers() -> Result<(), ForegroundError> {
    // SAFETY: handler only stores lock-free atomics; all file/product work runs on main thread.
    install_signal_handlers_with(|signal| unsafe {
        libc::signal(signal, handle_signal as *const () as libc::sighandler_t) != libc::SIG_ERR
    })
}

#[cfg(target_os = "linux")]
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::ForegroundError;

    #[test]
    fn foreground_error_formats_each_public_failure() {
        let errors = [
            (ForegroundError::RootUser, "unprivileged user"),
            (
                ForegroundError::Configuration("bad config".into()),
                "bad config",
            ),
            (
                ForegroundError::Runtime(crate::runtime::RuntimeError::Start),
                "product start",
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
    fn signal_handler_setup_reports_installation_failure() {
        super::install_signal_handlers_with(|signal| signal != libc::SIGTERM).unwrap_err();
        super::install_signal_handlers_with(|_| true).unwrap();
    }

    #[test]
    fn foreground_mode_refuses_root_before_loading_runtime_providers() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let error = super::run_managed(std::path::Path::new("/missing/config")).unwrap_err();
        assert!(matches!(error, ForegroundError::RootUser));
    }
}
