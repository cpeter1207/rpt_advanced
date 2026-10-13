//! Per-test native faults; ordinary calls forward directly to the real SpeexDSP DSO.
use super::abi;
use std::{cell::Cell, ptr};

thread_local! {
    static FAIL_INIT: Cell<bool> = const { Cell::new(false) };
    static GET_RESPONSE: Cell<Option<(i32, u32)>> = const { Cell::new(None) };
}

pub(super) fn fail_init() {
    FAIL_INIT.set(true);
}

pub(super) fn get_response(kind: i32, span: u32) {
    GET_RESPONSE.set(Some((kind, span)));
}

pub(super) unsafe fn jitter_buffer_init(step: i32) -> *mut abi::JitterBuffer {
    if FAIL_INIT.replace(false) {
        ptr::null_mut()
    } else {
        unsafe { abi::jitter_buffer_init(step) }
    }
}

pub(super) unsafe fn jitter_buffer_get(
    context: *mut abi::JitterBuffer,
    packet: *mut abi::JitterBufferPacket,
    desired: i32,
    offset: *mut i32,
) -> i32 {
    if let Some((kind, span)) = GET_RESPONSE.replace(None) {
        unsafe { (*packet).span = span };
        kind
    } else {
        unsafe { abi::jitter_buffer_get(context, packet, desired, offset) }
    }
}
