use super::*;
use std::collections::VecDeque;
struct Pending {
    run: unsafe extern "C" fn(*mut c_void) -> c_int,
    context: *mut c_void,
}
unsafe impl Send for Pending {}
struct Backend {
    queue: Mutex<VecDeque<Pending>>,
    reject: std::sync::atomic::AtomicBool,
    name: CString,
    executing: Mutex<()>,
    pause_return: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    unreference_started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ast_taskprocessor_get(
    name: *const c_char,
    _options: u32,
) -> *mut bindings::ast_taskprocessor {
    if unsafe { CStr::from_ptr(name) }
        .to_bytes()
        .ends_with(b"-reject-open")
    {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Backend {
        queue: Mutex::new(VecDeque::new()),
        reject: std::sync::atomic::AtomicBool::new(false),
        name: unsafe { CStr::from_ptr(name) }.to_owned(),
        executing: Mutex::new(()),
        pause_return: Mutex::new(None),
        unreference_started: Mutex::new(None),
    }))
    .cast()
}
#[unsafe(no_mangle)]
unsafe extern "C" fn __ast_taskprocessor_push(
    handle: *mut bindings::ast_taskprocessor,
    run: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    context: *mut c_void,
    _file: *const c_char,
    _line: c_int,
    _function: *const c_char,
) -> c_int {
    let backend = unsafe { &*handle.cast::<Backend>() };
    if backend.reject.load(std::sync::atomic::Ordering::Relaxed) {
        return -1;
    }
    backend.queue.lock().unwrap().push_back(Pending {
        run: run.unwrap(),
        context,
    });
    0
}
thread_local! { static CURRENT: Cell<*mut bindings::ast_taskprocessor> = const { Cell::new(ptr::null_mut()) }; }
use std::cell::Cell;
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_taskprocessor_is_task(handle: *mut bindings::ast_taskprocessor) -> c_int {
    CURRENT.with(|current| i32::from(current.get() == handle))
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_taskprocessor_unreference(
    handle: *mut bindings::ast_taskprocessor,
) -> *mut c_void {
    let backend = unsafe { &*handle.cast::<Backend>() };
    if let Some(started) = backend.unreference_started.lock().unwrap().take() {
        started.send(()).unwrap();
    }
    // The real unique default taskprocessor joins its execution thread on final unreference.
    drop(backend.executing.lock().unwrap());
    assert!(backend.queue.lock().unwrap().is_empty());
    drop(unsafe { Box::from_raw(handle.cast::<Backend>()) });
    ptr::null_mut()
}
pub fn reject(handle: *mut bindings::ast_taskprocessor) {
    unsafe { &*handle.cast::<Backend>() }
        .reject
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn cleanup(handle: *mut bindings::ast_taskprocessor) {
    unsafe {
        ast_taskprocessor_unreference(handle);
    }
}
pub fn execute_all(handle: *mut bindings::ast_taskprocessor) {
    let backend = unsafe { &*handle.cast::<Backend>() };
    let _executing = backend.executing.lock().unwrap();
    loop {
        let pending = unsafe { &*handle.cast::<Backend>() }
            .queue
            .lock()
            .unwrap()
            .pop_front();
        let Some(pending) = pending else {
            if let Some((returned, resume)) = backend.pause_return.lock().unwrap().take() {
                returned.send(()).unwrap();
                resume.recv().unwrap();
            }
            break;
        };
        CURRENT.with(|current| current.set(handle));
        unsafe {
            (pending.run)(pending.context);
        }
        CURRENT.with(|current| current.set(ptr::null_mut()));
    }
}

pub fn name(handle: *mut bindings::ast_taskprocessor) -> String {
    unsafe { &*handle.cast::<Backend>() }
        .name
        .to_string_lossy()
        .into_owned()
}

pub fn pause_worker_exit(
    handle: *mut bindings::ast_taskprocessor,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
) {
    let backend = unsafe { &*handle.cast::<Backend>() };
    let (returned, receive_returned) = std::sync::mpsc::channel();
    let (resume, receive_resume) = std::sync::mpsc::channel();
    let (unreference, receive_unreference) = std::sync::mpsc::channel();
    *backend.pause_return.lock().unwrap() = Some((returned, receive_resume));
    *backend.unreference_started.lock().unwrap() = Some(unreference);
    (receive_returned, resume, receive_unreference)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ast_taskprocessor_seq_num() -> u32 {
    static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
