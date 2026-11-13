//! Native peer processing owned by the dynamically loaded radio product.

use crate::abi;
use std::{
    ffi::c_void,
    mem::{offset_of, size_of},
    ptr::NonNull,
};

#[path = "processing_observations.rs"]
mod observations;
pub(crate) use observations::Observations;

type Process = unsafe extern "C" fn(*mut c_void, u32, u32, *mut f32, u32) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Buffering = unsafe extern "C" fn(*mut c_void, *mut abi::UrpProgrammeBuffering) -> i32;

struct Processor {
    handle: NonNull<c_void>,
    process: Process,
    destroy: Destroy,
    buffering: Option<Buffering>,
    maximum: u32,
    _observation: Option<std::sync::Arc<observations::Observation>>,
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
    observations: Option<&Observations>,
    output: &mut abi::rptadv_pcm_processor_v1,
) -> Result<bool, String> {
    *output = abi::rptadv_pcm_processor_v1 {
        struct_size: size_of::<abi::rptadv_pcm_processor_v1>() as u32,
        abi_version: 1,
        context: std::ptr::null_mut(),
        process: None,
        destroy: None,
        programme_buffering: None,
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
    let buffering = if api.struct_size as usize
        >= offset_of!(abi::UrpAstDescriptor, link_programme_buffering)
            + size_of::<Option<Buffering>>()
    {
        api.link_programme_buffering
    } else {
        None
    };
    let processor = Box::new(Processor {
        handle,
        process,
        destroy,
        buffering,
        maximum,
        _observation: None,
    });
    if status != 0 {
        return Err(format!(
            "native peer processing preparation failed ({status})"
        ));
    }
    let mut processor = processor;
    processor._observation = observations.map(|registry| registry.register(node, api, handle));
    output.context = Box::into_raw(processor).cast();
    output.process = Some(process_pcm);
    output.destroy = Some(destroy_processor);
    output.programme_buffering = buffering.map(|_| programme_buffering as Buffering);
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

unsafe extern "C" fn programme_buffering(
    context: *mut c_void,
    output: *mut abi::UrpProgrammeBuffering,
) -> i32 {
    // SAFETY: the transferred unique owner serializes this query with process_pcm.
    let Some(processor) = (unsafe { context.cast::<Processor>().as_mut() }) else {
        return -1;
    };
    if output.is_null() {
        return -1;
    }
    let Some(query) = processor.buffering else {
        return -1;
    };
    // SAFETY: the caller supplies the SDK sized output record and retains its
    // storage for this synchronous query. The real graph and library remain live.
    unsafe { query(processor.handle.as_ptr(), output) }
}

unsafe extern "C" fn destroy_processor(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: prepare transfers this unique allocation; the host quiesces and destroys it once.
        drop(unsafe { Box::from_raw(context.cast::<Processor>()) });
    }
}

#[cfg(test)]
#[path = "processing_tests.rs"]
mod tests;
