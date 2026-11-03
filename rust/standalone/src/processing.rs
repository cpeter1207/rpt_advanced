//! Native peer processing owned by the dynamically loaded radio product.

use crate::abi;
use std::{ffi::c_void, mem::size_of, ptr::NonNull};

type Process = unsafe extern "C" fn(*mut c_void, u32, u32, *mut f32, u32) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);

struct Processor {
    handle: NonNull<c_void>,
    process: Process,
    destroy: Destroy,
    maximum: u32,
}

impl Drop for Processor {
    fn drop(&mut self) {
        // SAFETY: this unique owner is reclaimed only after its serial callback is quiescent.
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}

/// Prepare a unique native-rate processor without copying any processing policy into this host.
pub(crate) fn prepare(
    api: &abi::UrpAstDescriptor,
    ffmpeg: *const c_void,
    configuration: &str,
    node: &str,
    maximum_frames: usize,
    output: &mut abi::rptadv_pcm_processor_v1,
) -> Result<bool, String> {
    *output = abi::rptadv_pcm_processor_v1 {
        struct_size: size_of::<abi::rptadv_pcm_processor_v1>() as u32,
        abi_version: 1,
        context: std::ptr::null_mut(),
        process: None,
        destroy: None,
    };
    let (Some(create), Some(process), Some(destroy)) = (
        api.native_link_prepare,
        api.link_process_f32,
        api.link_destroy,
    ) else {
        return Err("shared product has no complete native peer-processing API".into());
    };
    let maximum = length(maximum_frames)?;
    if maximum == 0 || node.is_empty() {
        return Err("native peer processing requires a node and positive frame bound".into());
    }
    let args = abi::UrpNativeProcessingArgs {
        struct_size: size_of::<abi::UrpNativeProcessingArgs>() as u32,
        abi_version: 1,
        configuration: configuration.as_ptr(),
        configuration_length: length(configuration.len())?,
        node: node.as_ptr(),
        node_length: length(node.len())?,
    };
    let mut handle = std::ptr::null_mut();
    // SAFETY: request storage lasts through preparation. Composition retains the dynamic
    // product and FFmpeg provider until every transferred processor has been destroyed.
    let status = unsafe { create(&args, ffmpeg, maximum, &mut handle) };
    let Some(handle) = NonNull::new(handle) else {
        return if status == 0 {
            Ok(false)
        } else {
            Err(format!(
                "native peer processing preparation failed ({status})"
            ))
        };
    };
    let processor = Box::new(Processor {
        handle,
        process,
        destroy,
        maximum,
    });
    if status != 0 {
        return Err(format!(
            "native peer processing preparation failed ({status})"
        ));
    }
    output.context = Box::into_raw(processor).cast();
    output.process = Some(process_pcm);
    output.destroy = Some(destroy_processor);
    Ok(true)
}

fn length(value: usize) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| "processing span exceeds its ABI bound".into())
}

unsafe extern "C" fn process_pcm(context: *mut c_void, samples: *mut f32, count: usize) -> i32 {
    // SAFETY: non-null contexts are uniquely owned live allocations transferred by prepare.
    let Some(processor) = (unsafe { context.cast::<Processor>().as_mut() }) else {
        return -1;
    };
    if samples.is_null() || count > processor.maximum as usize {
        return -1;
    }
    // SAFETY: the sole audio owner supplies the advertised exclusive PCM span. Both the
    // provider and executable operation remain loaded through processor destruction.
    unsafe {
        (processor.process)(
            processor.handle.as_ptr(),
            abi::URP_AST_LINK_DIRECTION_READ,
            48_000,
            samples,
            count as u32,
        )
    }
}

unsafe extern "C" fn destroy_processor(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: prepare transfers this unique allocation; the host quiesces and destroys it once.
        drop(unsafe { Box::from_raw(context.cast::<Processor>()) });
    }
}

#[derive(Default)]
struct Diagnostics {
    messages: Vec<String>,
    invalid: bool,
}

/// Validate profiles in the shared product and copy its borrowed warning messages.
pub(crate) fn validate(
    api: &abi::UrpAstDescriptor,
    args: &abi::UrpNativeProcessingArgs,
) -> Result<Vec<String>, String> {
    let validate = api
        .native_processing_validate
        .ok_or("shared product has no native processing validator")?;
    let mut diagnostics = Diagnostics::default();
    // SAFETY: validation borrows its request and callback state only for this synchronous call.
    let status = unsafe {
        validate(
            args,
            Some(copy_diagnostic),
            std::ptr::from_mut(&mut diagnostics).cast(),
        )
    };
    if diagnostics.invalid {
        return Err("shared processing validator returned an invalid diagnostic".into());
    }
    if status != 0 {
        return Err(format!(
            "shared processing validation failed ({status}): {}",
            diagnostics.messages.join("; ")
        ));
    }
    Ok(diagnostics.messages)
}

unsafe extern "C" fn copy_diagnostic(context: *mut c_void, text: *const u8, length: u32) {
    // SAFETY: validate retains the complete callback state for this synchronous call.
    let diagnostics = unsafe { &mut *context.cast::<Diagnostics>() };
    if text.is_null() {
        diagnostics.invalid = true;
        return;
    }
    // SAFETY: the shared validator advertises this complete borrowed diagnostic span.
    match std::str::from_utf8(unsafe { std::slice::from_raw_parts(text, length as usize) }) {
        Ok(text) => diagnostics.messages.push(text.to_owned()),
        Err(_) => diagnostics.invalid = true,
    }
}

#[cfg(test)]
#[path = "processing_tests.rs"]
mod tests;
