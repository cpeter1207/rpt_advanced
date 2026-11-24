//! Retained per-peer executor capability, independent of codec and controller code.

use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
};

pub(crate) type Step = unsafe extern "C" fn(*mut c_void) -> i32;

type ConfigurePrivate = unsafe extern "C" fn(*mut c_void, *const c_void, u32, u32, u32) -> i32;
type SetPrivate = unsafe extern "C" fn(*mut c_void, u32, u32, u32) -> i32;
const RELEASED_PREFIX:usize=std::mem::offset_of!(Descriptor,peer_set_deadline)+size_of::<*const c_void>();
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
    configure_private: Option<ConfigurePrivate>,
    set_private: Option<SetPrivate>,
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
    if prefix[0] < RELEASED_PREFIX as u32 || prefix[1] != 1 {
        return Err(Error::Descriptor);
    }
    let mut storage=std::mem::MaybeUninit::<Descriptor>::zeroed();
    unsafe{ptr::copy_nonoverlapping(pointer.cast::<u8>(),storage.as_mut_ptr().cast(),RELEASED_PREFIX)};
    let api=unsafe{storage.assume_init()};
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
impl Executor {
    /// Validate the whole table before creating the runtime.
    ///
    /// # Safety
    /// The descriptor must be readable for its stated size and its DSO retained
    /// until this owner and all its registrations are destroyed.
    pub(crate) unsafe fn new(pointer: *const c_void) -> Result<Self, Error> {
        let api = unsafe { checked_descriptor(pointer) }?;
        let extent=unsafe{pointer.cast::<u32>().read_unaligned()} as usize;
        // This is an append-only optional callback beyond the released alpha14 table.
        let configure_private = if extent >= RELEASED_PREFIX + size_of::<Option<ConfigurePrivate>>() {
            unsafe{pointer.cast::<u8>().add(RELEASED_PREFIX).cast::<Option<ConfigurePrivate>>().read_unaligned()}
        } else {None};
        let live_offset = RELEASED_PREFIX + size_of::<Option<ConfigurePrivate>>();
        let set_private = if extent >= live_offset + size_of::<Option<SetPrivate>>() {
            unsafe { pointer.cast::<u8>().add(live_offset).cast::<Option<SetPrivate>>().read_unaligned() }
        } else { None };
        let mut runtime = ptr::null_mut();
        if unsafe { api.runtime_create.unwrap()(&mut runtime) } != 0 {
            return Err(Error::Initialize);
        }
        Ok(Self {
            api, configure_private, set_private,
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

    pub(crate) fn supports_live_policy(&self) -> bool { self.set_private.is_some() }
    /// Change the existing context while holding the peer exclusively or on its callback turn.
    pub(crate) unsafe fn set_delays(&self, peer: *mut c_void, [min, initial, max]: [u32; 3]) -> Result<(), Error> {
        let set = self.set_private.ok_or(Error::Descriptor)?;
        if unsafe { set(peer, min, initial, max) } == 0 { Ok(()) } else { Err(Error::Initialize) }
    }

    pub(crate) fn supports_private(&self,delays:[u32;3])->bool{
        delays==[20,40,100] || self.configure_private.is_some()
    }
    /// Configure the actual private call before the first audio operation or attachment.
    pub(crate) unsafe fn configure_private(&self,peer:*mut c_void,jitter:*const c_void,delays:[u32;3])->Result<(),Error>{
        if delays==[20,40,100]{return unsafe{self.configure_jitter(peer,jitter)};}
        let [min,initial,max]=delays;
        let configure=self.configure_private.ok_or(Error::Descriptor)?;
        if unsafe{configure(peer,jitter,min,initial,max)}==0 {Ok(())} else {Err(Error::Initialize)}
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
    #[cfg(test)]
    pub(crate) unsafe fn observe(&self, peer: *mut c_void, output: *mut c_void) -> i32 {
        unsafe { self.api.peer_observe.unwrap()(peer, output.cast()) }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        unsafe { self.api.runtime_destroy.unwrap()(self.runtime.as_ptr()) };
    }
}
