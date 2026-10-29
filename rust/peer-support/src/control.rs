//! Shared off-audio client for the selected dynamic control-execution provider.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

/// Existing control ABI task. Success transfers context; run then release occur
/// exactly once, potentially before submit returns. Rejection touches neither.
#[repr(C)]
pub struct Task {
    /// Caller payload; never an audio callback context.
    pub context: *mut c_void,
    /// Execute on the selected serialized control owner.
    pub run: Option<unsafe extern "C" fn(*mut c_void)>,
    /// Release the consumed allocation after run.
    pub release: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// Mirror of the existing `rptadv_control_adapter.h`, not a new ABI. Host/product
/// generated records verify size/alignment/offsets against this leaf client.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    /// Exact incompatible version, currently one.
    pub abi_version: u32,
    /// Complete readable record size.
    pub struct_size: usize,
    /// Immutable NUL-terminated capability name.
    pub capability: *const c_char,
    /// Create one bounded executor on the selected backend.
    pub open: Option<unsafe extern "C" fn(*const c_char, usize) -> *mut c_void>,
    /// Nonblocking admission; zero transfers the task, other statuses retain it.
    pub submit: Option<unsafe extern "C" fn(*mut c_void, Task) -> c_int>,
    /// Gate and wait off executor; negative rejects unsafe self-drain.
    pub stop_and_drain: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    /// Final provider/context barrier, off executor only.
    pub close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}
// Immutable descriptor/capability storage is part of the provider contract.
unsafe impl Sync for Descriptor {}

/// Nonblocking admission outcome; the rejected payload remains caller-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionReason {
    /// Executor admission was stopped.
    Stopped,
    /// Configured pending-work credit is exhausted.
    Full,
    /// Backend refused the request.
    Backend,
}
/// Original untouched payload and the explicit provider rejection.
pub struct Rejected<T> {
    /// Safe to retry or retire, never executed on rejection.
    pub payload: T,
    /// Why admission failed.
    pub reason: RejectionReason,
}
/// A control owner cannot wait for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainError {
    /// Retry on the external lifecycle owner.
    OnExecutor,
}

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
pub struct Client {
    descriptor: Descriptor,
    context: *mut c_void,
}

// SAFETY: the descriptor contract permits concurrent FIFO submission; close is
// performed only by the lifecycle owner after all clients and tasks quiesce.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

impl Client {
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
            descriptor: *descriptor,
            context,
        })
    }
}

struct Payload<T> {
    value: T,
    run: fn(T),
}

unsafe extern "C" fn run_task<T>(context: *mut c_void) {
    // SAFETY: one accepted provider callback owns this allocation exactly once.
    let task = unsafe { ptr::read(context.cast::<Payload<T>>()) };
    let _ = catch_unwind(AssertUnwindSafe(|| (task.run)(task.value)));
}

unsafe extern "C" fn release_task<T>(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: run_task consumed the value; only its allocation remains.
        unsafe {
            drop(Box::from_raw(
                context.cast::<std::mem::MaybeUninit<Payload<T>>>(),
            ));
        }
    }));
}

impl Client {
    /// Transfer one owned non-audio operation. Ordinary rejection returns the
    /// untouched value; accepted work may already have run/released on return.
    pub fn submit<T: Send + 'static>(&self, payload: T, run: fn(T)) -> Result<(), Rejected<T>> {
        let context = Box::into_raw(Box::new(Payload {
            value: payload,
            run,
        }));
        let result = unsafe {
            self.descriptor.submit.unwrap()(
                self.context,
                Task {
                    context: context.cast(),
                    run: Some(run_task::<T>),
                    release: Some(release_task::<T>),
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
        Err(Rejected {
            payload: task.value,
            reason,
        })
    }

    /// Gate admission and wait for accepted work off the executor/audio path.
    pub fn stop_and_drain(&self) -> Result<(), DrainError> {
        if unsafe { self.descriptor.stop_and_drain.unwrap()(self.context) } == 0 {
            Ok(())
        } else {
            Err(DrainError::OnExecutor)
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // A self-drop violation deliberately leaks the provider context instead of
        // freeing taskprocessor state still reachable by an active callback.
        unsafe {
            self.descriptor.close.unwrap()(self.context);
        }
    }
}
