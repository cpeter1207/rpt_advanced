use super::*;
use crate::{abi, services::HostServices};
use std::cell::Cell;

#[test]
fn prepare_binds_feedback_before_activation_and_keeps_context_through_retirement() {
    thread_local! {
        static PORT: Cell<Option<abi::UrpRadioFeedbackPort>> = const { Cell::new(None) };
        static ACTIVATED: Cell<bool> = const { Cell::new(false) };
        static FAIL: Cell<bool> = const { Cell::new(false) };
        static DESTROYED: Cell<bool> = const { Cell::new(false) };
    }
    unsafe extern "C" fn bind(
        _: *mut c_void,
        _: *mut c_void,
        port: *const abi::UrpRadioFeedbackPort,
    ) -> i32 {
        let port = unsafe { *port };
        assert_eq!(port.struct_size as usize, std::mem::size_of_val(&port));
        assert_eq!(port.abi_version, 1);
        assert!(!port.context.is_null());
        assert!(port.publish.is_some());
        assert!(!ACTIVATED.get());
        PORT.set(Some(port));
        -i32::from(FAIL.get())
    }
    unsafe extern "C" fn activate(
        _: *mut c_void,
        _: *mut c_void,
        _: abi::rptadv_radio_receive_v3,
        _: *mut c_void,
        _: abi::rptadv_radio_transmit_v3,
        context: *mut c_void,
    ) -> i32 {
        let Some(port) = PORT.get() else {
            return -1;
        };
        assert_eq!(port.context, context);
        unsafe { port.publish.unwrap()(context, std::ptr::null()) };
        ACTIVATED.set(true);
        0
    }
    unsafe extern "C" fn destroy(context: *mut c_void, radio: *mut c_void) {
        if let Some(port) = PORT.get() {
            unsafe { port.publish.unwrap()(port.context, std::ptr::null()) };
        }
        DESTROYED.set(true);
        unsafe { (*crate::fixture::host_descriptor()).radio_destroy.unwrap()(context, radio) };
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for failure in [false, true] {
        PORT.set(None);
        ACTIVATED.set(false);
        DESTROYED.set(false);
        FAIL.set(failure);
        let mut table = unsafe { crate::fixture::host_descriptor().read() };
        table.radio_feedback_bind = Some(bind);
        table.radio_activate = Some(activate);
        table.radio_destroy = Some(destroy);
        let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
        let result = RadioWorker::prepare(
            services.radio("1000", "usb", 8).unwrap(),
            Instant::now(),
            RadioStatus::default(),
            0,
            50,
        );
        let worker = if failure {
            let Err(PrepareFailure::Activation(Error::Operation, worker)) = result else {
                panic!("a rejected binding must retain its borrowed callback context");
            };
            assert!(!ACTIVATED.get());
            *worker
        } else {
            assert!(ACTIVATED.get());
            result.unwrap_or_else(|failure| panic!("{failure:?}"))
        };
        assert!(!DESTROYED.get());
        drop(worker.stop());
        assert!(DESTROYED.get());
    }
}

thread_local! {
    static CAPTURED: Cell<Option<abi::UrpRadioFeedbackPort>> = const { Cell::new(None) };
}

fn feedback_worker() -> (RadioWorker, abi::UrpRadioFeedbackPort) {
    unsafe extern "C" fn bind(
        _: *mut c_void,
        _: *mut c_void,
        port: *const abi::UrpRadioFeedbackPort,
    ) -> i32 {
        CAPTURED.set(Some(unsafe { *port }));
        0
    }
    unsafe extern "C" fn activate(
        _: *mut c_void,
        _: *mut c_void,
        _: abi::rptadv_radio_receive_v3,
        _: *mut c_void,
        _: abi::rptadv_radio_transmit_v3,
        _: *mut c_void,
    ) -> i32 {
        0
    }
    CAPTURED.set(None);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_feedback_bind = Some(bind);
    table.radio_activate = Some(activate);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let worker = RadioWorker::prepare(
        services.radio("1000", "usb", 8).unwrap(),
        Instant::now(),
        RadioStatus::default(),
        0,
        0,
    )
    .unwrap_or_else(|failure| panic!("{failure:?}"));
    (worker, CAPTURED.get().expect("prepared feedback callback"))
}

fn physical_feedback(sample: u64) -> abi::UrpRadioFeedback {
    // All fields are integer scalars/records; no pointer or invalid enum is created.
    let mut value: abi::UrpRadioFeedback = unsafe { std::mem::zeroed() };
    value.struct_size = std::mem::size_of_val(&value) as u32;
    value.abi_version = 1;
    value.generation_id = 7;
    value.sample_index = sample;
    value.frame_count = 8;
    value.flags = 63;
    value.request_id = 3;
    value.service_request_id = 3;
    value.readback_request_id = 3;
    value.service_generation = 7;
    value.readback_generation = 7;
    value.acquired_sample_index = sample;
    value
}

fn feedback_status(
    worker: &mut RadioWorker,
    port: abi::UrpRadioFeedbackPort,
    value: Option<&abi::UrpRadioFeedback>,
) -> String {
    if let Some(value) = value {
        unsafe { port.publish.unwrap()(port.context, value) };
    }
    let mut samples = [0.0; 8];
    let mut keyed = 0;
    let mut ctcss = 0;
    assert_eq!(
        unsafe {
            transmit_callback(
                port.context,
                samples.as_mut_ptr(),
                8,
                &mut keyed,
                &mut ctcss,
            )
        },
        0
    );
    assert_eq!(keyed, 0);
    worker.collect_snapshots();
    worker.local_status_text()
}

#[test]
fn callback_consumes_physical_evidence_once_without_conflating_output_service_and_readback() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut worker, port) = feedback_worker();
    assert!(
        worker
            .local_status_text()
            .contains("radio-feedback: unavailable")
    );
    let mut value = physical_feedback(8);
    let report = feedback_status(&mut worker, port, Some(&value));
    assert!(
        report.contains("duplex=half requested=off rendered=off serviced=off observed=off"),
        "{report}"
    );
    assert!(
        report.contains("acquisition-age=0frames state=0"),
        "{report}"
    );
    // No publication for the following callback: cached OFF cannot be renewed.
    assert!(feedback_status(&mut worker, port, None).contains("radio-feedback: unavailable"));
    value.sample_index = 24;
    value.acquired_sample_index = 24;
    value.flags &= !8; // Completed GPIO write is not independent readback.
    let report = feedback_status(&mut worker, port, Some(&value));
    assert!(report.contains("serviced=off observed=unknown"), "{report}");
    value.sample_index = 32;
    value.acquired_sample_index = 32;
    value.flags = 0; // Zero-filled/off defaults carry no evidence.
    let report = feedback_status(&mut worker, port, Some(&value));
    assert!(
        report.contains(
            "duplex=unknown requested=unknown rendered=unknown serviced=unknown observed=unknown"
        ),
        "{report}"
    );
    drop(worker.stop());
}

#[test]
fn callback_rejects_wrong_frame_lifecycle_or_replayed_position() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut worker, port) = feedback_worker();
    let valid = physical_feedback(8);
    assert!(
        !feedback_status(&mut worker, port, Some(&valid)).contains("radio-feedback: unavailable")
    );
    for changed in [0, 1, 2, 3, 4, 5] {
        let mut value = physical_feedback(16);
        match changed {
            0 => value.frame_count = 7,
            1 => value.generation_id = 8,
            2 => value.sample_index = 8,
            3 => value.generation_id = 0,
            4 => value.abi_version = 2,
            _ => value.struct_size = 4,
        }
        assert!(
            feedback_status(&mut worker, port, Some(&value))
                .contains("radio-feedback: unavailable")
        );
    }
    // Invalid publications must not poison the last accepted lifecycle/position.
    let mut value = physical_feedback(16);
    value.physical_duplex = 1;
    value.requested_ptt = 1;
    value.rendered_ptt = 1;
    value.serviced_ptt = 1;
    value.observed_ptt = 1;
    value.transmitter_state = 1;
    let report = feedback_status(&mut worker, port, Some(&value));
    assert!(
        report.contains("duplex=full requested=on rendered=on serviced=on observed=on"),
        "{report}"
    );
    drop(worker.stop());
}

#[test]
fn readback_needs_matching_fresh_acquisition_request_and_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut worker, port) = feedback_worker();
    for changed in 0..11 {
        let mut value = physical_feedback(32 + changed * 8);
        match changed {
            0 => value.readback_request_id = 2,
            1 => value.readback_generation = 6,
            2 => value.flags &= !32,
            3 => value.acquired_sample_index = value.sample_index + 1,
            4 => value.acquisition_age_frames = 1, // Inconsistent evidence.
            5 => {
                value.acquired_sample_index -= 9;
                value.acquisition_age_frames = 9;
            }
            6 => value.request_id = 0,
            7 => value.flags &= !2,
            8 => value.observed_ptt = 2,
            9 => value.requested_ptt = 2,
            _ => value.rendered_ptt = 2,
        }
        let report = feedback_status(&mut worker, port, Some(&value));
        assert!(report.contains("observed=unknown"), "{changed}: {report}");
    }
    let mut value = physical_feedback(128);
    value.acquired_sample_index -= 8;
    value.acquisition_age_frames = 8;
    value.service_request_id = 2;
    let report = feedback_status(&mut worker, port, Some(&value));
    assert!(report.contains("serviced=unknown observed=off"), "{report}");
    assert_eq!(worker.poll_stop(), Ok(true));
    assert!(
        worker
            .local_status_text()
            .contains("radio-feedback: unavailable")
    );
    drop(worker.stop());
}

#[test]
fn physical_prefix_never_requires_or_borrows_programme_tail() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut worker, port) = feedback_worker();
    let mut value = physical_feedback(8);
    let physical_size =
        std::mem::offset_of!(abi::UrpRadioFeedback, transmitter_state) + std::mem::size_of::<i32>();
    assert_eq!(physical_size, 112);
    value.struct_size = physical_size as u32;
    let mut prefix = [0_u64; 14];
    unsafe {
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&value).cast::<u8>(),
            prefix.as_mut_ptr().cast(),
            physical_size,
        );
        port.publish.unwrap()(port.context, prefix.as_ptr().cast());
    }
    let report = feedback_status(&mut worker, port, None);
    assert!(report.contains("observed=off"), "{report}");
    let short = [4_u32];
    unsafe { port.publish.unwrap()(port.context, short.as_ptr().cast()) };
    assert!(feedback_status(&mut worker, port, None).contains("radio-feedback: unavailable"));
    unsafe { port.publish.unwrap()(std::ptr::null_mut(), std::ptr::null()) };
    drop(worker.stop());
}
