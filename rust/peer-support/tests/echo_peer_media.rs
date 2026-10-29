//! Shared host calls preserve source credit and distinguish retained egress from failure.
#![cfg(unix)]
use rptadv_peer_support::echolink::{abi, host::Error};
use std::{ffi::c_void, mem::size_of, ptr};
mod support;

#[repr(C)]
struct EncodeResult {
    size: u32,
    status: u32,
    consumed: usize,
    bytes: usize,
    start: u64,
    source: u32,
    packet: u32,
    required: usize,
}

#[test]
fn shared_media_preserves_provider_status_and_original_pacing_arguments() {
    let fixture = support::Fixture::new("network");
    let peer = fixture.peer();
    let mut result = EncodeResult {
        size: size_of::<EncodeResult>() as u32,
        status: 0,
        consumed: 0,
        bytes: 0,
        start: 0,
        source: 0,
        packet: 0,
        required: 0,
    };
    let mut packet = [0; 4];
    unsafe {
        peer.prepare_audio(
            &[0.25, -0.25],
            &mut packet,
            7,
            ptr::from_mut(&mut result).cast(),
        )
    }
    .unwrap();
    assert_eq!((result.status, result.consumed, result.bytes), (2, 0, 0));
    assert_eq!(result.required, 12);
    assert_eq!(unsafe { peer.reset_encoder() }, Ok(()));
    assert_eq!(peer.elapsed_ms(), Ok(0x1_0000_0007));
    assert_eq!(unsafe { peer.set_deadline(123456) }, Ok(()));
    assert_eq!(unsafe { peer.set_deadline(u64::MAX) }, Ok(()));
    for (packet, expected) in [
        (&b"ok"[..], Ok(true)),
        (&b"full"[..], Ok(false)),
        (&b"pending"[..], Ok(false)),
        (&b"failed"[..], Err(Error::Status(abi::Status::Failed))),
    ] {
        assert_eq!(unsafe { peer.send_prepared(packet, 160, 4567) }, expected);
    }
    assert_eq!(unsafe { peer.send_text(b"W1ABC", b"hello") }, Ok(()));
}

#[derive(Default)]
struct Received {
    audio: usize,
    gap: u32,
    text: usize,
}
unsafe extern "C" fn audio(
    context: *mut c_void,
    start: u64,
    pcm: *const f32,
    count: usize,
    plc: u32,
) -> usize {
    assert_eq!((start, count, plc), (800, 3, 1));
    assert_eq!(
        unsafe { std::slice::from_raw_parts(pcm, count) },
        [0.25, 0.5, -0.25]
    );
    unsafe { &mut *context.cast::<Received>() }.audio += count;
    2
}
unsafe extern "C" fn gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    assert_eq!((start, count), (803, 160));
    unsafe { &mut *context.cast::<Received>() }.gap += count;
    80
}
unsafe extern "C" fn text(context: *mut c_void, sender: abi::ByteSpan, body: abi::ByteSpan) {
    assert_eq!(
        unsafe { std::slice::from_raw_parts(sender.data, sender.length) },
        b"W1ABC"
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts(body.data, body.length) },
        b"hello"
    );
    unsafe { &mut *context.cast::<Received>() }.text += 1;
}

#[test]
fn shared_read_preserves_source_positions_credit_and_finite_completion() {
    let fixture = support::Fixture::new("network");
    let peer = fixture.peer();
    let mut received = Received::default();
    let context = ptr::from_mut(&mut received).cast();
    let sink = abi::PcmSink {
        struct_size: size_of::<abi::PcmSink>() as u32,
        abi_version: 1,
        context,
        audio: Some(audio),
        gap: Some(gap),
    };
    for completion in 0..=2 {
        let result = unsafe { peer.read(completion, Some(&sink), Some(text), context) }.unwrap();
        assert_eq!(
            (
                result.progress,
                result.activity,
                result.finish_pending,
                result.state
            ),
            if completion == 0 {
                (2, 1, 1, 1)
            } else {
                (3, 2, 0, 1)
            }
        );
    }
    assert_eq!((received.audio, received.gap, received.text), (3, 160, 3));
    let result = unsafe { peer.read(0, None, None, ptr::null_mut()) }.unwrap();
    assert_eq!(
        result.progress, 0,
        "a state-only query must not invent source progress"
    );
}
