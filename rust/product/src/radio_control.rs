//! Physical maintenance is serialized with the current controller radio lease.

use crate::{
    abi,
    lifecycle::{input, selected},
};
use std::{
    ffi::{c_char, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

/// A copied maintenance result; no borrowed caller storage enters the executor.
pub(crate) struct Reply {
    pub(crate) request: abi::rptadv_radio_control_v1,
    pub(crate) text: String,
}

/// Dispatch one already-authorized local maintenance operation.
///
/// # Safety
/// All records and spans are readable for this synchronous call. The caller
/// exclusively owns `request`; a supplied sink must not unwind.
pub(crate) unsafe extern "C" fn execute(
    local: *const c_char,
    local_length: usize,
    request: *mut abi::rptadv_radio_control_v1,
    sink: abi::rptadv_text_sink_v1,
    sink_context: *mut c_void,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let local = unsafe { input(local, local_length) }
            .map_err(|_| -1)?
            .to_owned();
        let request_copy = unsafe { copy_request(request) }?;
        let (engine, _call) = selected().map_err(|_| -7)?;
        let reply = engine
            .run(move |host| Ok(host.radio_control(&local, request_copy)))
            .map_err(|_| -7)??;
        // SAFETY: the caller retains exclusive, complete writable storage until return.
        unsafe { request.write(reply.request) };
        if let Some(sink) = sink {
            // SAFETY: text is borrowed only during this synchronous sink invocation.
            unsafe { sink(sink_context, reply.text.as_ptr().cast(), reply.text.len()) };
        }
        Ok::<(), i32>(())
    }))
    .unwrap_or(Err(-7));
    result.map_or_else(|error| error, |()| 0)
}

unsafe fn copy_request(
    pointer: *const abi::rptadv_radio_control_v1,
) -> Result<abi::rptadv_radio_control_v1, i32> {
    if pointer.is_null()
        || unsafe { ptr::addr_of!((*pointer).struct_size).read() }
            != size_of::<abi::rptadv_radio_control_v1>() as u32
        || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != 1
    {
        return Err(-1);
    }
    // SAFETY: the complete sized record was validated above.
    let request = unsafe { pointer.read() };
    if !(1..=6).contains(&request.operation) || request.keyed > 1 {
        return Err(-1);
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_results_and_errors_cross_the_serialized_lease_without_losing_command_output() {
        let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
        unsafe extern "C" fn maintenance(
            _: *mut c_void,
            _: *mut c_void,
            request: *mut abi::rptadv_radio_control_v1,
            sink: abi::rptadv_text_sink_v1,
            context: *mut c_void,
        ) -> i32 {
            // SAFETY: the product owns and lends its complete copied request.
            let request = unsafe { &mut *request };
            if request.operation == 3 {
                return -6;
            }
            request.command.value = 777;
            if let Some(sink) = sink {
                let value = c"receive_input_peak=0.5";
                unsafe { sink(context, value.as_ptr(), value.to_bytes().len()) };
            }
            0
        }
        unsafe extern "C" fn text(context: *mut c_void, pointer: *const c_char, length: usize) {
            // SAFETY: this test retains the result and the complete provider span.
            let output = unsafe { &mut *context.cast::<String>() };
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            output.push_str(std::str::from_utf8(bytes).unwrap());
        }
        let api = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
        unsafe { api.stop.unwrap()() };
        let mut host = unsafe { *crate::fixture::host_descriptor() };
        host.radio_control = Some(maintenance);
        let source = "[1000]\nradio_channel=usb\n";
        crate::lifecycle::SKIP_TICKER_THREAD.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            unsafe {
                api.start.unwrap()(
                    &host,
                    crate::fixture::control_descriptor(),
                    rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                    rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                    source.as_ptr().cast(),
                    source.len(),
                )
            },
            0
        );
        let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
        request.struct_size = size_of_val(&request) as u32;
        request.abi_version = 1;
        request.operation = 2;
        let mut output = String::new();
        let call = api.radio_control.unwrap();
        assert_eq!(
            unsafe {
                call(
                    c"1000".as_ptr(),
                    4,
                    &mut request,
                    Some(text),
                    ptr::from_mut(&mut output).cast(),
                )
            },
            0
        );
        assert_eq!(request.command.value, 777);
        assert_eq!(output, "receive_input_peak=0.5");
        assert_eq!(
            unsafe { call(c"1000".as_ptr(), 4, &mut request, None, ptr::null_mut()) },
            0
        );
        request.operation = 3;
        assert_eq!(
            unsafe { call(c"1000".as_ptr(), 4, &mut request, None, ptr::null_mut()) },
            -6
        );
        assert_eq!(
            unsafe { call(c"2000".as_ptr(), 4, &mut request, None, ptr::null_mut()) },
            -4
        );
        assert_eq!(unsafe { api.stop.unwrap()() }, 0);
        crate::lifecycle::SKIP_TICKER_THREAD.store(false, std::sync::atomic::Ordering::Release);
    }

    #[test]
    fn descriptor_copies_and_validates_maintenance_before_control_dispatch() {
        let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
        let api = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
        unsafe { api.stop.unwrap()() };
        let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
        let call = api.radio_control.unwrap();
        assert_ne!(
            unsafe { call(c"1000".as_ptr(), 4, &mut request, None, ptr::null_mut()) },
            0
        );
        request.struct_size = size_of_val(&request) as u32;
        request.abi_version = 1;
        request.operation = 1;
        assert_ne!(
            unsafe { call(c"1000".as_ptr(), 4, &mut request, None, ptr::null_mut()) },
            0
        );
        assert_eq!(unsafe { copy_request(&request) }.unwrap().operation, 1);
        request.abi_version = 2;
        assert!(unsafe { copy_request(&request) }.is_err());
        request.abi_version = 1;
        request.operation = 0;
        assert!(unsafe { copy_request(&request) }.is_err());
        request.operation = 3;
        request.keyed = 2;
        assert!(unsafe { copy_request(&request) }.is_err());
        assert!(unsafe { copy_request(ptr::null()) }.is_err());
    }
}
