use super::*;
use crate::fixture::{host, reset};

const DIRECT_ACK_MISSING: u8 = 41;
const DIRECT_ACK_WRONG: u8 = 42;
const DIRECT_OPTION_FAILED: u8 = 43;
const LINK_ACK_MISSING: u8 = 44;
const LINK_ACK_WRONG: u8 = 45;
const LINK_OPTION_FAILED: u8 = 46;

#[unsafe(no_mangle)]
extern "C" fn ast_replace_sigchld() {}
#[unsafe(no_mangle)]
extern "C" fn ast_unreplace_sigchld() {}
#[unsafe(no_mangle)]
extern "C" fn ast_log(_: i32, _: *const c_char, _: i32, _: *const c_char, _: *const c_char) {}

unsafe extern "C" fn current(_: *mut c_void) -> u32 {
    1
}
unsafe extern "C" fn receive(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
    0
}
unsafe extern "C" fn transmit(_: *mut c_void, _: *mut f32, _: u32, _: *mut u32) -> i32 {
    0
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_setoption(
    channel: *mut ffi::ast_channel,
    option: i32,
    data: *mut c_void,
    length: i32,
    block: i32,
) -> i32 {
    assert_eq!(block, 0);
    if option == 0x52504C41 {
        assert_eq!(length as usize, size_of::<ffi::urp_ast_link_attach>());
        let attachment = unsafe { &mut *data.cast::<ffi::urp_ast_link_attach>() };
        assert_eq!(
            attachment.struct_size as usize,
            size_of::<ffi::urp_ast_link_attach>()
        );
        assert_eq!(attachment.abi_version, 1);
        assert_eq!(attachment.accepted_abi_version, 0);
        assert_eq!(channel, host(|state| state.token(0)));
        assert_eq!(attachment.peer_channel, ptr::dangling_mut());
        assert!(host(|state| state
            .read_rates
            .contains_key(&(attachment.peer_channel as usize))));
        let failure = host(|state| state.failure);
        attachment.accepted_abi_version = match failure {
            LINK_ACK_MISSING => 0,
            LINK_ACK_WRONG => 2,
            _ => 1,
        };
        return if failure == LINK_OPTION_FAILED { -1 } else { 0 };
    }
    assert_eq!(option, 0x52504144);
    assert_eq!(length as usize, size_of::<ffi::urp_ast_direct_callbacks>());
    // SAFETY: attach_direct supplies exclusive access to this complete live descriptor.
    let descriptor = unsafe { &mut *data.cast::<ffi::urp_ast_direct_callbacks>() };
    assert!(descriptor.receive.is_some() && descriptor.transmit.is_some());
    assert_eq!(descriptor.abi_version, 2);
    assert_eq!(descriptor.accepted_abi_version, 0);
    let failure = host(|state| state.failure);
    descriptor.accepted_abi_version = match failure {
        DIRECT_ACK_MISSING => 0,
        DIRECT_ACK_WRONG => 1,
        _ => 2,
    };
    if failure == DIRECT_OPTION_FAILED {
        -1
    } else {
        0
    }
}
unsafe extern "C" fn event(context: *mut c_void, kind: u32, _: *const c_void, count: usize) {
    unsafe { &mut *context.cast::<Vec<(u32, usize)>>() }.push((kind, count));
}

#[test]
fn radio_activation_accepts_the_installed_direct_callback_abi() {
    reset();
    let null = ptr::null_mut();
    unsafe {
        let mut radio = null;
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
        let result = radio_activate(null, radio, Some(receive), null, Some(transmit), null);
        radio_destroy(null, radio);
        assert_eq!(result, 0);
    }
    host(|state| {
        assert_eq!(state.calls, 1);
        state.clean();
    });
}

#[test]
fn radio_activation_rejects_unacknowledged_direct_callbacks_before_call() {
    for failure in [DIRECT_ACK_MISSING, DIRECT_ACK_WRONG, DIRECT_OPTION_FAILED] {
        reset();
        host(|state| state.failure = failure);
        let null = ptr::null_mut();
        // SAFETY: the fixture owns this handle, and callbacks outlive its destroy.
        unsafe {
            let mut radio = null;
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
            let result = radio_activate(null, radio, Some(receive), null, Some(transmit), null);
            let retained_channels = host(|state| state.channels);
            radio_destroy(null, radio);
            assert_eq!(
                result, -1,
                "direct attachment must be acknowledged: {failure}"
            );
            assert_eq!(
                retained_channels, 0,
                "failed attachment must hang up before returning"
            );
        }
        host(|state| {
            assert_eq!(state.calls, 0, "unaccepted callbacks must not start media");
            state.clean();
        });
    }
}

#[test]
fn missing_direct_callbacks_destroy_channels_and_cannot_reactivate_the_reservation() {
    for (receive, transmit) in [
        (
            None,
            Some(transmit as unsafe extern "C" fn(_, _, _, _) -> _),
        ),
        (Some(receive as unsafe extern "C" fn(_, _, _, _) -> _), None),
    ] {
        reset();
        let null = ptr::null_mut();
        unsafe {
            let mut radio = null;
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
            assert_eq!(
                radio_activate(null, radio, receive, null, transmit, null),
                -1
            );
            assert_eq!(
                radio_activate(null, radio, receive, null, transmit, null),
                -1
            );
            radio_destroy(null, radio);
        }
        host(|state| {
            assert_eq!(state.calls, 0);
            state.clean();
        });
    }
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
    assert_eq!(descriptor().abi_version, 3);
}

#[test]
fn peer_binding_requires_radio_option_acknowledgment_and_preserves_owners() {
    for failure in [0, LINK_ACK_MISSING, LINK_ACK_WRONG, LINK_OPTION_FAILED] {
        reset();
        let null = ptr::null_mut();
        unsafe {
            let mut radio = null;
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
            let peer = PeerIo::dial(c"radio@host/200", c"100", 8, || true).unwrap();
            assert_eq!(
                host(|state| state.read_rates[&(peer.channel.pointer.as_ptr() as usize)]),
                peer.rate(),
                "the hook must see the already negotiated decoded rate"
            );
            let peer = into_raw(peer);
            host(|state| state.failure = failure);
            assert_eq!(
                peer_bind_radio(null, peer, radio),
                if failure == 0 { 0 } else { -1 }
            );
            assert_eq!(peer_bind_radio(null, null, radio), -1);
            assert_eq!(peer_bind_radio(null, peer, null), -1);
            assert_eq!(
                host(|state| state.channels),
                2,
                "binding borrows both handles"
            );
            let reserved = &mut *radio.cast::<ReservedRadio>();
            drop(reserved.radio.take());
            assert_eq!(peer_bind_radio(null, peer, radio), -1);
            peer_destroy(null, peer);
            radio_destroy(null, radio);
        }
        host(|state| state.clean());
    }
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
        host(|state| state.failure = 1);
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), -1);
        assert!(radio.is_null());
        host(|state| state.failure = 0);
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
        assert_eq!(
            radio_activate(null, null, Some(receive), null, Some(transmit), null),
            -1
        );
        assert_eq!(
            radio_activate(null, radio, Some(receive), null, Some(transmit), null),
            0
        );
        radio_destroy(null, radio);
        radio_destroy(null, null);
        host(|state| state.failure = 30);
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
        assert_eq!(
            radio_activate(null, radio, Some(receive), null, Some(transmit), null),
            -1
        );
        radio_destroy(null, radio);
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
