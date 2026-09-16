//! Public Asterisk registration storage and lifecycle ownership.
use crate::bindings as ffi;
use std::ffi::{c_char, c_int};

/// Public-header application callback, retained until unregister completes.
pub type ApplicationCallback = unsafe extern "C" fn(*mut ffi::ast_channel, *const c_char) -> c_int;
/// Public-header CLI callback, retained until unregister completes.
pub type CliCallback =
    unsafe extern "C" fn(*mut ffi::ast_cli_entry, c_int, *mut ffi::ast_cli_args) -> *mut c_char;

/// Pinned CLI metadata and its registered application. Drop only after callback admission
/// closes and accepted calls have drained; Asterisk never sees relocated entry storage.
pub struct Registration {
    entries: Box<[ffi::ast_cli_entry; 3]>,
}
// SAFETY: ownership moves only on lifecycle/control; the entries stay boxed and are
// never accessed by Rust while Asterisk invokes handlers against their fixed addresses.
unsafe impl Send for Registration {}

impl Registration {
    /// Register the application and every CLI spelling with the loader's actual module identity.
    /// A partial CLI registration is unregistered before its storage can be released.
    ///
    /// # Safety
    /// The module and non-unwinding callbacks must remain loaded until this owner is dropped;
    /// its drop must occur only after closing and draining all callback admission.
    pub unsafe fn register(
        module: *mut ffi::ast_module,
        application: ApplicationCallback,
        handlers: [CliCallback; 3],
    ) -> Result<Self, crate::Error> {
        if module.is_null() {
            return Err(crate::Error::Registration);
        }
        let mut entries: Box<[ffi::ast_cli_entry; 3]> = Box::new(unsafe { std::mem::zeroed() });
        for (index, handler) in handlers.into_iter().enumerate() {
            entries[index].handler = Some(handler);
            entries[index].summary = if index == 2 {
                c"Inject an rpt_advanced DTMF command".as_ptr()
            } else {
                c"Control rpt_advanced links".as_ptr()
            };
        }
        // SAFETY: strings are static; module/callback lifetime is the caller's contract.
        if unsafe {
            ffi::ast_register_application2(
                c"RptAdvanced".as_ptr(),
                Some(application),
                c"Connect an AllStarLink peer".as_ptr(),
                c"RptAdvanced(node): connect an authenticated IAX peer to a configured node."
                    .as_ptr(),
                module.cast(),
            )
        } != 0
        {
            return Err(crate::Error::Registration);
        }
        let registration = Self { entries };
        // Drop also unregisters all CLI entries after a partial registration failure.
        if unsafe {
            ffi::__ast_cli_register_multiple(registration.entries.as_ptr().cast_mut(), 3, module)
        } != 0
        {
            return Err(crate::Error::Registration);
        }
        Ok(registration)
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: callbacks have drained; the pinned entries stay alive through unregistration.
        unsafe {
            ffi::ast_cli_unregister_multiple(self.entries.as_mut_ptr(), 3);
            ffi::ast_unregister_application(c"RptAdvanced".as_ptr());
        }
    }
}

#[cfg(test)]
#[path = "module_tests.rs"]
pub(crate) mod tests;
