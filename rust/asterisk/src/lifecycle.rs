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
    source: Option<LegacySource>,
}

struct LegacySource {
    configuration: String,
    path: PathBuf,
    control: EchoBackend,
}

#[derive(Clone, Copy)]
struct EchoBackend(*const ffi::rptadv_control_descriptor_v1);
// Immutable selected table/code is retained by the loader through product stop.
unsafe impl Send for EchoBackend {}
static STATE: Mutex<Option<State>> = Mutex::new(None);
// A reload polls outside STATE so product callbacks can look up their owner. Keep the
// loader's DSO and callback ownership through the final poll, including on reentry.
static TRANSITION: Mutex<()> = Mutex::new(());
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

fn configuration_path(name: &str) -> Option<PathBuf> {
    let directory = unsafe { ffi::ast_config_AST_CONFIG_DIR };
    if directory.is_null() {
        return None;
    }
    let directory = unsafe { CStr::from_ptr(directory) }.to_str().ok()?;
    Some(Path::new(directory).join(name))
}

/// Retain the resolved path with the document read from it for one load transaction.
fn configuration() -> Option<(PathBuf, String)> {
    let path = configuration_path("rpt_advanced.conf")?;
    let document = std::fs::read_to_string(&path).ok()?;
    Some((path, document))
}

fn with_echo_preparation<T>(
    path: &Path,
    product: Product,
    control: EchoBackend,
    operation: impl FnOnce(rptadv_peer_support::echolink::host::Preparation<'_>) -> T,
) -> T {
    let jitter = Path::new(env!("RPT_ADVANCED_LIBDIR")).join("librptadv_jitter_adapter.so.1");
    let bytes = |value: &str| rptadv_peer_support::echolink::abi::ByteSpan {
        data: value.as_ptr(),
        length: value.len(),
    };
    let resources = rptadv_peer_support::echolink::abi::RuntimeConfig {
        gsm_library: bytes("librptadv_codec_gsm.so.1"),
        speex_library: bytes("librptadv_codec_speex.so.1"),
        jitter_library: bytes(
            jitter
                .to_str()
                .expect("UTF-8 build path with an ASCII filename"),
        ),
        ..Default::default()
    };
    operation(unsafe {
        rptadv_peer_support::echolink::host::Preparation::new(
            path,
            Path::new("librptadv_echolink.so.1"),
            &resources,
            control.0.cast(),
            move |peer| product.incoming_echo(peer),
        )
        .with_notice(crate::services::echo_notice)
    })
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

unsafe fn register(module: *mut c_void) -> Result<Registration, crate::Error> {
    // SAFETY: Asterisk keeps the module and callback code live through confirmed unload.
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
}

/// Start from the trusted service document in Asterisk's configured directory.
///
/// # Safety
/// The loader owns every descriptor and its DSO until this adapter confirms unload.
/// The management table advertises its accessible immutable size prefix.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rptadv_asterisk_load_management_v1(
    module: *mut c_void,
    product: *const ffi::rptadv_product_descriptor_v1,
    management: *const ffi::rptadv_product_management_v1,
    control: *const ffi::rptadv_control_descriptor_v1,
    file: *const ffi::rptadv_file_descriptor,
    speech: *const ffi::rptadv_speech_descriptor,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let mut selected = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if selected.is_some()
            || module.is_null()
            || management.is_null()
            || !management.is_aligned()
        {
            return 1;
        }
        // SAFETY: the loader lends at least the public length word.
        if unsafe { std::ptr::addr_of!((*management).struct_size).read() } as usize
            != size_of::<ffi::rptadv_product_management_v1>()
        {
            return 1;
        }
        // SAFETY: complete table was checked before borrowing its fields.
        let management = unsafe { &*management };
        if management.abi_version != 1 || management.capability != *b"rptadv.mgmt1\0\0\0\0" {
            return 1;
        }
        let Some(start) = management.start else {
            return 1;
        };
        // SAFETY: this separately versioned table belongs to the same retained product DSO.
        let Some(product) = (unsafe { Product::open(product) }) else {
            return 1;
        };
        let Some(path) = configuration_path("rpt_advanced.service.json") else {
            return 1;
        };
        let Ok(bytes) = rptadv_peer_support::working_files::read_service_provisioning(&path) else {
            return 1;
        };
        let mut services = crate::services::HostServicesOwner::new();
        services.install_private(product.descriptor());
        // SAFETY: input is borrowed synchronously and the live host is retained
        // after acceptance or uncertain startup, until the product confirms stop.
        let result = unsafe {
            start(
                services.descriptor(),
                control,
                file,
                speech,
                bytes.as_ptr().cast(),
                bytes.len(),
            )
        };
        if !matches!(result, 0 | -2) {
            return 1;
        }
        let registration = if result == 0 {
            // SAFETY: these are Asterisk's current module and callbacks.
            match unsafe { register(module) } {
                Ok(registration) => Some(registration),
                Err(_) if product.stop() => return 1,
                Err(_) => None,
            }
        } else {
            None
        };
        *selected = Some(State {
            product,
            services,
            registration,
            source: None,
        });
        0
    }))
    .unwrap_or(1)
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
        let Some((path, configuration)) = configuration() else {
            return 1;
        };
        let mut services = crate::services::HostServicesOwner::new();
        services.install_private(product.descriptor());
        let prepared =
            with_echo_preparation(&path, product, EchoBackend(control), |preparation| unsafe {
                services.prepare_echo(product.descriptor(), &configuration, preparation)
            });
        if prepared.is_err() {
            return 1;
        }
        if !unsafe { product.start(services.descriptor(), control, file, speech, &configuration) } {
            return 1;
        }
        let registration = if unsafe { services.activate_echo() }.is_ok() {
            unsafe { register(module) }
        } else {
            Err(crate::Error::Admission)
        };
        match registration {
            Ok(registration) => {
                *selected = Some(State {
                    product,
                    registration: Some(registration),
                    services,
                    source: Some(LegacySource {
                        configuration,
                        path,
                        control: EchoBackend(control),
                    }),
                });
                0
            }
            Err(_) => {
                if services.stop_echo().is_ok() && product.stop() && services.private_stopped() {
                    1
                } else {
                    // A live product cannot be abandoned while its callback code unloads.
                    *selected = Some(State {
                        product,
                        registration: None,
                        services,
                        source: Some(LegacySource {
                            configuration,
                            path,
                            control: EchoBackend(control),
                        }),
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
        let Ok(_transition) = TRANSITION.try_lock() else {
            return -1;
        };
        let mut selected = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = selected.as_mut() else {
            return -1;
        };
        let Some(source) = state.source.as_mut() else {
            let product = state.product;
            drop(selected);
            return if product.reload_working() { 0 } else { -1 };
        };
        let Ok(configuration) = std::fs::read_to_string(&source.path) else {
            return -1;
        };
        let result = with_echo_preparation(
            &source.path,
            state.product,
            source.control,
            |preparation| unsafe {
                state.services.reload_echo(
                    state.product.descriptor(),
                    &configuration,
                    preparation,
                    || state.product.reload(&configuration),
                    || state.product.reload(&source.configuration),
                )
            },
        );
        match result {
            Ok(outcome) => {
                if let Some(error) = outcome.retirement {
                    echo_reload_error(error);
                }
                source.configuration = configuration;
                0
            }
            Err(error) => {
                echo_reload_error(error);
                -1
            }
        }
    }))
    .unwrap_or(-1)
}

extern "C" fn unload() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(_transition) = TRANSITION.try_lock() else {
            return -1;
        };
        let mut selected = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let Some(state) = selected.as_ref() else {
            return 0;
        };
        if state.services.stop_echo().is_err() || !state.product.stop() || !state.services.private_stopped() {
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
