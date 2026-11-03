use super::*;
use crate::abi;
use std::ffi::c_void;
use std::{cell::Cell, mem::size_of, ptr};

thread_local! {
    static MODE: Cell<u8> = const { Cell::new(0) };
    static PREPARES: Cell<usize> = const { Cell::new(0) };
    static PROCESSES: Cell<usize> = const { Cell::new(0) };
    static DESTROYS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn create(
    args: *const abi::UrpNativeProcessingArgs,
    ffmpeg: *const c_void,
    maximum: u32,
    output: *mut *mut c_void,
) -> i32 {
    // SAFETY: preparation retains the request and its text for this synchronous call.
    let args = unsafe { &*args };
    assert_eq!(
        args.struct_size as usize,
        size_of::<abi::UrpNativeProcessingArgs>()
    );
    assert_eq!(args.abi_version, 1);
    // SAFETY: the request advertises both retained UTF-8 spans.
    assert_eq!(
        unsafe {
            std::slice::from_raw_parts(args.configuration, args.configuration_length as usize)
        },
        b"[link]\nlimiter_enabled=yes\n"
    );
    // SAFETY: the request advertises the retained node-name span.
    assert_eq!(
        unsafe { std::slice::from_raw_parts(args.node, args.node_length as usize) },
        b"524950"
    );
    assert_eq!(ffmpeg, ptr::NonNull::<u8>::dangling().as_ptr().cast());
    assert_eq!(maximum, 4);
    PREPARES.set(PREPARES.get() + 1);
    let mode = MODE.get();
    // SAFETY: the caller provides one writable handle destination.
    unsafe {
        *output = if matches!(mode, 1 | 2) {
            ptr::null_mut()
        } else {
            Box::into_raw(Box::new(7_u8)).cast()
        }
    };
    if matches!(mode, 2 | 3) { -7 } else { 0 }
}

unsafe extern "C" fn process(
    context: *mut c_void,
    direction: u32,
    rate: u32,
    samples: *mut f32,
    count: u32,
) -> i32 {
    assert!(!context.is_null());
    assert_eq!(direction, abi::URP_AST_LINK_DIRECTION_READ);
    assert_eq!(rate, 48_000);
    assert!(count <= 4);
    PROCESSES.set(PROCESSES.get() + 1);
    // SAFETY: the wrapper forwards the caller's complete exclusive PCM span.
    for sample in unsafe { std::slice::from_raw_parts_mut(samples, count as usize) } {
        *sample *= 0.5;
    }
    if MODE.get() == 4 { -9 } else { 0 }
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    // SAFETY: every non-null handle was allocated by create and is released once.
    drop(unsafe { Box::from_raw(context.cast::<u8>()) });
    DESTROYS.set(DESTROYS.get() + 1);
}

unsafe extern "C" fn inspect(
    _: *const abi::UrpNativeProcessingArgs,
    diagnostic: abi::UrpNativeProcessingDiagnostic,
    context: *mut c_void,
) -> i32 {
    let diagnostic = diagnostic.unwrap();
    // SAFETY: validation retains its diagnostic state and callback for this call.
    unsafe { diagnostic(context, b"first warning".as_ptr(), 13) };
    match MODE.get() {
        5 => {
            // SAFETY: this deliberately malformed diagnostic tests pointer rejection.
            unsafe { diagnostic(context, ptr::null(), 1) };
            0
        }
        6 => {
            // SAFETY: the borrowed byte exists; validation rejects its invalid UTF-8.
            unsafe { diagnostic(context, [0xff].as_ptr(), 1) };
            0
        }
        7 => -8,
        _ => {
            // SAFETY: the complete second diagnostic remains readable for the callback.
            unsafe { diagnostic(context, b"second warning".as_ptr(), 14) };
            0
        }
    }
}

fn fixture() -> abi::UrpAstDescriptor {
    MODE.set(0);
    PREPARES.set(0);
    PROCESSES.set(0);
    DESTROYS.set(0);
    // SAFETY: this descriptor contains only scalar metadata and nullable function pointers.
    let mut api: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
    api.native_link_prepare = Some(create);
    api.link_process_f32 = Some(process);
    api.link_destroy = Some(destroy);
    api.native_processing_validate = Some(inspect);
    api
}

fn output() -> abi::rptadv_pcm_processor_v1 {
    abi::rptadv_pcm_processor_v1 {
        struct_size: size_of::<abi::rptadv_pcm_processor_v1>() as u32,
        abi_version: 1,
        context: ptr::null_mut(),
        process: None,
        destroy: None,
    }
}

fn prepared(
    api: &abi::UrpAstDescriptor,
    output: &mut abi::rptadv_pcm_processor_v1,
) -> Result<bool, String> {
    prepare(
        api,
        ptr::NonNull::<u8>::dangling().as_ptr().cast(),
        "[link]\nlimiter_enabled=yes\n",
        "524950",
        4,
        output,
    )
}

#[test]
fn native_peer_processor_owns_one_handle_and_preserves_bounds_and_provider_status() {
    let api = fixture();
    let mut output = output();
    assert_eq!(prepared(&api, &mut output), Ok(true));
    assert!(!output.context.is_null());
    let mut samples = [0.8_f32, -0.4];
    // SAFETY: preparation transferred this unique callback context and complete PCM span.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), samples.len()) },
        0
    );
    assert_eq!(samples, [0.4, -0.2]);
    MODE.set(4);
    // SAFETY: the same serial owner retains its context and PCM workspace.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), samples.len()) },
        -9
    );
    // SAFETY: malformed spans are rejected before a provider can dereference them.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, ptr::null_mut(), 1) },
        -1
    );
    // SAFETY: the advertised oversized count must be rejected before provider access.
    assert_eq!(
        unsafe { output.process.unwrap()(output.context, samples.as_mut_ptr(), 5) },
        -1
    );
    // SAFETY: a null context is rejected before provider access.
    assert_eq!(
        unsafe { output.process.unwrap()(ptr::null_mut(), samples.as_mut_ptr(), 1) },
        -1
    );
    assert_eq!(PROCESSES.get(), 2);
    // SAFETY: the unique context is released once after callback use has ended.
    unsafe { output.destroy.unwrap()(output.context) };
    // SAFETY: null destruction is a no-op and transfers no ownership.
    unsafe { output.destroy.unwrap()(ptr::null_mut()) };
    assert_eq!(DESTROYS.get(), 1);
}

#[test]
fn preparation_bypass_and_failure_clean_up_only_acquired_handles() {
    let api = fixture();
    for mode in [1, 2, 3] {
        MODE.set(mode);
        let mut output = output();
        let result = prepared(&api, &mut output);
        if mode == 1 {
            assert_eq!(result, Ok(false));
        } else {
            assert!(result.unwrap_err().contains("-7"));
        }
        assert!(output.context.is_null());
        assert!(output.process.is_none());
        assert!(output.destroy.is_none());
    }
    assert_eq!(DESTROYS.get(), 1);
}

#[test]
fn invalid_preparation_is_rejected_before_acquiring_a_handle() {
    let api = fixture();
    for missing in 0..3 {
        let mut api = api;
        match missing {
            0 => api.native_link_prepare = None,
            1 => api.link_process_f32 = None,
            _ => api.link_destroy = None,
        }
        assert!(prepared(&api, &mut output()).is_err());
    }
    for (node, maximum) in [("", 4), ("524950", 0), ("524950", usize::MAX)] {
        assert!(prepare(&api, ptr::null(), "", node, maximum, &mut output()).is_err());
    }
    assert_eq!(PREPARES.get(), 0);
    assert!(length(usize::MAX).is_err());
    assert_eq!(length(0), Ok(0));
}

#[test]
fn processing_validation_copies_warnings_and_reports_errors_and_bad_diagnostics() {
    let mut api = fixture();
    let args = abi::UrpNativeProcessingArgs {
        struct_size: size_of::<abi::UrpNativeProcessingArgs>() as u32,
        abi_version: 1,
        configuration: ptr::null(),
        configuration_length: 0,
        node: b"524950".as_ptr(),
        node_length: 6,
    };
    assert_eq!(
        validate(&api, &args).unwrap(),
        ["first warning", "second warning"]
    );
    for mode in [5, 6, 7] {
        MODE.set(mode);
        assert!(validate(&api, &args).is_err());
    }
    api.native_processing_validate = None;
    assert!(validate(&api, &args).is_err());
}
