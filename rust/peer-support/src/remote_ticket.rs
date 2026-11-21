//! Forward independent remote operations while retaining the provider of their callback code.
use std::ffi::c_void;

/// Synchronous UTF-8 result visitor in the released remote-ticket ABI.
pub type Sink = Option<unsafe extern "C" fn(*mut c_void, *const u8, u32)>;

/// An SDK ticket and its matched, complete callback group.
pub struct Ticket {
    /// SDK-owned independent ticket.
    pub context: *mut c_void,
    /// Publish source flags to this ticket.
    pub source: unsafe extern "C" fn(*mut c_void, u32) -> i32,
    /// Poll the ticket and synchronously deliver a result to the optional visitor.
    pub take: unsafe extern "C" fn(*mut c_void, Sink, *mut c_void) -> i32,
    /// Cancel/destroy the SDK ticket exactly once.
    pub destroy: unsafe extern "C" fn(*mut c_void),
}

struct Retained<O> {
    ticket: Ticket,
    _owner: O,
}

impl Ticket {
    /// Return host forwarding operations which retain the provider until SDK destruction returns.
    pub fn retaining<O: 'static>(self, owner: O) -> Self {
        let retained = Box::new(Retained {
            ticket: self,
            _owner: owner,
        });
        Self {
            context: Box::into_raw(retained).cast(),
            source: source::<O>,
            take: take::<O>,
            destroy: destroy::<O>,
        }
    }
}

unsafe extern "C" fn source<O>(context: *mut c_void, flags: u32) -> i32 {
    // SAFETY: the client borrows the exact live forwarding ticket and its provider handle.
    unsafe {
        let ticket = &(*context.cast::<Retained<O>>()).ticket;
        (ticket.source)(ticket.context, flags)
    }
}

unsafe extern "C" fn take<O>(context: *mut c_void, sink: Sink, sink_context: *mut c_void) -> i32 {
    // SAFETY: the unique ticket stays live, and the visitor borrows its context synchronously.
    unsafe {
        let ticket = &(*context.cast::<Retained<O>>()).ticket;
        (ticket.take)(ticket.context, sink, sink_context)
    }
}

unsafe extern "C" fn destroy<O>(context: *mut c_void) {
    // SAFETY: the caller returns its unique forwarding ticket exactly once. The owning box
    // retains the provider until the matching SDK cancellation/destructor has completed.
    unsafe {
        let retained = Box::from_raw(context.cast::<Retained<O>>());
        (retained.ticket.destroy)(retained.ticket.context);
    }
}
