//! A timed PCM consumer for the real private transport fixtures.
use super::{PrivateCall, abi};
use std::{ffi::c_void, mem::size_of};

#[derive(Default)]
pub(super) struct Audio {
    pub(super) original: usize,
    pub(super) gaps: u32,
    pub(super) digits: usize,
    pub(super) concealed: usize,
    pub(super) resets: u32,
    pub(super) qualified: usize,
}
unsafe extern "C" fn audio(context: *mut c_void, _: u64, pcm: *const f32, count: usize,
    concealed: u32, birth: *const abi::rptadv_source_birth_v1) -> usize {
    let capture = unsafe { &mut *context.cast::<Audio>() };
    if concealed == 0 {
        capture.original += count;
        if !pcm.is_null() && !birth.is_null()
            && unsafe { std::slice::from_raw_parts(pcm, count) }.iter().any(|v| v.abs() > 0.01) {
            capture.qualified += count;
        }
    } else {
        capture.concealed += count;
    }
    count
}
unsafe extern "C" fn gap(context: *mut c_void, _: u64, count: u32) -> u32 {
    unsafe { (*context.cast::<Audio>()).gaps += count };
    count
}
unsafe extern "C" fn discontinuity(context: *mut c_void) -> i32 {
    unsafe { (*context.cast::<Audio>()).resets += 1 };
    0
}
unsafe extern "C" fn digit(context: *mut c_void, _: u32) -> i32 {
    unsafe { (*context.cast::<Audio>()).digits += 1 };
    0
}

pub(super) fn read(call: &PrivateCall, capture: &mut Audio) -> Result<(),crate::Error> {
    let mut sink: abi::rptadv_peer_source_sink_v1 = unsafe { std::mem::zeroed() };
    sink.struct_size = size_of::<abi::rptadv_peer_source_sink_v1>() as u32;
    sink.abi_version = 1;
    sink.context = std::ptr::from_mut(capture).cast();
    sink.host_clock_epoch = call.generations().1;
    // The oldest kernel receipt can predate this dialog and its SELECT.
    // Use the same absolute monotonic clock as the production frontend.
    let mut now = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC,now.as_mut_ptr()) },0);
    let now = unsafe { now.assume_init() };
    sink.reference_now_ns = u64::try_from(now.tv_sec).unwrap()*1_000_000_000
        + u64::try_from(now.tv_nsec).unwrap();
    sink.audio_timed = Some(audio);
    sink.gap = Some(gap);
    sink.digit = Some(digit);
    sink.source_discontinuity = Some(discontinuity);
    let mut result = unsafe { std::mem::zeroed() };
    unsafe { call.read(0, &sink, &mut result) }
}
