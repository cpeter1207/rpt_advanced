//! Validated client for the portable product runtime descriptor.

use crate::bindings as ffi;
use std::{ffi::c_void, mem::size_of, ptr};

/// Selected product descriptor retained by the composition loader.
#[derive(Clone, Copy)]
pub struct Product(&'static ffi::rptadv_product_descriptor_v1);
// SAFETY: the table is immutable for process lifetime and product operations serialize internally.
unsafe impl Send for Product {}
// SAFETY: every operation is defined as concurrently callable by the product ABI.
unsafe impl Sync for Product {}

impl Product {
    /// Borrow the validated table while the composition loader retains its DSO.
    pub(crate) fn descriptor(&self) -> &'static ffi::rptadv_product_descriptor_v1 {
        self.0
    }

    /// Validate an immutable complete product table.
    ///
    /// # Safety
    /// `pointer` provides a readable size/version prefix and remains loaded through unload.
    pub unsafe fn open(pointer: *const ffi::rptadv_product_descriptor_v1) -> Option<Self> {
        if pointer.is_null()
            || !pointer.is_aligned()
            || unsafe { ptr::addr_of!((*pointer).struct_size).read() }
                != size_of::<ffi::rptadv_product_descriptor_v1>() as u32
            || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != 8
        {
            return None;
        }
        let descriptor = unsafe { &*pointer };
        (descriptor.capability == *b"rptadv.prod8\0\0\0\0"
            && descriptor.reload_working.is_some()
            && descriptor.reload_poll.is_some()
            && descriptor.stop.is_some()
            && descriptor.authorize_incoming.is_some()
            && descriptor.incoming.is_some()
            && descriptor.link_command.is_some()
            && descriptor.site_command.is_some()
            && descriptor.link_status.is_some()
            && descriptor.digit.is_some())
        .then_some(Self(descriptor))
    }
    /// Request and wait for the checked working-store reload on the product owner.
    pub(crate) fn reload_working(&self) -> bool {
        // SAFETY: the loader retains the product through the final reload receipt.
        let mut code = unsafe { self.0.reload_working.expect("checked reload group")() };
        while code == 1 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            // SAFETY: the same retained table owns the pending receipt.
            code = unsafe { self.0.reload_poll.expect("checked reload group")() };
        }
        code == 0
    }
    /// Stop every product owner and drain accepted calls.
    pub fn stop(&self) -> bool {
        unsafe { self.0.stop.unwrap()() == 0 }
    }
    /// Apply one stable link action number.
    pub fn link_command(&self, local: &str, remote: &str, action: u32) -> bool {
        unsafe {
            self.0.link_command.unwrap()(
                local.as_ptr().cast(),
                local.len(),
                remote.as_ptr().cast(),
                remote.len(),
                action,
            ) == 0
        }
    }
    /// Apply an authenticated site operation through shared policy. Zero accepts
    /// the request; it does not claim that hardware has serviced it.
    pub fn site_command(&self, local: &str, operation: u32, target: &str, value: f64) -> i32 {
        let request = ffi::rptadv_site_command_v1 {
            struct_size: size_of::<ffi::rptadv_site_command_v1>() as u32,
            abi_version: ffi::RPTADV_SITE_COMMAND_ABI_VERSION,
            operation,
            target: target.as_ptr().cast(),
            target_length: target.len(),
            value,
        };
        unsafe { self.0.site_command.unwrap()(local.as_ptr().cast(), local.len(), &request) }
    }
    /// Emit link status through one synchronous borrowed sink.
    /// # Safety
    /// `sink` and `context` remain live for the synchronous call and never unwind.
    pub unsafe fn link_status(
        &self,
        local: &str,
        sink: unsafe extern "C" fn(*mut c_void, *const std::ffi::c_char, usize),
        context: *mut c_void,
    ) -> bool {
        unsafe {
            self.0.link_status.unwrap()(local.as_ptr().cast(), local.len(), Some(sink), context)
                == 0
        }
    }
    /// Feed one digit and report command completion.
    pub fn digit(&self, local: &str, digit: char) -> Result<bool, crate::Error> {
        let mut completed = 0;
        let code = unsafe {
            self.0.digit.unwrap()(
                local.as_ptr().cast(),
                local.len(),
                digit as u8,
                &mut completed,
            )
        };
        (code == 0)
            .then_some(completed != 0)
            .ok_or(crate::Error::Admission)
    }
}
