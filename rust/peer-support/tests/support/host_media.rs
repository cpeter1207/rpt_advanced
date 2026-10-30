//! Same consumer-visible dispatch checks for both native hosts.
use super::{host_abi as abi, *};

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
unsafe extern "C" fn text(
    context: *mut c_void,
    sender: abi::rptadv_peer_bytes_v1,
    body: abi::rptadv_peer_bytes_v1,
) {
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
unsafe extern "C" fn step(context: *mut c_void) -> i32 {
    unsafe { &*context.cast::<std::sync::atomic::AtomicUsize>() }
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    0
}

pub unsafe fn check(context: *mut c_void) {
    let fixture = support::Fixture::new("network");
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let fault =
        unsafe { library.get::<unsafe extern "C" fn(usize)>(b"fixture_record_fault\0") }.unwrap();
    let handle = Box::into_raw(Box::new(PeerHandle::Echo(fixture.peer()))).cast();
    unsafe { rejected_records(context, handle) };
    let mut observed: abi::rptadv_peer_observation_v1 = unsafe { std::mem::zeroed() };
    observed.struct_size = size_of::<abi::rptadv_peer_observation_v1>() as u32;
    observed.abi_version = 1;
    assert_eq!(unsafe { peer_observe(context, handle, &mut observed) }, 0);
    assert_eq!(
        (
            observed.echo_valid,
            observed.echo_state,
            observed.echo_first_cause,
            observed.echo_codec_plc_samples
        ),
        (1, 2, 4, 640)
    );
    assert_eq!(
        (
            observed.ingress_overflow_drops,
            observed.ingress_queued_packets,
            observed.ingress_capacity_packets
        ),
        (12, 3, 64),
        "the host must copy real per-peer ingress credit"
    );
    assert_eq!(observed.echo_ingress_bytes, 123);
    unsafe { fault(56) };
    assert_eq!(unsafe { peer_observe(context, handle, &mut observed) }, -1);
    unsafe { fault(57) };
    assert_eq!(unsafe { peer_reset_encoder(context, handle) }, -1);
    assert_eq!(unsafe { peer_elapsed_ms(context, handle) }, 0);
    unsafe { fault(0) };
    let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
    result.struct_size = size_of::<abi::rptadv_codec_encode_result_v1>() as u32;
    let mut packet = [0; 4];
    unsafe {
        assert_eq!(
            peer_prepare_audio(
                context,
                handle,
                [0.25, -0.25].as_ptr(),
                2,
                packet.as_mut_ptr(),
                packet.len(),
                7,
                &mut result
            ),
            2,
            "provider Ok must not hide the encoder's OutputFull result"
        );
        assert_eq!(
            (
                result.consumed_frames,
                result.encoded_bytes,
                result.required_capacity
            ),
            (0, 0, 12)
        );
        assert_eq!(peer_reset_encoder(context, handle), 0);
        assert_eq!(
            peer_prepare_audio(
                context,
                handle,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                7,
                &mut result
            ),
            2
        );
        assert_eq!(
            peer_prepare_audio(
                context,
                handle,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                8,
                &mut result
            ),
            -1
        );
        assert_eq!(
            peer_send_prepared(context, handle, ptr::null(), 0, 160, 4567),
            -1
        );
        assert_eq!(peer_elapsed_ms(context, handle), u32::MAX);
        assert_eq!(peer_set_deadline(context, handle, 123456), 0);
        let span = |value: &[u8]| abi::rptadv_peer_bytes_v1 {
            data: value.as_ptr(),
            length: value.len(),
        };
        assert_eq!(
            peer_send_human_text(context, handle, span(b"W1ABC"), span(b"hello")),
            0
        );
        assert_eq!(
            peer_send_human_text(context, handle, span(b"W1ABC"), span(b"")),
            -1
        );
        for (bytes, expected) in [
            (&b"ok"[..], 0),
            (&b"full"[..], 1),
            (&b"pending"[..], 1),
            (&b"failed"[..], -1),
        ] {
            assert_eq!(
                peer_send_prepared(context, handle, bytes.as_ptr(), bytes.len(), 160, 4567),
                expected
            );
        }
        let mut received = Received::default();
        let receive_context = ptr::from_mut(&mut received).cast();
        let sink = abi::rptadv_peer_source_sink_v1 {
            struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
            abi_version: 1,
            context: receive_context,
            audio: Some(audio),
            gap: Some(gap),
        };
        let mut output: abi::rptadv_peer_source_result_v1 = std::mem::zeroed();
        output.struct_size = size_of::<abi::rptadv_peer_source_result_v1>() as u32;
        output.abi_version = 1;
        for completion in 0..=2 {
            assert_eq!(
                peer_read_source(
                    context,
                    handle,
                    completion,
                    &sink,
                    Some(text),
                    receive_context,
                    &mut output
                ),
                0
            );
            assert_eq!(
                (
                    output.progress,
                    output.activity,
                    output.finish_pending,
                    output.state,
                    output.represented_node_number,
                ),
                if completion == 0 {
                    (2, 1, 1, 1, 123456)
                } else {
                    (3, 2, 0, 1, 123456)
                }
            );
        }
        assert_eq!((received.audio, received.gap, received.text), (3, 160, 3));
        assert_eq!(
            peer_read_source(
                context,
                handle,
                1,
                ptr::null(),
                None,
                ptr::null_mut(),
                &mut output
            ),
            0
        );
        assert_eq!(
            peer_read_source(
                context,
                handle,
                3,
                ptr::null(),
                None,
                ptr::null_mut(),
                &mut output
            ),
            -1
        );
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mut registration = ptr::null_mut();
        assert_eq!(
            peer_step_attach(
                context,
                handle,
                Some(step),
                ptr::from_ref(&calls).cast_mut().cast(),
                &mut registration
            ),
            0
        );
        peer_step_wake(context, registration);
        peer_step_stop(context, registration);
        peer_step_wake(context, registration);
        peer_step_detach(context, registration);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        peer_destroy(context, handle);
    }
}

/// Reject foreign record prefixes and invalid spans before reading their payloads.
unsafe fn rejected_records(context: *mut c_void, handle: *mut c_void) {
    for field in 0..=3 {
        let mut info: abi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
        info.struct_size = size_of::<abi::rptadv_peer_info_v1>() as u32;
        info.abi_version = 1;
        let mut output = ptr::from_mut(&mut info);
        let mut peer = handle;
        match field {
            0 => peer = ptr::null_mut(),
            1 => output = ptr::null_mut(),
            2 => info.struct_size = 0,
            _ => info.abi_version = 0,
        }
        assert_eq!(unsafe { peer_info(context, peer, output) }, -1);
    }
    for field in 0..=6 {
        let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<abi::rptadv_codec_encode_result_v1>() as u32;
        let pcm = [0.25, -0.25];
        let mut packet = [0; 4];
        let (mut peer, mut samples, mut count, mut bytes, mut capacity, mut output) = (
            handle,
            pcm.as_ptr(),
            pcm.len(),
            packet.as_mut_ptr(),
            packet.len(),
            ptr::from_mut(&mut result),
        );
        match field {
            0 => peer = ptr::null_mut(),
            1 => output = ptr::null_mut(),
            2 => result.struct_size = 0,
            3 => count = isize::MAX as usize / size_of::<f32>() + 1,
            4 => capacity = isize::MAX as usize + 1,
            5 => samples = ptr::null(),
            _ => bytes = ptr::null_mut(),
        }
        assert_eq!(
            unsafe {
                peer_prepare_audio(context, peer, samples, count, bytes, capacity, 7, output)
            },
            -1
        );
    }
    for field in 0..=3 {
        let mut output: abi::rptadv_peer_observation_v1 = unsafe { std::mem::zeroed() };
        output.struct_size = size_of::<abi::rptadv_peer_observation_v1>() as u32;
        output.abi_version = 1;
        let mut out = ptr::from_mut(&mut output);
        let mut peer = handle;
        match field {
            0 => peer = ptr::null_mut(),
            1 => out = ptr::null_mut(),
            2 => output.struct_size = 0,
            _ => output.abi_version = 0,
        }
        assert_eq!(unsafe { peer_observe(context, peer, out) }, -1);
    }
    for field in 0..=5 {
        let mut output: abi::rptadv_peer_source_result_v1 = unsafe { std::mem::zeroed() };
        output.struct_size = size_of::<abi::rptadv_peer_source_result_v1>() as u32;
        output.abi_version = 1;
        let mut sink = abi::rptadv_peer_source_sink_v1 {
            struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
            abi_version: 1,
            context: ptr::null_mut(),
            audio: None,
            gap: None,
        };
        let (mut peer, mut out) = (handle, ptr::from_mut(&mut output));
        match field {
            0 => peer = ptr::null_mut(),
            1 => out = ptr::null_mut(),
            2 => output.struct_size = 0,
            3 => output.abi_version = 0,
            4 => sink.struct_size = 0,
            _ => sink.abi_version = 0,
        }
        assert_eq!(
            unsafe { peer_read_source(context, peer, 0, &sink, None, ptr::null_mut(), out) },
            -1
        );
    }
    unsafe {
        assert_eq!(
            peer_step_attach(context, handle, None, ptr::null_mut(), ptr::null_mut()),
            -1
        );
        let mut token = ptr::null_mut();
        assert_eq!(
            peer_step_attach(context, ptr::null_mut(), None, ptr::null_mut(), &mut token),
            -1
        );
        assert_eq!(
            peer_step_attach(context, handle, None, ptr::null_mut(), &mut token),
            -1
        );
        peer_step_stop(context, ptr::null_mut());
        peer_step_wake(context, ptr::null_mut());
        peer_step_detach(context, ptr::null_mut());
        assert_eq!(peer_reset_encoder(context, ptr::null_mut()), -1);
        assert_eq!(peer_elapsed_ms(context, ptr::null_mut()), 0);
        assert_eq!(peer_set_deadline(context, ptr::null_mut(), 0), -1);
        assert_eq!(
            peer_send_prepared(context, ptr::null_mut(), ptr::null(), 0, 160, 4567),
            -1
        );
        assert_eq!(
            peer_send_prepared(context, handle, ptr::null(), 1, 160, 4567),
            -1
        );
        assert_eq!(
            peer_send_prepared(
                context,
                handle,
                ptr::null(),
                isize::MAX as usize + 1,
                160,
                4567
            ),
            -1
        );
        let empty = abi::rptadv_peer_bytes_v1 {
            data: ptr::null(),
            length: 0,
        };
        assert_eq!(
            peer_send_human_text(context, ptr::null_mut(), empty, empty),
            -1
        );
    }
}
