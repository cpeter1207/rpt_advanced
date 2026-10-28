use super::*;
use crate::fixture::{host, reset};
use std::mem::size_of_val;

const DIRECT_ACK_MISSING: u8 = 41;
const DIRECT_ACK_WRONG: u8 = 42;
const DIRECT_OPTION_FAILED: u8 = 43;
const LINK_ACK_MISSING: u8 = 44;
const LINK_ACK_WRONG: u8 = 45;
const LINK_OPTION_FAILED: u8 = 46;
const SITE_ACK_MISSING: u8 = 47;
const SITE_ACK_WRONG: u8 = 48;
const SITE_OPTION_FAILED: u8 = 49;

#[test]
fn site_bridge_forwards_on_the_reserved_channel_and_requires_acknowledgment() {
    for failure in [0, SITE_ACK_MISSING, SITE_ACK_WRONG, SITE_OPTION_FAILED] {
        reset();
        let null = ptr::null_mut();
        // SAFETY: each record is borrowed synchronously; the sole channel owner is retained.
        unsafe {
            let mut radio = null;
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
            host(|state| state.failure = failure);
            let binding = ffi::UrpAstSiteBinding {
                struct_size: size_of::<ffi::UrpAstSiteBinding>() as u32,
                abi_version: 1,
                generation: 10,
                output_mask: 4,
                initial_high_mask: 4,
                safe_high_mask: 0,
            };
            let request = ffi::UrpAstSiteRequest {
                struct_size: size_of::<ffi::UrpAstSiteRequest>() as u32,
                abi_version: 1,
                generation: 11,
                output_mask: 4,
                high_mask: 4,
            };
            let mut snapshot: ffi::UrpAstSiteSnapshot = std::mem::zeroed();
            snapshot.struct_size = size_of_val(&snapshot) as u32;
            snapshot.abi_version = 1;
            snapshot.generation = 123;
            let expected = match failure {
                0 => 0,
                SITE_OPTION_FAILED => -4,
                _ => -1,
            };
            assert_eq!(radio_site_bind(null, radio, 1, &binding), expected);
            assert_eq!(radio_site_publish(null, radio, 1, &request), expected);
            assert_eq!(radio_site_snapshot(null, radio, 1, &mut snapshot), expected);
            assert_eq!(snapshot.generation, if failure == 0 { 11 } else { 123 });
            assert_eq!(snapshot.readback_valid_mask, 0);
            assert_eq!(
                host(|state| state.channels),
                1,
                "site I/O must borrow the radio channel"
            );
            assert_eq!(radio_site_bind(null, null, 1, &binding), -1);
            assert_eq!(radio_site_bind(null, radio, 1, ptr::null()), -1);
            assert_eq!(radio_site_publish(null, null, 1, &request), -1);
            assert_eq!(radio_site_publish(null, radio, 1, ptr::null()), -1);
            assert_eq!(radio_site_snapshot(null, null, 1, &mut snapshot), -1);
            assert_eq!(radio_site_snapshot(null, radio, 1, ptr::null_mut()), -1);
            drop((*radio.cast::<ReservedRadio>()).radio.take());
            assert_eq!(radio_site_bind(null, radio, 1, &binding), -1);
            assert_eq!(radio_site_publish(null, radio, 1, &request), -1);
            assert_eq!(radio_site_snapshot(null, radio, 1, &mut snapshot), -1);
            radio_destroy(null, radio);
        }
        host(|state| state.clean());
    }
}

#[unsafe(no_mangle)]
extern "C" fn ast_replace_sigchld() {}
#[unsafe(no_mangle)]
extern "C" fn ast_unreplace_sigchld() {}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_log(
    level: i32,
    _: *const c_char,
    _: i32,
    function: *const c_char,
    format: *const c_char,
    text: *const c_char,
) {
    if unsafe { std::ffi::CStr::from_ptr(function) } == c"scheduler" {
        SCHEDULER_LOGS.with(|logs| {
            logs.borrow_mut().push((
                level,
                unsafe { std::ffi::CStr::from_ptr(format) }
                    .to_str()
                    .unwrap()
                    .to_owned(),
                unsafe { std::ffi::CStr::from_ptr(text) }
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ));
        });
    }
}

thread_local! {
    static SCHEDULER_LOGS: std::cell::RefCell<Vec<(i32, String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[test]
fn scheduler_notice_uses_warning_logger_with_text_as_data() {
    SCHEDULER_LOGS.with(|logs| logs.borrow_mut().clear());
    let notice = descriptor().scheduler_notice.unwrap();
    let message = "scheduler queue 50% — Δ;ignored";
    unsafe {
        notice(ptr::null_mut(), message.as_ptr().cast(), 26);
        for (text, length) in [
            (ptr::null(), 1),
            (c"\xff".as_ptr(), 1),
            (c"x".as_ptr(), usize::MAX),
            (c"x".as_ptr(), 2),
        ] {
            notice(ptr::null_mut(), text, length);
        }
    }
    SCHEDULER_LOGS.with(|logs| {
        assert_eq!(
            *logs.borrow(),
            [(
                3,
                "rpt_advanced: %s\n".into(),
                "scheduler queue 50% — Δ".into()
            )]
        );
    });
}

unsafe extern "C" fn current(_: *mut c_void) -> u32 {
    1
}
unsafe extern "C" fn receive(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
    0
}
unsafe extern "C" fn transmit(
    _: *mut c_void,
    _: *mut f32,
    _: u32,
    _: *mut u32,
    _: *mut u32,
) -> i32 {
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
    if option == 0x52505349 {
        assert_eq!(length as usize, size_of::<ffi::UrpAstSiteControl>());
        let site = unsafe { &mut *data.cast::<ffi::UrpAstSiteControl>() };
        assert_eq!(site.struct_size as usize, size_of_val(site));
        assert_eq!(site.abi_version, 1);
        assert_eq!(site.accepted_abi_version, 0);
        assert_eq!(site.bank, 1);
        assert_eq!(channel, host(|state| state.token(0)));
        match site.operation {
            0 => assert_eq!(
                (
                    site.binding.generation,
                    site.binding.output_mask,
                    site.binding.initial_high_mask,
                    site.binding.safe_high_mask
                ),
                (10, 4, 4, 0)
            ),
            1 => assert_eq!(
                (
                    site.request.generation,
                    site.request.output_mask,
                    site.request.high_mask
                ),
                (11, 4, 4)
            ),
            2 => {
                site.snapshot.generation = 11;
                site.snapshot.readback_valid_mask = 0;
            }
            _ => panic!("unknown site operation"),
        }
        let failure = host(|state| state.failure);
        site.accepted_abi_version = match failure {
            SITE_ACK_MISSING => 0,
            SITE_ACK_WRONG => 2,
            _ => 1,
        };
        return if failure == SITE_OPTION_FAILED { -4 } else { 0 };
    }
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
    assert_eq!(descriptor.abi_version, 3);
    assert_eq!(descriptor.accepted_abi_version, 0);
    let failure = host(|state| state.failure);
    descriptor.accepted_abi_version = match failure {
        DIRECT_ACK_MISSING => 0,
        DIRECT_ACK_WRONG => 1,
        _ => 3,
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
        let identity = b"1000\0usb";
        assert_eq!(
            radio_open(
                null,
                identity.as_ptr().cast(),
                identity.len(),
                8,
                &mut radio
            ),
            0
        );
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
            Some(transmit as unsafe extern "C" fn(_, _, _, _, _) -> _),
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
    assert_eq!(descriptor().abi_version, 8);
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
fn destroy_raw_releases_a_peer_retained_by_the_adapter() {
    reset();
    // SAFETY: the fixture owns the fake channel, and the raw peer is destroyed once.
    unsafe {
        let peer = PeerIo::dial(c"radio@host/200", c"100", 8, || true).unwrap();
        let peer = into_raw(peer);
        destroy_raw(peer);
    }
    host(|state| state.clean());
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
        for identity in [
            b"\0usb".as_slice(),
            b"node\0".as_slice(),
            b"node\0usb\0other",
        ] {
            assert_eq!(
                radio_open(
                    null,
                    identity.as_ptr().cast(),
                    identity.len(),
                    8,
                    &mut radio
                ),
                -1
            );
            assert!(radio.is_null());
        }
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
