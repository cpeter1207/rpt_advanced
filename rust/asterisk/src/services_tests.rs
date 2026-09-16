use super::*;
use crate::fixture::{host, reset};

#[unsafe(no_mangle)]
extern "C" fn ast_replace_sigchld() {}
#[unsafe(no_mangle)]
extern "C" fn ast_unreplace_sigchld() {}
#[unsafe(no_mangle)]
extern "C" fn ast_log(_: i32, _: *const c_char, _: i32, _: *const c_char, _: *const c_char) {}

unsafe extern "C" fn current(_: *mut c_void) -> u32 {
    1
}
unsafe extern "C" fn render(_: *mut c_void, _: u32, _: *mut f32, _: usize) -> u32 {
    1
}
unsafe extern "C" fn event(context: *mut c_void, kind: u32, _: *const c_void, count: usize) {
    unsafe { &mut *context.cast::<Vec<(u32, usize)>>() }.push((kind, count));
}

#[test]
fn clock_notice_and_panic_boundaries_reject_invalid_inputs() {
    let null = ptr::null_mut();
    unsafe {
        assert_eq!(local_time(null, 0, ptr::null_mut()), -1);
        let mut result: ffi::rptadv_local_time_v1 = std::mem::zeroed();
        assert_eq!(local_time(null, 0, &mut result), -1);
        result.struct_size = size_of::<ffi::rptadv_local_time_v1>() as u32;
        for (seconds, valid) in [(0, 1), (i64::MAX, 0), (-70_000_000_000, 0)] {
            assert_eq!(local_time(null, seconds, &mut result), 0);
            assert_eq!(result.valid, valid);
        }
        for (name, length) in [
            (ptr::null(), 1),
            (ptr::null(), 0),
            (c"\xff".as_ptr(), 1),
            (c"x".as_ptr(), 2),
        ] {
            command_notice(null, name, length, 0);
        }
        command_notice(null, c"100".as_ptr(), 3, 1);
        command_notice(null, c"100".as_ptr(), 3, 0);
    }
    assert_eq!(boundary(-1, || panic!("callback fault")), -1);
    assert_eq!(descriptor().abi_version, 1);
}

#[test]
fn radio_service_validates_handles_and_releases_each_successful_open() {
    reset();
    let null = ptr::null_mut();
    unsafe {
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, ptr::null_mut()), -1);
        let mut radio = ptr::dangling_mut();
        assert_eq!(radio_open(null, ptr::null(), 1, 8, &mut radio), -1);
        assert!(radio.is_null());
        for failure in [1, 30] {
            host(|state| state.failure = failure);
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), -1);
            assert!(radio.is_null());
        }
        host(|state| state.failure = 0);
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
        assert_eq!(radio_ready(null, null), -1);
        assert_eq!(radio_ready(null, radio), 0);
        assert_eq!(radio_exchange(null, null, 0, Some(render), null), -1);
        assert_eq!(radio_exchange(null, radio, 0, None, null), -1);
        host(|state| state.voice(vec![8192; 2], 0));
        assert_eq!(radio_exchange(null, radio, 0, Some(render), null), 0);
        assert_eq!(radio_exchange(null, radio, 0, Some(render), null), -1);
        radio_destroy(null, radio);
        radio_destroy(null, null);
    }
    host(|state| state.clean());
}

#[test]
fn peer_services_validate_buffers_dispatch_borrowed_events_and_release_once() {
    reset();
    let null = ptr::null_mut();
    unsafe {
        assert_eq!(
            peer_dial(
                null,
                c"radio@host/200".as_ptr(),
                14,
                c"100".as_ptr(),
                3,
                8,
                Some(current),
                null,
                ptr::null_mut()
            ),
            -1
        );
        let mut peer = ptr::dangling_mut();
        for (destination, length, callback) in [
            (
                ptr::null(),
                1,
                Some(current as unsafe extern "C" fn(*mut c_void) -> u32),
            ),
            (c"x".as_ptr(), 1, None),
            (
                c"x".as_ptr(),
                2,
                Some(current as unsafe extern "C" fn(*mut c_void) -> u32),
            ),
        ] {
            assert_eq!(
                peer_dial(
                    null,
                    destination,
                    length,
                    c"100".as_ptr(),
                    3,
                    8,
                    callback,
                    null,
                    &mut peer
                ),
                -1
            );
            assert!(peer.is_null());
        }
        assert_eq!(
            peer_dial(
                null,
                c"radio@host/200".as_ptr(),
                14,
                c"100".as_ptr(),
                3,
                0,
                Some(current),
                null,
                &mut peer
            ),
            -1
        );
        assert_eq!(
            peer_dial(
                null,
                c"radio@host/200".as_ptr(),
                14,
                c"100".as_ptr(),
                3,
                8,
                Some(current),
                null,
                &mut peer
            ),
            0
        );
        assert_eq!(peer_rate(null, null), 0);
        assert_eq!(peer_rate(null, peer), 48000);
        assert_eq!(peer_ready(null, null), -1);
        assert_eq!(peer_ready(null, peer), 0);
        assert_eq!(peer_read(null, null, Some(event), null), -1);
        assert_eq!(peer_read(null, peer, None, null), -1);
        let mut received = Vec::<(u32, usize)>::new();
        let context = ptr::from_mut(&mut received).cast();
        host(|state| state.voice(vec![8192; 2], 0));
        assert_eq!(peer_read(null, peer, Some(event), context), 0);
        host(|state| {
            state.control(i32::from(b'#'));
            state.edit_frame(|frame| frame.frametype = ffi::AST_FRAME_DTMF_END);
        });
        assert_eq!(peer_read(null, peer, Some(event), context), 0);
        host(|state| {
            state.voice(vec![1; 2], 0);
            state.edit_frame(|frame| frame.frametype = ffi::AST_FRAME_TEXT);
        });
        assert_eq!(peer_read(null, peer, Some(event), context), 0);
        assert_eq!(received, [(3, 2), (2, 1), (1, 4)]);
        assert_eq!(peer_read(null, peer, Some(event), context), -1);
        assert_eq!(peer_send_text(null, null, c"x".as_ptr(), 1), -1);
        assert_eq!(peer_send_text(null, peer, ptr::null(), 1), -1);
        assert_eq!(peer_send_text(null, peer, c"x".as_ptr(), 2), -1);
        assert_eq!(peer_send_text(null, peer, c"x".as_ptr(), 1), 0);
        assert_eq!(peer_send_digit(null, null, b'1'), -1);
        assert_eq!(peer_send_digit(null, peer, b'x'), -1);
        assert_eq!(peer_send_digit(null, peer, b'1'), 0);
        assert_eq!(peer_write(null, null, ptr::null(), 0), -1);
        assert_eq!(peer_write(null, peer, ptr::null(), 1), -1);
        assert_eq!(peer_write(null, peer, ptr::null(), 0), 0);
        assert_eq!(peer_write(null, peer, [0.5].as_ptr(), 1), 0);
        peer_destroy(null, peer);
        peer_destroy(null, null);
    }
    host(|state| state.clean());
}

#[test]
fn directory_service_validates_method_output_capacity_and_authentication() {
    let null = ptr::null_mut();
    crate::link::directory::tests::valid_record();
    let mut output = [0; 128];
    let mut written = 0;
    let lookup = |method, remote, source, source_length, output, capacity, written| unsafe {
        directory_lookup(
            null,
            method,
            c"static".as_ptr(),
            6,
            ptr::null(),
            0,
            remote,
            3,
            source,
            source_length,
            output,
            capacity,
            written,
        )
    };
    assert_eq!(
        lookup(
            0,
            ptr::null(),
            ptr::null(),
            0,
            output.as_mut_ptr(),
            128,
            &mut written
        ),
        -1
    );
    assert_eq!(
        lookup(
            3,
            c"123".as_ptr(),
            ptr::null(),
            0,
            output.as_mut_ptr(),
            128,
            &mut written
        ),
        -1
    );
    for method in 0..=2 {
        assert_eq!(
            lookup(
                method,
                c"123".as_ptr(),
                ptr::null(),
                0,
                output.as_mut_ptr(),
                128,
                &mut written
            ),
            0
        );
    }
    assert_eq!(
        lookup(
            0,
            c"123".as_ptr(),
            c"192.0.2.1".as_ptr(),
            9,
            output.as_mut_ptr(),
            128,
            &mut written
        ),
        0
    );
    assert_eq!(written, 14);
    assert_eq!(
        lookup(
            0,
            c"bad".as_ptr(),
            ptr::null(),
            0,
            output.as_mut_ptr(),
            128,
            &mut written
        ),
        -1
    );
    assert_eq!(
        lookup(
            0,
            c"123".as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            128,
            &mut written
        ),
        -1
    );
    assert_eq!(
        lookup(
            0,
            c"123".as_ptr(),
            ptr::null(),
            0,
            output.as_mut_ptr(),
            128,
            ptr::null_mut()
        ),
        -1
    );
    assert_eq!(
        lookup(
            0,
            c"123".as_ptr(),
            ptr::null(),
            0,
            output.as_mut_ptr(),
            1,
            &mut written
        ),
        -1
    );
}
