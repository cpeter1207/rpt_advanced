//! Asterisk registration and configuration translation around the portable product owner.

use crate::{bindings as ffi, module::Registration, product::Product};
use std::{
    ffi::{CStr, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::Mutex,
};

struct State {
    product: Product,
    registration: Option<Registration>,
    services: crate::services::HostServicesOwner,
    configuration: String,
    path: PathBuf,
    control: EchoBackend,
}

#[derive(Clone, Copy)]
struct EchoBackend(*const ffi::rptadv_control_descriptor_v1);
// Immutable selected table/code is retained by the loader through product stop.
unsafe impl Send for EchoBackend {}
static STATE: Mutex<Option<State>> = Mutex::new(None);
#[cfg(test)]
static TEST_PRODUCT: Mutex<Option<Product>> = Mutex::new(None);

/// Versioned public-Asterisk lifecycle descriptor consumed by the C loader.
#[repr(C)]
pub struct Descriptor {
    /// Complete table size.
    pub struct_size: u32,
    /// Exact incompatible ABI version.
    pub abi_version: u32,
    /// Exact capability name including NUL termination.
    pub capability: [u8; 16],
    /// Prepare the product and register public Asterisk callbacks.
    pub load: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const ffi::rptadv_product_descriptor_v1,
            *const ffi::rptadv_control_descriptor_v1,
            *const ffi::rptadv_file_descriptor,
            *const ffi::rptadv_speech_descriptor,
        ) -> i32,
    >,
    /// Replace copied configuration.
    pub reload: Option<extern "C" fn() -> i32>,
    /// Stop the product, then unregister callbacks.
    pub unload: Option<extern "C" fn() -> i32>,
}

fn configuration_path() -> Option<PathBuf> {
    let directory = unsafe { ffi::ast_config_AST_CONFIG_DIR };
    if directory.is_null() {
        return None;
    }
    let directory = unsafe { CStr::from_ptr(directory) }.to_str().ok()?;
    Some(Path::new(directory).join("rpt_advanced.conf"))
}

fn configuration() -> Option<String> {
    std::fs::read_to_string(configuration_path()?).ok()
}

fn with_echo_preparation<T>(
    path: &Path,
    product: Product,
    control: EchoBackend,
    operation: impl FnOnce(rptadv_peer_support::echolink::host::Preparation<'_>) -> T,
) -> Option<T> {
    let jitter = Path::new(env!("RPT_ADVANCED_LIBDIR")).join("librptadv_jitter_adapter.so.1");
    let bytes = |value: &str| rptadv_peer_support::echolink::abi::ByteSpan {
        data: value.as_ptr(),
        length: value.len(),
    };
    let resources = rptadv_peer_support::echolink::abi::RuntimeConfig {
        gsm_library: bytes("librptadv_codec_gsm.so.1"),
        speex_library: bytes("librptadv_codec_speex.so.1"),
        jitter_library: bytes(jitter.to_str()?),
        ..Default::default()
    };
    Some(operation(unsafe {
        rptadv_peer_support::echolink::host::Preparation::new(
            path,
            Path::new("librptadv_echolink.so.1"),
            &resources,
            control.0.cast(),
            move |peer| product.incoming_echo(peer),
        )
        .with_notice(crate::services::echo_notice)
    }))
}

fn echo_reload_error(error: impl std::fmt::Debug) {
    let text =
        std::ffi::CString::new(format!("{error:?}")).expect("typed Echo errors contain no NUL");
    unsafe {
        ffi::ast_log(
            ffi::__LOG_ERROR as i32,
            c"rust/asterisk/lifecycle.rs".as_ptr(),
            0,
            c"reload".as_ptr(),
            c"rpt_advanced: EchoLink reload: %s\n".as_ptr(),
            text.as_ptr(),
        )
    };
}

/// Return the selected product while registrations retain its callback code.
pub(crate) fn product() -> Option<Product> {
    #[cfg(test)]
    if let Some(product) = *TEST_PRODUCT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
    {
        return Some(product);
    }
    STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map(|state| state.product)
}

unsafe extern "C" fn load(
    module: *mut c_void,
    product: *const ffi::rptadv_product_descriptor_v1,
    control: *const ffi::rptadv_control_descriptor_v1,
    file: *const ffi::rptadv_file_descriptor,
    speech: *const ffi::rptadv_speech_descriptor,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let mut selected = STATE.lock().unwrap_or_else(|error| error.into_inner());
        if selected.is_some() {
            return 1;
        }
        let Some(product) = (unsafe { Product::open(product) }) else {
            return 1;
        };
        let Some(configuration) = configuration() else {
            return 1;
        };
        let Some(path) = configuration_path() else {
            return 1;
        };
        let mut services = crate::services::HostServicesOwner::new();
        let prepared =
            with_echo_preparation(&path, product, EchoBackend(control), |preparation| unsafe {
                services.prepare_echo(product.descriptor(), &configuration, preparation)
            });
        if !matches!(prepared, Some(Ok(()))) {
            return 1;
        }
        if !unsafe { product.start(services.descriptor(), control, file, speech, &configuration) } {
            return 1;
        }
        let registration = if unsafe { services.activate_echo() }.is_ok() {
            unsafe {
                Registration::register(
                    module.cast(),
                    crate::application::callback,
                    [
                        crate::cli::link_callback,
                        crate::cli::alias_callback,
                        crate::cli::command_callback,
                        crate::cli::site_callback,
                    ],
                )
            }
        } else {
            Err(crate::Error::Admission)
        };
        match registration {
            Ok(registration) => {
                *selected = Some(State {
                    product,
                    registration: Some(registration),
                    services,
                    configuration,
                    path,
                    control: EchoBackend(control),
                });
                0
            }
            Err(_) => {
                if services.stop_echo().is_ok() && product.stop() {
                    1
                } else {
                    // A live product cannot be abandoned while its callback code unloads.
                    *selected = Some(State {
                        product,
                        registration: None,
                        services,
                        configuration,
                        path,
                        control: EchoBackend(control),
                    });
                    0
                }
            }
        }
    }))
    .unwrap_or(1)
}

extern "C" fn reload() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let mut selected = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = selected.as_mut() else {
            return -1;
        };
        let Ok(configuration) = std::fs::read_to_string(&state.path) else {
            return -1;
        };
        let result = with_echo_preparation(
            &state.path,
            state.product,
            state.control,
            |preparation| unsafe {
                state.services.reload_echo(
                    state.product.descriptor(),
                    &configuration,
                    preparation,
                    || state.product.reload(&configuration),
                    || state.product.reload(&state.configuration),
                )
            },
        );
        match result {
            Some(Ok(outcome)) => {
                if let Some(error) = outcome.retirement {
                    echo_reload_error(error);
                }
                state.configuration = configuration;
                0
            }
            Some(Err(error)) => {
                echo_reload_error(error);
                -1
            }
            None => -1,
        }
    }))
    .unwrap_or(-1)
}

extern "C" fn unload() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let mut selected = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = selected.as_ref() else {
            return 0;
        };
        if state.services.stop_echo().is_err() || !state.product.stop() {
            return -1;
        }
        let state = selected.take().expect("selected lifecycle remains owned");
        drop(state.registration);
        0
    }))
    .unwrap_or(-1)
}

static DESCRIPTOR: Descriptor = Descriptor {
    struct_size: size_of::<Descriptor>() as u32,
    abi_version: 1,
    capability: *b"rptadv.asterisk\0",
    load: Some(load),
    reload: Some(reload),
    unload: Some(unload),
};

/// Return immutable process-lifetime Asterisk entry metadata.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_asterisk_descriptor_v1() -> *const Descriptor {
    &DESCRIPTOR
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
pub(crate) mod tests;
