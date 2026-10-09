#![deny(warnings, missing_docs)]
//! Lock-free standalone implementation of the versioned control executor.

use crossbeam_queue::ArrayQueue;
use std::{
    cell::Cell,
    ffi::{CStr, c_char, c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle, Thread},
    time::Duration,
};

const STOPPED: usize = 1usize << (usize::BITS - 1);
const PENDING: usize = STOPPED - 1;
const MAX_CAPACITY: usize = 65_536;

const fn valid_capacity(capacity: usize) -> bool {
    capacity > 0 && capacity <= MAX_CAPACITY
}

thread_local! {
    static CONTROL_OWNER: Cell<*const Shared> = const { Cell::new(ptr::null()) };
}

/// One owned C task transferred to the control executor on successful submission.
#[repr(C)]
pub struct Task {
    /// Opaque caller-owned context.
    pub context: *mut c_void,
    /// Invoke the task once on the serialized control thread.
    pub run: Option<unsafe extern "C" fn(*mut c_void)>,
    /// Release the context once after invocation.
    pub release: Option<unsafe extern "C" fn(*mut c_void)>,
}

// SAFETY: successful submission transfers exclusive ownership of context and callbacks.
unsafe impl Send for Task {}

/// Versioned standalone control-executor provider interface.
#[repr(C)]
pub struct Descriptor {
    /// Exact incompatible descriptor revision.
    pub abi_version: u32,
    /// Complete descriptor size in bytes.
    pub struct_size: usize,
    /// NUL-terminated capability identity.
    pub capability: *const c_char,
    /// Create a bounded serialized executor; null indicates failure.
    pub open: Option<unsafe extern "C" fn(*const c_char, usize) -> *mut c_void>,
    /// Submit: 0 accepted, 1 stopped, 2 full, 3 backend failure.
    pub submit: Option<unsafe extern "C" fn(*mut c_void, Task) -> c_int>,
    /// Stop admission and drain; -1 indicates invalid context or self-drain.
    pub stop_and_drain: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    /// Drain, join, and release; -1 leaves the context caller-owned.
    pub close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

// SAFETY: this immutable static table contains static text and function pointers.
unsafe impl Sync for Descriptor {}

struct Shared {
    queue: ArrayQueue<QueuedTask>,
    state: AtomicUsize,
    capacity: usize,
}

struct QueuedTask {
    context: *mut c_void,
    run: unsafe extern "C" fn(*mut c_void),
    release: unsafe extern "C" fn(*mut c_void),
}

// SAFETY: submission transfers exclusive ownership of the context and callbacks.
unsafe impl Send for QueuedTask {}

impl QueuedTask {
    fn into_task(self) -> Task {
        Task {
            context: self.context,
            run: Some(self.run),
            release: Some(self.release),
        }
    }
}

/// One bounded FIFO and its single serialized execution thread.
pub struct StandaloneExecutor {
    shared: Arc<Shared>,
    worker: Thread,
    join: AtomicPtr<JoinHandle<()>>,
    joining: AtomicBool,
    joined: AtomicBool,
}

impl StandaloneExecutor {
    fn open(name: &str, capacity: usize) -> Result<Self, OpenError> {
        if name.is_empty() || !valid_capacity(capacity) {
            return Err(OpenError::InvalidConfiguration);
        }
        let shared = Arc::new(Shared {
            queue: ArrayQueue::new(capacity),
            state: AtomicUsize::new(0),
            capacity,
        });
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name(format!("rptadv-{name}"))
            .spawn(move || worker_loop(worker_shared))
            .map_err(|_| OpenError::ThreadUnavailable)?;
        let worker = handle.thread().clone();
        Ok(Self {
            shared,
            worker,
            join: AtomicPtr::new(Box::into_raw(Box::new(handle))),
            joining: AtomicBool::new(false),
            joined: AtomicBool::new(false),
        })
    }

    fn reserve(&self) -> Result<(), RejectionReason> {
        let mut state = self.shared.state.load(Ordering::Acquire);
        loop {
            if state & STOPPED != 0 {
                return Err(RejectionReason::Stopped);
            }
            if state & PENDING >= self.shared.capacity {
                return Err(RejectionReason::Full);
            }
            match self.shared.state.compare_exchange_weak(
                state,
                state + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => state = actual,
            }
        }
    }

    fn submit(&self, task: Task) -> Result<(), (Task, RejectionReason)> {
        let Task {
            context,
            run,
            release,
        } = task;
        let (Some(run), Some(release)) = (run, release) else {
            return Err((
                Task {
                    context,
                    run,
                    release,
                },
                RejectionReason::Backend,
            ));
        };
        let task = QueuedTask {
            context,
            run,
            release,
        };
        if let Err(reason) = self.reserve() {
            return Err((task.into_task(), reason));
        }
        if let Err(task) = self.shared.queue.push(task) {
            self.shared.state.fetch_sub(1, Ordering::Release);
            return Err((task.into_task(), RejectionReason::Full));
        }
        self.worker.unpark();
        Ok(())
    }

    fn stop_and_drain(&self) -> Result<(), DrainError> {
        if CONTROL_OWNER.with(Cell::get) == Arc::as_ptr(&self.shared) {
            return Err(DrainError::OnExecutor);
        }
        self.shared.state.fetch_or(STOPPED, Ordering::AcqRel);
        self.worker.unpark();
        while self.shared.state.load(Ordering::Acquire) & PENDING != 0 {
            thread::sleep(Duration::from_millis(1));
        }
        self.join_worker();
        Ok(())
    }

    fn join_worker(&self) {
        if self.joined.load(Ordering::Acquire) {
            return;
        }
        if self
            .joining
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let handle = self.join.swap(ptr::null_mut(), Ordering::AcqRel);
            // SAFETY: open installs one handle, and the successful joining CAS
            // grants this caller its unique removal before any later call can proceed.
            let _ = unsafe { Box::from_raw(handle) }.join();
            self.joined.store(true, Ordering::Release);
        } else {
            while !self.joined.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

impl Drop for StandaloneExecutor {
    fn drop(&mut self) {
        if CONTROL_OWNER.with(Cell::get) == Arc::as_ptr(&self.shared) {
            self.shared.state.fetch_or(STOPPED, Ordering::AcqRel);
            self.worker.unpark();
            let handle = self.join.swap(ptr::null_mut(), Ordering::AcqRel);
            if !handle.is_null() {
                // SAFETY: dropping this unique handle detaches the worker; its shared queue
                // remains alive until the worker exits after finishing the current task.
                drop(unsafe { Box::from_raw(handle) });
            }
        } else {
            let _ = self.stop_and_drain();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RejectionReason {
    Stopped,
    Full,
    Backend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrainError {
    OnExecutor,
}

#[derive(Debug)]
enum OpenError {
    InvalidConfiguration,
    ThreadUnavailable,
}

fn worker_loop(shared: Arc<Shared>) {
    CONTROL_OWNER.with(|value| value.set(Arc::as_ptr(&shared)));
    loop {
        while let Some(task) = shared.queue.pop() {
            // The C ABI requires both callbacks to return normally.
            unsafe {
                (task.run)(task.context);
                (task.release)(task.context);
            }
            shared.state.fetch_sub(1, Ordering::Release);
        }
        let state = shared.state.load(Ordering::Acquire);
        if state & STOPPED != 0 && state & PENDING == 0 {
            break;
        }
        thread::park();
    }
    CONTROL_OWNER.with(|value| value.set(ptr::null()));
}

unsafe extern "C" fn open_abi(name: *const c_char, capacity: usize) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if name.is_null() {
            return ptr::null_mut();
        }
        let Ok(name) = (unsafe { CStr::from_ptr(name) }).to_str() else {
            return ptr::null_mut();
        };
        StandaloneExecutor::open(name, capacity).map_or(ptr::null_mut(), |executor| {
            Box::into_raw(Box::new(executor)).cast()
        })
    }))
    .unwrap_or(ptr::null_mut())
}

unsafe extern "C" fn submit_abi(context: *mut c_void, task: Task) -> c_int {
    if context.is_null() {
        return 3;
    }
    match unsafe { &*context.cast::<StandaloneExecutor>() }.submit(task) {
        Ok(()) => 0,
        Err((_, RejectionReason::Stopped)) => 1,
        Err((_, RejectionReason::Full)) => 2,
        Err((_, RejectionReason::Backend)) => 3,
    }
}

unsafe extern "C" fn drain_abi(context: *mut c_void) -> c_int {
    if context.is_null() {
        return -1;
    }
    if unsafe { &*context.cast::<StandaloneExecutor>() }
        .stop_and_drain()
        .is_ok()
    {
        0
    } else {
        -1
    }
}

unsafe extern "C" fn close_abi(context: *mut c_void) -> c_int {
    if unsafe { drain_abi(context) } != 0 {
        return -1;
    }
    unsafe { drop(Box::from_raw(context.cast::<StandaloneExecutor>())) };
    0
}

static DESCRIPTOR: Descriptor = Descriptor {
    abi_version: 1,
    struct_size: std::mem::size_of::<Descriptor>(),
    capability: c"rptadv.control".as_ptr(),
    open: Some(open_abi),
    submit: Some(submit_abi),
    stop_and_drain: Some(drain_abi),
    close: Some(close_abi),
};

/// Return the immutable standalone control-executor descriptor.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_control_standalone_descriptor_v1() -> *const Descriptor {
    &DESCRIPTOR
}

#[cfg(test)]
mod tests;
