//! Retained per-peer executor capability, independent of codec and controller code.

use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
};

pub(crate) type Step = unsafe extern "C" fn(*mut c_void) -> i32;

type Descriptor = crate::abi::rptadv_iax2_async_descriptor_v1;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Descriptor,
    Initialize,
    Attach,
}

/// One shared non-audio executor. Its host detaches every peer before releasing it.
pub(crate) struct Executor {
    api: Descriptor,
    runtime: NonNull<c_void>,
}
// The capability explicitly permits concurrent registration/stop, never concurrent
// use of one peer. The runtime only schedules; it does not own radio PCM consumers.
unsafe impl Send for Executor {}
unsafe impl Sync for Executor {}

unsafe fn checked_descriptor(pointer: *const c_void) -> Result<Descriptor, Error> {
    if pointer.is_null() {
        return Err(Error::Descriptor);
    }
    let prefix = unsafe { ptr::read_unaligned(pointer.cast::<[u32; 2]>()) };
    if prefix[0] < size_of::<Descriptor>() as u32 || prefix[1] != 1 {
        return Err(Error::Descriptor);
    }
    let api = unsafe { ptr::read_unaligned(pointer.cast::<Descriptor>()) };
    if api.capability != *b"rptadv.iaxasync\0"
        || ![
            api.runtime_create.is_some(),
            api.runtime_destroy.is_some(),
            api.peer_attach.is_some(),
            api.peer_stop.is_some(),
            api.peer_detach.is_some(),
            api.peer_configure_jitter.is_some(),
            api.server_enable.is_some(),
            api.peer_observe.is_some(),
            api.peer_wake.is_some(),
            api.peer_set_deadline.is_some(),
        ]
        .into_iter()
        .all(|present| present)
    {
        return Err(Error::Descriptor);
    }
    Ok(api)
}
/// Validate reload capability without creating a second executor.
///
/// # Safety
/// The fixed descriptor prefix and stated full storage must be readable.
pub(crate) unsafe fn valid_descriptor(pointer: *const c_void) -> bool {
    unsafe { checked_descriptor(pointer) }.is_ok()
}
impl Executor {
    /// Validate the whole table before creating the runtime.
    ///
    /// # Safety
    /// The descriptor must be readable for its stated size and its DSO retained
    /// until this owner and all its registrations are destroyed.
    pub(crate) unsafe fn new(pointer: *const c_void) -> Result<Self, Error> {
        let api = unsafe { checked_descriptor(pointer) }?;
        let mut runtime = ptr::null_mut();
        if unsafe { api.runtime_create.unwrap()(&mut runtime) } != 0 {
            return Err(Error::Initialize);
        }
        Ok(Self {
            api,
            runtime: NonNull::new(runtime).ok_or(Error::Initialize)?,
        })
    }
    /// Signal only; never borrows the peer or executes a step on the caller.
    ///
    /// # Safety
    /// Token remains attached until this signal returns.
    pub(crate) unsafe fn wake(&self, token: *mut c_void) {
        unsafe { self.api.peer_wake.unwrap()(token) };
    }
    /// Set the current owner's next relative wake; MAX clears the deadline.
    ///
    /// # Safety
    /// Caller is the sole serialized owner of this live peer.
    pub(crate) unsafe fn set_deadline(&self, peer: *mut c_void, after: u64) -> i32 {
        unsafe { self.api.peer_set_deadline.unwrap()(peer, after) }
    }

    /// Transfer exclusive peer operations to a stable callback context until detach.
    ///
    /// # Safety
    /// Peer and context must remain live and unmoved until detach returns. The
    /// callback may begin before this function returns; it must not unwind.
    pub(crate) unsafe fn attach(
        &self,
        peer: *mut c_void,
        step: Step,
        context: *mut c_void,
    ) -> Result<*mut c_void, Error> {
        let mut token = ptr::null_mut();
        let status = unsafe {
            self.api.peer_attach.unwrap()(
                self.runtime.as_ptr(),
                peer,
                Some(step),
                context,
                &mut token,
            )
        };
        if status != 0 || token.is_null() {
            Err(Error::Attach)
        } else {
            Ok(token)
        }
    }

    /// Request stop without waiting; allowed inside the retained callback.
    ///
    /// # Safety
    /// Token belongs to this executor and has not completed detach.
    pub(crate) unsafe fn stop(&self, token: *mut c_void) {
        unsafe { self.api.peer_stop.unwrap()(token) };
    }

    /// Wait off-executor until no callback can touch the peer or context.
    ///
    /// # Safety
    /// Token is uniquely detached once, never from its own callback.
    pub(crate) unsafe fn detach(&self, token: *mut c_void) {
        unsafe { self.api.peer_detach.unwrap()(token) };
    }

    /// Install one codec-neutral jitter context before starting peer callbacks.
    ///
    /// # Safety
    /// Peer is uniquely owned and not attached. Jitter descriptor and DSO remain
    /// live until peer destruction, and satisfy their versioned descriptor contract.
    pub(crate) unsafe fn configure_jitter(
        &self,
        peer: *mut c_void,
        jitter: *const c_void,
    ) -> Result<(), Error> {
        if unsafe { self.api.peer_configure_jitter.unwrap()(peer, jitter) } == 0 {
            Ok(())
        } else {
            Err(Error::Initialize)
        }
    }

    /// Start asynchronous socket routing without moving admission into the receive task.
    ///
    /// # Safety
    /// Server belongs to this protocol DSO and is not being used concurrently.
    pub(crate) unsafe fn enable_server(&self, server: *mut c_void) -> Result<(), Error> {
        if unsafe { self.api.server_enable.unwrap()(self.runtime.as_ptr(), server) } == 0 {
            Ok(())
        } else {
            Err(Error::Initialize)
        }
    }

    /// Copy protocol diagnostics while executing the peer's sole serialized turn.
    ///
    /// # Safety
    /// Peer is exclusively owned by the caller. Output is a writable ABI1 peer
    /// observation record with its structure size and ABI version initialized.
    pub(crate) unsafe fn observe(&self, peer: *mut c_void, output: *mut c_void) -> i32 {
        unsafe { self.api.peer_observe.unwrap()(peer, output.cast()) }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        unsafe { self.api.runtime_destroy.unwrap()(self.runtime.as_ptr()) };
    }
}

#[cfg(test)]
#[path = "executor/tests.rs"]
mod tests;
