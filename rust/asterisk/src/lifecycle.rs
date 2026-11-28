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
}

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
            *const ffi::rptadv_product_management_v1,
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

pub(crate) fn asl_incoming(local: &str) -> Option<crate::services::Handoff> {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .services
        .asl_incoming(local)
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
unsafe extern "C" fn load(
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
        services.install_echo(product.descriptor(), control);
        if services.install_asl().is_err() {
            return 1;
        }
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
                Err(_)
                    if product.stop()
                        && services.private_stopped()
                        && services.echo_stopped()
                        && crate::registration::drain_resolver() =>
                {
                    return 1;
                }
                Err(_) => None,
            }
        } else {
            None
        };
        *selected = Some(State {
            product,
            services,
            registration,
        });
        0
    }))
    .unwrap_or(1)
}

extern "C" fn reload() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(_transition) = TRANSITION.try_lock() else {
            return -1;
        };
        let product = STATE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.product);
        if product.is_some_and(|p| p.reload_working()) {
            0
        } else {
            -1
        }
    }))
    .unwrap_or(-1)
}

fn unload_deferred(reason: &'static CStr) -> i32 {
    // SAFETY: format and reason are static and contain no provider or user data.
    unsafe {
        ffi::ast_log(
            ffi::__LOG_WARNING as i32,
            c"rust/asterisk/lifecycle.rs".as_ptr(),
            0,
            c"unload".as_ptr(),
            c"rpt_advanced: unload deferred: %s\n".as_ptr(),
            reason.as_ptr(),
        )
    };
    -1
}

extern "C" fn unload() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(_transition) = TRANSITION.try_lock() else {
            return unload_deferred(c"another lifecycle operation is in progress");
        };
        // TRANSITION pins the module while product stop drains callbacks that may look up STATE.
        let product = {
            let selected = STATE.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = selected.as_ref() else {
                return 0;
            };
            if !state.services.close_asl_ingress() {
                drop(selected);
                return unload_deferred(c"AllStarLink ingress is still draining");
            }
            state.product
        };
        // A healthy asynchronous withdrawal may need several Product stop polls.
        // Keep all owners registered until Product confirms callback quiescence.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !product.stop() {
            if std::time::Instant::now() >= deadline {
                return unload_deferred(c"controller did not quiesce before the unload deadline");
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut selected = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let state = selected.as_ref().expect("transition retains lifecycle");
        let deferred = if !state.services.echo_stopped() {
            Some(c"EchoLink owner did not confirm shutdown")
        } else if !state.services.private_stopped() {
            Some(c"phone owner did not confirm shutdown")
        } else if !crate::registration::drain_resolver() {
            Some(c"registration resolver is still draining")
        } else {
            None
        };
        if let Some(reason) = deferred {
            drop(selected);
            return unload_deferred(reason);
        }
        let state = selected.take().expect("selected lifecycle remains owned");
        drop(selected);
        drop(state.registration);
        0
    }))
    .unwrap_or_else(|_| unload_deferred(c"lifecycle callback failed"))
}

static DESCRIPTOR: Descriptor = Descriptor {
    struct_size: size_of::<Descriptor>() as u32,
    abi_version: 2,
    capability: *b"rptadv.asterisk\0",
    load: Some(load),
    reload: Some(reload),
    unload: Some(unload),
};

/// Return immutable process-lifetime Asterisk entry metadata.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_asterisk_descriptor_v2() -> *const Descriptor {
    &DESCRIPTOR
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
pub(crate) mod tests;
