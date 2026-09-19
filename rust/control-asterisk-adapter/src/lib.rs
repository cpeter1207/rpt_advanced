#![deny(warnings, missing_docs)]
//! Replaceable Asterisk taskprocessor execution. No scheduling or node policy lives here.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{Arc, Condvar, Mutex},
};

#[allow(warnings, missing_docs, clippy::all)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/asterisk.rs"));
}

struct Admission {
    accepting: bool,
    pending: usize,
}
struct State {
    admission: Mutex<Admission>,
    drained: Condvar,
    capacity: usize,
}
struct Accepted {
    task: OwnedTask,
    state: Arc<State>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmitError {
    Stopped,
    Full,
    Backend,
}

struct AsteriskExecutor {
    handle: *mut bindings::ast_taskprocessor,
    state: Arc<State>,
}
// SAFETY: Asterisk accepts concurrent push/is_task calls. Admission serializes push,
// and handle unreference occurs only after admission closes and accepted callbacks finish.
unsafe impl Send for AsteriskExecutor {}
unsafe impl Sync for AsteriskExecutor {}

impl AsteriskExecutor {
    fn open(name: &str, capacity: usize) -> Result<Self, ()> {
        if capacity == 0 || name.is_empty() {
            return Err(());
        }
        // One application executor owns the sole external reference. Put Asterisk's
        // process-wide sequence first so even a truncated display name remains unique.
        let sequence = unsafe { bindings::ast_taskprocessor_seq_num() };
        let name = CString::new(format!("rptadv-{sequence:08x}-{name}")).map_err(|_| ())?;
        let handle =
            unsafe { bindings::ast_taskprocessor_get(name.as_ptr(), bindings::TPS_REF_DEFAULT) };
        if handle.is_null() {
            return Err(());
        }
        Ok(Self {
            handle,
            state: Arc::new(State {
                admission: Mutex::new(Admission {
                    accepting: true,
                    pending: 0,
                }),
                drained: Condvar::new(),
                capacity,
            }),
        })
    }
}

impl AsteriskExecutor {
    fn submit(&self, task: Task) -> Result<(), (Task, SubmitError)> {
        let mut admission = self
            .state
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let reason = if !admission.accepting {
            Some(SubmitError::Stopped)
        } else if admission.pending >= self.state.capacity {
            Some(SubmitError::Full)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err((task, reason));
        }
        let accepted = Box::into_raw(Box::new(Accepted {
            task: OwnedTask(task),
            state: self.state.clone(),
        }));
        admission.pending += 1;
        // Push while holding admission preserves FIFO acceptance order across producers.
        let result = unsafe {
            bindings::__ast_taskprocessor_push(
                self.handle,
                Some(execute),
                accepted.cast(),
                c"rptadv-control".as_ptr(),
                0,
                c"submit".as_ptr(),
            )
        };
        if result == 0 {
            return Ok(());
        }
        admission.pending -= 1;
        // SAFETY: a failed Asterisk push never invokes or takes ownership of the callback.
        let rejected = unsafe { Box::from_raw(accepted) };
        let task = rejected.task.0;
        std::mem::forget(rejected.task);
        Err((task, SubmitError::Backend))
    }

    fn stop_and_drain(&self) -> bool {
        if unsafe { bindings::ast_taskprocessor_is_task(self.handle) } != 0 {
            return false;
        }
        let mut admission = self
            .state
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        admission.accepting = false;
        while admission.pending != 0 {
            admission = self
                .state
                .drained
                .wait(admission)
                .unwrap_or_else(|e| e.into_inner());
        }
        true
    }
}

impl Drop for AsteriskExecutor {
    fn drop(&mut self) {
        // The descriptor refuses destruction on this executor. Retain the external handle
        // if an internal caller violates that contract instead of freeing reachable code.
        if self.stop_and_drain() {
            // This uniquely named default executor must not be externally shared.
            // Its final unreference joins the worker after our callback fully returns;
            // the loader keeps this DSO loaded until this call (and close) completes.
            unsafe {
                bindings::ast_taskprocessor_unreference(self.handle);
            }
        }
    }
}

unsafe extern "C" fn execute(context: *mut c_void) -> c_int {
    // SAFETY: exactly one successful enqueue owns this allocation and calls us once.
    let accepted = unsafe { Box::from_raw(context.cast::<Accepted>()) };
    let Accepted { task, state } = *accepted;
    task.run();
    let mut admission = state.admission.lock().unwrap_or_else(|e| e.into_inner());
    admission.pending -= 1;
    if admission.pending == 0 {
        state.drained.notify_all();
    }
    0
}

/// Stable C-compatible task: success transfers context; rejection leaves it untouched.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Task {
    /// Owned opaque caller payload.
    pub context: *mut c_void,
    /// Invoke once on the serialized owner; must not unwind or destroy context.
    pub run: unsafe extern "C" fn(*mut c_void),
    /// Release once after invocation; must not unwind.
    pub release: unsafe extern "C" fn(*mut c_void),
}
// SAFETY: submission explicitly transfers exclusive opaque-payload ownership.
unsafe impl Send for Task {}

struct OwnedTask(Task);
impl OwnedTask {
    fn run(self) {
        unsafe { (self.0.run)(self.0.context) };
    }
}
impl Drop for OwnedTask {
    fn drop(&mut self) {
        unsafe { (self.0.release)(self.0.context) };
    }
}

/// Control capability ABI. All operations are control-only and require live handles.
#[repr(C)]
pub struct Descriptor {
    /// Exact incompatible-contract version.
    pub abi_version: u32,
    /// Complete descriptor byte size.
    pub struct_size: usize,
    /// Capability identity, NUL-terminated static text.
    pub capability: *const c_char,
    /// Open one uniquely named application FIFO; null means preparation failed.
    pub open: unsafe extern "C" fn(*const c_char, usize) -> *mut c_void,
    /// Enqueue: 0 accepted, 1 stopped, 2 full, 3 backend failure; rejected task untouched.
    pub submit: unsafe extern "C" fn(*mut c_void, Task) -> c_int,
    /// Gate and drain payloads: 0 complete, -1 invalid context or called from its own executor.
    /// This is not the code-unload barrier; the lifecycle owner must also close.
    pub stop_and_drain: unsafe extern "C" fn(*mut c_void) -> c_int,
    /// Drain/release/join: 0 consumed handle; -1 retains it for external-owner retry.
    /// The loader retains provider code through successful close. No external taskprocessor
    /// reference may share this executor, so final unreference joins its default worker.
    pub close: unsafe extern "C" fn(*mut c_void) -> c_int,
}
// SAFETY: immutable descriptor points only to static text and executable functions.
unsafe impl Sync for Descriptor {}

unsafe extern "C" fn open_abi(name: *const c_char, capacity: usize) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if name.is_null() {
            return ptr::null_mut();
        }
        let Ok(name) = (unsafe { CStr::from_ptr(name) }).to_str() else {
            return ptr::null_mut();
        };
        AsteriskExecutor::open(name, capacity).map_or(ptr::null_mut(), |executor| {
            Box::into_raw(Box::new(executor)).cast()
        })
    }))
    .unwrap_or(ptr::null_mut())
}

unsafe extern "C" fn submit_abi(context: *mut c_void, task: Task) -> c_int {
    if context.is_null() {
        return 3;
    }
    match unsafe { &*context.cast::<AsteriskExecutor>() }.submit(task) {
        Ok(()) => 0,
        Err((_, SubmitError::Stopped)) => 1,
        Err((_, SubmitError::Full)) => 2,
        Err((_, SubmitError::Backend)) => 3,
    }
}

unsafe extern "C" fn drain_abi(context: *mut c_void) -> c_int {
    if context.is_null() {
        return -1;
    }
    if unsafe { &*context.cast::<AsteriskExecutor>() }.stop_and_drain() {
        0
    } else {
        -1
    }
}

unsafe extern "C" fn close_abi(context: *mut c_void) -> c_int {
    if unsafe { drain_abi(context) } != 0 {
        return -1;
    }
    unsafe {
        drop(Box::from_raw(context.cast::<AsteriskExecutor>()));
    }
    0
}

static DESCRIPTOR: Descriptor = Descriptor {
    abi_version: 1,
    struct_size: size_of::<Descriptor>(),
    capability: c"rptadv.control".as_ptr(),
    open: open_abi,
    submit: submit_abi,
    stop_and_drain: drain_abi,
    close: close_abi,
};

/// Return this adapter's immutable versioned control-execution function table.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_control_descriptor_v1() -> *const Descriptor {
    &DESCRIPTOR
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
