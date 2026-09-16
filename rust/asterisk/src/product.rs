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
    /// Validate an immutable complete product table.
    ///
    /// # Safety
    /// `pointer` provides a readable size/version prefix and remains loaded through unload.
    pub unsafe fn open(pointer: *const ffi::rptadv_product_descriptor_v1) -> Option<Self> {
        if pointer.is_null()
            || unsafe { ptr::addr_of!((*pointer).struct_size).read() }
                != size_of::<ffi::rptadv_product_descriptor_v1>() as u32
            || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != 2
        {
            return None;
        }
        let descriptor = unsafe { &*pointer };
        (descriptor.capability == *b"rptadv.prod2\0\0\0\0"
            && descriptor.start.is_some()
            && descriptor.reload.is_some()
            && descriptor.stop.is_some()
            && descriptor.authorize_incoming.is_some()
            && descriptor.incoming.is_some()
            && descriptor.link_command.is_some()
            && descriptor.link_status.is_some()
            && descriptor.digit.is_some())
        .then_some(Self(descriptor))
    }
    /// Start the portable owner with validated provider tables and copied configuration.
    ///
    /// # Safety
    /// Every descriptor points to a complete compatible immutable table whose code
    /// remains loaded until a successful product stop.
    pub unsafe fn start(
        &self,
        host: *const ffi::rptadv_host_services_v2,
        control: *const ffi::rptadv_control_descriptor_v1,
        file: *const ffi::rptadv_file_descriptor,
        speech: *const ffi::rptadv_speech_descriptor,
        configuration: &str,
    ) -> bool {
        unsafe {
            self.0.start.unwrap()(
                host,
                control,
                file,
                speech,
                configuration.as_ptr().cast(),
                configuration.len(),
            ) == 0
        }
    }
    /// Replace configuration without discarding a working generation on failure.
    pub fn reload(&self, configuration: &str) -> bool {
        unsafe { self.0.reload.unwrap()(configuration.as_ptr().cast(), configuration.len()) == 0 }
    }
    /// Stop every product owner and drain accepted calls.
    pub fn stop(&self) -> bool {
        unsafe { self.0.stop.unwrap()() == 0 }
    }
    /// Check current policy and topology before answering an incoming channel.
    pub fn authorize_incoming(&self, local: &str, remote: &str, source: &str) -> bool {
        unsafe {
            self.0.authorize_incoming.unwrap()(
                local.as_ptr().cast(),
                local.len(),
                remote.as_ptr().cast(),
                remote.len(),
                source.as_ptr().cast(),
                source.len(),
            ) == 0
        }
    }
    /// Transfer one answered peer to current product admission.
    /// # Safety
    /// `peer` is a unique live handle from the selected host table. Return zero or
    /// minus one transfers it; return one leaves it caller-owned.
    pub unsafe fn incoming(
        &self,
        local: &str,
        remote: &str,
        source: &str,
        peer: *mut c_void,
    ) -> i32 {
        unsafe {
            self.0.incoming.unwrap()(
                local.as_ptr().cast(),
                local.len(),
                remote.as_ptr().cast(),
                remote.len(),
                source.as_ptr().cast(),
                source.len(),
                peer,
            )
        }
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
