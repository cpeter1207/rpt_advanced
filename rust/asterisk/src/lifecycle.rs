//! Asterisk registration and configuration translation around the portable product owner.

use crate::{bindings as ffi, module::Registration, product::Product};
use std::{
    ffi::{CStr, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::Mutex,
};

struct State {
    product: Product,
    registration: Option<Registration>,
}
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

fn configuration() -> Option<String> {
    let directory = unsafe { ffi::ast_config_AST_CONFIG_DIR };
    if directory.is_null() {
        return None;
    }
    let directory = unsafe { CStr::from_ptr(directory) }.to_str().ok()?;
    std::fs::read_to_string(Path::new(directory).join("rpt_advanced.conf")).ok()
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
        if !unsafe {
            product.start(
                crate::services::descriptor(),
                control,
                file,
                speech,
                &configuration,
            )
        } {
            return 1;
        }
        let registration = unsafe {
            Registration::register(
                module.cast(),
                crate::application::callback,
                [
                    crate::cli::link_callback,
                    crate::cli::alias_callback,
                    crate::cli::command_callback,
                ],
            )
        };
        match registration {
            Ok(registration) => {
                *selected = Some(State {
                    product,
                    registration: Some(registration),
                });
                0
            }
            Err(_) => {
                if product.stop() {
                    1
                } else {
                    // A live product cannot be abandoned while its callback code unloads.
                    *selected = Some(State {
                        product,
                        registration: None,
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
        let Some(configuration) = configuration() else {
            return -1;
        };
        if product().is_some_and(|product| product.reload(&configuration)) {
            0
        } else {
            -1
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
        if !state.product.stop() {
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
