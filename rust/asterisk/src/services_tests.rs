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
const REMOTE_PENDING: u8 = 50;
const REMOTE_ACK_MISSING: u8 = 51;
const REMOTE_FAILED: u8 = 52;
const FEEDBACK_ACK_MISSING: u8 = 81;
const FEEDBACK_ACK_WRONG: u8 = 82;
const FEEDBACK_OPTION_FAILED: u8 = 83;

#[test]
fn feedback_port_requires_stopped_radio_and_acknowledged_channel_binding() {
    unsafe extern "C" fn publish(_: *mut c_void, _: *const ffi::UrpRadioFeedback) {}
    reset();
    let null = ptr::null_mut();
    let mut context = 7_u64;
    let port = ffi::UrpRadioFeedbackPort {
        struct_size: size_of::<ffi::UrpRadioFeedbackPort>() as u32,
        abi_version: 1,
        context: ptr::from_mut(&mut context).cast(),
        publish: Some(publish),
    };
    unsafe {
        assert_eq!(radio_feedback_bind(null, null, &port), -1);
        let mut empty = null;
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut empty), 0);
        drop((*empty.cast::<ReservedRadio>()).radio.take());
        assert_eq!(radio_feedback_bind(null, empty, &port), -1);
        radio_destroy(null, empty);
        for (failure, expected) in [
            (FEEDBACK_ACK_MISSING, -1),
            (FEEDBACK_ACK_WRONG, -1),
            (FEEDBACK_OPTION_FAILED, -1),
            (0, 0),
        ] {
            let mut radio = null;
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
            assert_eq!(radio_feedback_bind(null, radio, ptr::null()), -1);
            let short = [8_u32, 1];
            assert_eq!(radio_feedback_bind(null, radio, short.as_ptr().cast()), -1);
            let wrong_version = ffi::UrpRadioFeedbackPort {
                abi_version: 2,
                ..port
            };
            assert_eq!(radio_feedback_bind(null, radio, &wrong_version), -1);
            assert_eq!(radio_feedback_bind(null, radio, &port), 0);
            assert_eq!(
                (*radio.cast::<ReservedRadio>()).feedback.unwrap().context,
                port.context
            );
            let unbind = ffi::UrpRadioFeedbackPort {
                publish: None,
                ..port
            };
            assert_eq!(radio_feedback_bind(null, radio, &unbind), 0);
            assert!((*radio.cast::<ReservedRadio>()).feedback.is_none());
            assert_eq!(radio_feedback_bind(null, radio, &port), 0);
            host(|state| state.failure = failure);
            assert_eq!(
                radio_activate(null, radio, Some(receive), null, Some(transmit), null),
                expected
            );
            assert_eq!(radio_feedback_bind(null, radio, &unbind), -1);
            assert_eq!(
                radio_activate(null, radio, Some(receive), null, Some(transmit), null),
                -1
            );
            radio_destroy(null, radio);
            host(|state| state.failure = 0);
            host(|state| state.clean());
        }
    }
}

#[test]
fn remote_host_bridge_preserves_pending_owner_and_requires_acknowledgment() {
    reset();
    let null = ptr::null_mut();
    // SAFETY: the fixture retains this reservation through every serialized call.
    unsafe {
        assert_eq!(radio_metadata_bind(null, null, None), -1);
        assert_eq!(radio_service(null, null), -1);
        assert_eq!(radio_ready(null, null), -1);
        assert_eq!(radio_quiesce(null, null), -1);
        let mut radio = null;
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
        assert_eq!(radio_service(null, radio), 0);
        host(|state| state.failure = REMOTE_PENDING);
        assert_eq!(radio_ready(null, radio), 1);
        assert_eq!(radio_quiesce(null, radio), 1);
        assert_eq!(host(|state| state.channels), 1);
        host(|state| state.failure = REMOTE_FAILED);
        assert_eq!(radio_ready(null, radio), -123);
        host(|state| state.failure = REMOTE_ACK_MISSING);
        assert_eq!(radio_ready(null, radio), -7);
        assert_eq!(radio_quiesce(null, radio), -7);
        assert_eq!(host(|state| state.channels), 1);
        host(|state| state.failure = 0);
        assert_eq!(radio_ready(null, radio), 0);
        assert_eq!(radio_quiesce(null, radio), 0);
        assert_eq!(host(|state| state.channels), 1);
        drop((*radio.cast::<ReservedRadio>()).radio.take());
        assert_eq!(radio_metadata_bind(null, radio, None), -1);
        assert_eq!(radio_service(null, radio), -7);
        assert_eq!(radio_ready(null, radio), -7);
        assert_eq!(radio_quiesce(null, radio), 0);
        radio_destroy(null, radio);
    }
    host(|state| state.clean());
}

#[test]
fn remote_submit_validates_borrows_and_transfers_the_owned_ticket() {
    reset();
    let null = ptr::null_mut();
    // SAFETY: the fixture retains complete input/output records and its unique reservation.
    unsafe {
        let mut radio = null;
        assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
        let operation = ffi::UrpRemoteOperation {
            struct_size: size_of::<ffi::UrpRemoteOperation>() as u32,
            abi_version: 1,
            operation: 9,
            selector: 7,
            signed_value: -12,
            magnitude: 145_500_000,
            decimal_places: 1,
            flags: 4,
        };
        let mut output: ffi::rptadv_remote_ticket_v1 = std::mem::zeroed();
        assert_eq!(
            radio_remote_submit(null, radio, &operation, 3, ptr::null_mut()),
            -1
        );
        output.context = ptr::dangling_mut();
        assert_eq!(
            radio_remote_submit(null, radio, ptr::null(), 3, &mut output),
            -1
        );
        assert_eq!(output.struct_size, 0);
        assert!(output.context.is_null());
        assert_eq!(
            radio_remote_submit(null, null, &operation, 3, &mut output),
            -1
        );
        host(|state| state.failure = REMOTE_FAILED);
        assert_eq!(
            radio_remote_submit(null, radio, &operation, 3, &mut output),
            -123
        );
        assert!(output.context.is_null());
        assert!(output.source.is_none() && output.take.is_none() && output.destroy.is_none());
        host(|state| state.failure = 0);
        assert_eq!(
            radio_remote_submit(null, radio, &operation, 3, &mut output),
            0
        );
        assert_eq!(output.struct_size as usize, size_of_val(&output));
        assert_eq!(output.abi_version, 1);
        assert!(!output.context.is_null());
        assert_eq!(output.source.unwrap()(output.context, 3), 0);
        assert_eq!(*output.context.cast::<u32>(), 3);
        assert_eq!(output.take.unwrap()(output.context, None, null), 1);
        output.destroy.unwrap()(output.context);
        radio_destroy(null, radio);
    }
    host(|state| state.clean());
}

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
    if matches!(unsafe { std::ffi::CStr::from_ptr(function) }, name if name == c"scheduler" || name == c"echolink")
    {
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
fn echo_notice_uses_error_logger_and_never_a_foreign_format_string() {
    SCHEDULER_LOGS.with(|logs| logs.borrow_mut().clear());
    echo_notice("echo account failure 50% %n");
    echo_notice("invalid\0notice");
    SCHEDULER_LOGS.with(|logs| {
        assert_eq!(
            *logs.borrow(),
            [(
                4,
                "rpt_advanced: EchoLink: %s\n".into(),
                "echo account failure 50% %n".into()
            )]
        )
    });
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
unsafe extern "C" fn receive(
    _: *mut c_void,
    _: *const ffi::UrpNativeReceiveMetadata,
    _: *mut f32,
    _: u32,
) -> i32 {
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

unsafe extern "C" fn remote_ticket_source(context: *mut c_void, flags: u32) -> i32 {
    // SAFETY: the fixture transfers this unique boxed ticket through its matching destroy.
    unsafe { *context.cast::<u32>() = flags };
    0
}

unsafe extern "C" fn remote_ticket_take(
    _: *mut c_void,
    _: ffi::UrpNativeProcessingDiagnostic,
    _: *mut c_void,
) -> i32 {
    1
}

unsafe extern "C" fn remote_ticket_destroy(context: *mut c_void) {
    // SAFETY: the receiving test owns this ticket and destroys it exactly once.
    unsafe { drop(Box::from_raw(context.cast::<u32>())) };
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
    if option == ffi::URP_AST_OPTION_RADIO_FEEDBACK as i32 {
        assert_eq!(length as usize, size_of::<ffi::UrpAstFeedbackBinding>());
        let binding = unsafe { &mut *data.cast::<ffi::UrpAstFeedbackBinding>() };
        assert_eq!(binding.abi_version, 1);
        assert_eq!(binding.reserved, 0);
        assert_eq!(binding.accepted_abi_version, 0);
        assert!(binding.port.publish.is_some());
        let failure = host(|state| state.failure);
        binding.accepted_abi_version = match failure {
            FEEDBACK_ACK_MISSING => 0,
            FEEDBACK_ACK_WRONG => 2,
            _ => 1,
        };
        return if failure == FEEDBACK_OPTION_FAILED {
            -9
        } else {
            0
        };
    }
    if option == ffi::URP_AST_OPTION_RECEIVE_POLICY as i32 {
        assert_eq!(length as usize, size_of::<ffi::UrpAstChannelCommand>());
        // SAFETY: the typed bridge retains one complete writable command.
        let command = unsafe { &mut *data.cast::<ffi::UrpAstChannelCommand>() };
        assert_eq!(command.value, -1);
        assert_eq!(command.flags, u32::MAX);
        let failure = host(|state| state.failure);
        if failure == 50 {
            return 0;
        }
        if failure == 53 {
            return -4;
        }
        command.value = match failure {
            54 => 1,
            55 => 2,
            51 => 9,
            _ => 0,
        };
        command.flags = if matches!(failure, 54 | 55) { 7680 } else { 0 };
        if failure == 52 {
            command.value = 2;
        }
        if failure == 56 {
            command.abi_version = 2;
        }
        if failure == 57 {
            command.struct_size = 4;
        }
        if failure == 58 {
            command.command = 1;
        }
        if failure == 59 {
            command.target = 1;
        }
        return 0;
    }
    if option == ffi::URP_AST_OPTION_REMOTE_CONTROL as i32 {
        assert_eq!(length as usize, size_of::<ffi::UrpAstRemoteControl>());
        // SAFETY: this fixture is passed a complete exclusive control envelope.
        let request = unsafe { &mut *data.cast::<ffi::UrpAstRemoteControl>() };
        assert_eq!(request.abi_version, 1);
        let failure = host(|state| state.failure);
        request.accepted_abi_version = u32::from(failure != REMOTE_ACK_MISSING);
        return match request.operation {
            ffi::URP_AST_REMOTE_SERVICE => 0,
            ffi::URP_AST_REMOTE_QUIESCE => i32::from(failure == REMOTE_PENDING),
            ffi::URP_AST_REMOTE_READY => {
                if failure == REMOTE_FAILED {
                    -123
                } else {
                    i32::from(failure == REMOTE_PENDING)
                }
            }
            ffi::URP_AST_REMOTE_SUBMIT => {
                assert_eq!(request.source_flags, 3);
                assert_eq!(request.request.operation, 9);
                assert_eq!(request.request.selector, 7);
                assert_eq!(request.request.signed_value, -12);
                assert_eq!(request.request.magnitude, 145_500_000);
                assert_eq!(request.request.decimal_places, 1);
                assert_eq!(request.request.flags, 4);
                if failure == REMOTE_FAILED {
                    return -123;
                }
                request.ticket = ffi::UrpRemoteTicket {
                    struct_size: size_of::<ffi::UrpRemoteTicket>() as u32,
                    abi_version: 1,
                    context: Box::into_raw(Box::new(0_u32)).cast(),
                    source: Some(remote_ticket_source),
                    take: Some(remote_ticket_take),
                    destroy: Some(remote_ticket_destroy),
                };
                0
            }
            _ => -7,
        };
    }
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
    assert_eq!(length as usize, size_of::<ffi::UrpAstDirectCallbacks>());
    // SAFETY: attach_direct supplies exclusive access to this complete live descriptor.
    let descriptor = unsafe { &mut *data.cast::<ffi::UrpAstDirectCallbacks>() };
    assert!(descriptor.receive.is_some() && descriptor.transmit.is_some());
    assert_eq!(
        descriptor.abi_version,
        ffi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION
    );
    assert_eq!(descriptor.accepted_abi_version, 0);
    let failure = host(|state| state.failure);
    descriptor.accepted_abi_version = match failure {
        DIRECT_ACK_MISSING => 0,
        DIRECT_ACK_WRONG => 1,
        _ => ffi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION,
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
                retained_channels, 1,
                "failed attachment retains its callback owner until quiesced/destroyed"
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
    assert_eq!(descriptor().abi_version, 10);
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
            let peer = into_raw(peer, "1000", "2000");
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
        let peer = into_raw(peer, "1000", "2000");
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

#[test]
fn receive_policy_bridge_works_before_call_and_requires_a_valid_query_reply() {
    for failure in [0, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59] {
        reset();
        let null = ptr::null_mut();
        let mut radio = null;
        // SAFETY: this test retains all scalar records and the unique reservation.
        unsafe {
            assert_eq!(radio_open(null, c"usb".as_ptr(), 3, 8, &mut radio), 0);
            host(|state| state.failure = failure);
            let mut request: ffi::rptadv_radio_control_v1 = std::mem::zeroed();
            request.struct_size = size_of_val(&request) as u32;
            request.abi_version = 1;
            request.operation = 7;
            request.command.struct_size = size_of_val(&request.command) as u32;
            request.command.abi_version = 1;
            request.command.command = ffi::URP_AST_COMMAND_GET_RECEIVE_POLICY;
            let expected = match failure {
                0 | 54 | 55 => 0,
                53 => -4,
                _ => -1,
            };
            assert_eq!(
                radio_control(null, radio, &mut request, None, null),
                expected
            );
            assert_eq!(host(|state| state.channels), 1);
            if expected == 0 {
                assert_eq!(
                    (request.command.value, request.command.flags),
                    match failure {
                        54 => (1, 7680),
                        55 => (2, 7680),
                        _ => (0, 0),
                    }
                );
            }
            request.operation = 1;
            assert_eq!(radio_control(null, radio, &mut request, None, null), -7);
            request.operation = 7;
            request.command.target = 1;
            assert_eq!(radio_control(null, radio, &mut request, None, null), -1);
            request.command.target = 0;
            request.abi_version = 2;
            assert_eq!(radio_control(null, radio, &mut request, None, null), -1);
            request.abi_version = 1;
            assert_eq!(radio_control(null, null, &mut request, None, null), -1);
            assert_eq!(radio_control(null, radio, ptr::null_mut(), None, null), -1);
            request.struct_size = 4;
            assert_eq!(radio_control(null, radio, &mut request, None, null), -1);
            request.struct_size = size_of_val(&request) as u32;
            for defect in 0..3 {
                request.command.struct_size = size_of_val(&request.command) as u32;
                request.command.abi_version = 1;
                request.command.command = ffi::URP_AST_COMMAND_GET_RECEIVE_POLICY;
                match defect {
                    0 => request.command.struct_size = 4,
                    1 => request.command.abi_version = 2,
                    _ => request.command.command = 1,
                }
                assert_eq!(radio_control(null, radio, &mut request, None, null), -1);
            }
            request.command.struct_size = size_of_val(&request.command) as u32;
            request.command.abi_version = 1;
            request.command.command = ffi::URP_AST_COMMAND_GET_RECEIVE_POLICY;
            drop((*radio.cast::<ReservedRadio>()).radio.take());
            assert_eq!(radio_control(null, radio, &mut request, None, null), -1);
            radio_destroy(null, radio);
        }
        host(|state| state.clean());
    }
}
