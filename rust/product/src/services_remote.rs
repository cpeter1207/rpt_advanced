//! Nonblocking radio-control tickets. Physical policy remains in the shared SDK.

use super::{Radio, abi};
use rpt_advanced_core::remote_base_command::RemoteOperation;
use std::{ffi::c_void, mem::size_of, ptr};

/// A unique asynchronous operation, retaining only provider-owned copied state.
/// The control executor publishes source lifetime and copies a bounded result.
pub struct RemoteTicket(abi::rptadv_remote_ticket_v1);

// SAFETY: these SDK operations retain copied atomic/queue state, never a rig,
// channel or creator-thread-affine context. The selected control executor moves
// this unique ticket between tasks and never invokes it concurrently.
unsafe impl Send for RemoteTicket {}

impl RemoteTicket {
    /// Publish current key state and terminal source validity without waiting.
    pub fn source_state(&mut self, active: bool, valid: bool) -> Result<(), i32> {
        // SAFETY: admission retained a complete table and this unique live ticket.
        let code = unsafe {
            self.0.source.unwrap()(self.0.context, u32::from(active) | (u32::from(valid) << 1))
        };
        (code == 0).then_some(()).ok_or(code)
    }

    /// Copy at most 64 KiB of machine-readable result; one means still pending.
    pub fn take(&mut self) -> Result<Option<String>, i32> {
        unsafe extern "C" fn append(context: *mut c_void, text: *const u8, length: u32) {
            // SAFETY: take retains this exclusive result throughout the synchronous callback.
            let out = unsafe { &mut *context.cast::<Result<String, i32>>() };
            if text.is_null() && length != 0 || length > 65_536 {
                *out = Err(-1);
                return;
            }
            let bytes = if length == 0 {
                &[]
            } else {
                // SAFETY: the provider lends a readable complete span until return.
                unsafe { std::slice::from_raw_parts(text, length as usize) }
            };
            if let Ok(text) = std::str::from_utf8(bytes) {
                if let Ok(value) = out {
                    if value.len().saturating_add(text.len()) <= 65_536 {
                        value.push_str(text);
                        return;
                    }
                }
            }
            *out = Err(-1);
        }
        let mut output = Ok(String::new());
        // SAFETY: the provider only borrows the stack result during this copy.
        let code = unsafe {
            self.0.take.unwrap()(
                self.0.context,
                Some(append),
                ptr::from_mut(&mut output).cast(),
            )
        };
        match code {
            0 => output.map(Some),
            1 => Ok(None),
            _ => Err(code),
        }
    }
}

impl Drop for RemoteTicket {
    fn drop(&mut self) {
        let _ = self.source_state(false, false);
        // SAFETY: this is the unique ticket returned by a complete provider.
        unsafe { self.0.destroy.unwrap()(self.0.context) };
    }
}

impl Radio {
    /// Advance the shared DSP control mailbox without waiting for CAT or GPIO.
    pub fn service(&mut self) -> Result<(), i32> {
        let Some(call) = self.services.0.radio_service else {
            return Ok(());
        };
        // SAFETY: the host serializes this reserved radio on control.
        let code = unsafe { call(self.services.0.context, self.handle.as_ptr()) };
        (code == 0).then_some(()).ok_or(code)
    }
    /// Poll safe retirement while retaining this radio and all callback contexts.
    pub fn poll_quiesce(&mut self) -> Result<bool, i32> {
        let Some(call) = self.services.0.radio_quiesce else {
            return Ok(true);
        };
        // SAFETY: same serialized radio owner, retained until successful destroy.
        match unsafe { call(self.services.0.context, self.handle.as_ptr()) } {
            0 => Ok(true),
            1 => Ok(false),
            code => Err(code),
        }
    }
    /// Submit copied intent; no CAT exchange or DSP preparation runs here.
    pub fn remote_submit(
        &mut self,
        request: &RemoteOperation,
        local: bool,
        active: bool,
    ) -> Result<RemoteTicket, i32> {
        let call = self.services.0.radio_remote_submit.ok_or(-7)?;
        let operation = abi::UrpRemoteOperation {
            struct_size: request.struct_size,
            abi_version: request.abi_version,
            operation: request.operation,
            selector: request.selector,
            signed_value: request.signed_value,
            magnitude: request.magnitude,
            decimal_places: request.decimal_places,
            flags: request.flags,
        };
        // SAFETY: the result is a flat record of nullable pointers and scalars.
        let mut ticket: abi::rptadv_remote_ticket_v1 = unsafe { std::mem::zeroed() };
        // SAFETY: inputs/results are complete borrowed stack values; success transfers a ticket.
        let code = unsafe {
            call(
                self.services.0.context,
                self.handle.as_ptr(),
                &operation,
                u32::from(local) | (u32::from(active) << 1),
                &mut ticket,
            )
        };
        let valid = ticket.struct_size as usize == size_of::<abi::rptadv_remote_ticket_v1>()
            && ticket.abi_version == 1
            && !ticket.context.is_null()
            && ticket.source.is_some()
            && ticket.take.is_some()
            && ticket.destroy.is_some();
        if code != 0 || !valid {
            if !ticket.context.is_null() {
                if let Some(destroy) = ticket.destroy {
                    // SAFETY: a partial transferred ticket still retains its declared destructor.
                    unsafe { destroy(ticket.context) };
                }
            }
            return Err(if code != 0 { code } else { -7 });
        }
        Ok(RemoteTicket(ticket))
    }
}
