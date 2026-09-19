//! Validated client for the replaceable Asterisk control-execution provider.

use rpt_advanced_core::control::{
    ControlExecutor, ControlTask, DrainError, RejectedTask, RejectionReason,
};
use std::{
    ffi::{CStr, CString, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

/// ABI version 1 control-execution capability.
pub type Descriptor = crate::abi::rptadv_control_descriptor_v1;

/// Selected control capability could not be prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// Capability identity, ABI version, or descriptor size did not match.
    Incompatible,
    /// Taskprocessor name is empty or contains an embedded NUL byte.
    InvalidName,
    /// The selected backend could not open the executor.
    Unavailable,
}

/// Neutral core executor backed by one validated descriptor.
pub struct ControlClient {
    descriptor: &'static Descriptor,
    context: *mut c_void,
}

// SAFETY: the descriptor contract permits concurrent FIFO submission; close is
// performed only by the lifecycle owner after all clients and tasks quiesce.
unsafe impl Send for ControlClient {}
unsafe impl Sync for ControlClient {}

impl ControlClient {
    /// Validate and open the mandatory selected capability.
    ///
    /// # Safety
    /// A non-null `descriptor` provides a readable ABI version/size prefix and truthfully
    /// backs its claimed size. Its immutable storage, capability text and callback code
    /// must remain loaded for this client's lifetime. Lifecycle teardown must occur off
    /// the executor, after all producers relinquish this client.
    pub unsafe fn open(
        descriptor: *const Descriptor,
        name: &str,
        capacity: usize,
    ) -> Result<Self, ClientError> {
        if descriptor.is_null() {
            return Err(ClientError::Incompatible);
        }
        // SAFETY: only the readable prefix is inspected until the full size is validated.
        if unsafe { ptr::addr_of!((*descriptor).abi_version).read() } != 1
            || unsafe { ptr::addr_of!((*descriptor).struct_size).read() } != size_of::<Descriptor>()
        {
            return Err(ClientError::Incompatible);
        }
        // SAFETY: the claimed complete allocation has been validated; nullable callbacks
        // remain representable so a malformed foreign table can be rejected safely.
        let descriptor = unsafe { &*descriptor };
        if descriptor.capability.is_null()
            || unsafe { CStr::from_ptr(descriptor.capability) } != c"rptadv.control"
            || descriptor.open.is_none()
            || descriptor.submit.is_none()
            || descriptor.stop_and_drain.is_none()
            || descriptor.close.is_none()
        {
            return Err(ClientError::Incompatible);
        }
        if name.is_empty() {
            return Err(ClientError::InvalidName);
        }
        let name = CString::new(name).map_err(|_| ClientError::InvalidName)?;
        let context = unsafe { descriptor.open.unwrap()(name.as_ptr(), capacity) };
        if context.is_null() {
            return Err(ClientError::Unavailable);
        }
        Ok(Self {
            descriptor,
            context,
        })
    }
}

unsafe extern "C" fn run_task(context: *mut c_void) {
    // SAFETY: one accepted provider callback owns this allocation exactly once.
    let task = unsafe { ptr::read(context.cast::<ControlTask>()) };
    let _ = catch_unwind(AssertUnwindSafe(|| task.run()));
}

unsafe extern "C" fn release_task(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: run_task consumed the value; only its allocation remains.
        unsafe {
            drop(Box::from_raw(
                context.cast::<std::mem::MaybeUninit<ControlTask>>(),
            ));
        }
    }));
}

impl ControlExecutor for ControlClient {
    fn submit(&self, task: ControlTask) -> Result<(), RejectedTask> {
        let context = Box::into_raw(Box::new(task));
        let result = unsafe {
            self.descriptor.submit.unwrap()(
                self.context,
                crate::abi::rptadv_control_task_v1 {
                    context: context.cast(),
                    run: Some(run_task),
                    release: Some(release_task),
                },
            )
        };
        if result == 0 {
            return Ok(());
        }
        // SAFETY: rejection leaves the opaque payload untouched and caller-owned.
        let task = *unsafe { Box::from_raw(context) };
        let reason = match result {
            1 => RejectionReason::Stopped,
            2 => RejectionReason::Full,
            _ => RejectionReason::Backend,
        };
        Err(RejectedTask { task, reason })
    }

    fn stop_and_drain(&self) -> Result<(), DrainError> {
        if unsafe { self.descriptor.stop_and_drain.unwrap()(self.context) } == 0 {
            Ok(())
        } else {
            Err(DrainError::OnExecutor)
        }
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        // A self-drop violation deliberately leaks the provider context instead of
        // freeing taskprocessor state still reachable by an active callback.
        unsafe {
            self.descriptor.close.unwrap()(self.context);
        }
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
