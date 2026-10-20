//! Real polling and ownership, with only physical adapter calls replaced by fixtures.
use super::*;
use crate::site::acquisition::Acquisition;
use rpt_advanced_core::{
    config::{ConfigDocument, NodeId, ResolvedSiteSettings},
    runtime::{GenerationSettings, NodeHost, RuntimeGeneration},
    site::ReadingQuality,
};
use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[path = "acquisition_reload_tests.rs"]
mod reload;

pub(super) static IN_CONTROL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn observed_submit(
    context: *mut c_void,
    task: abi::rptadv_control_task_v1,
) -> i32 {
    IN_CONTROL.store(true, Ordering::Release);
    let result = unsafe { (*crate::fixture::control_descriptor()).submit.unwrap()(context, task) };
    IN_CONTROL.store(false, Ordering::Release);
    result
}

#[test]
fn site_engine_retires_external_device_workers_outside_control_even_when_drain_fails() {
    let _lifecycle = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let api = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
    for drain_failure in [false, true] {
        *PROBE.lock().unwrap() = Probe::default();
        crate::lifecycle::SKIP_TICKER_THREAD.store(true, Ordering::Release);
        let configuration = "[1000]\nradio_channel=usb\nduplex=full\n";
        let mut control = unsafe { crate::fixture::control_descriptor().read() };
        control.submit = Some(observed_submit);
        let mut host = unsafe { crate::fixture::host_descriptor().read() };
        host.context = ptr::without_provenance_mut(1);
        assert_eq!(
            unsafe {
                api.start.unwrap()(
                    Box::leak(Box::new(host)),
                    Box::leak(Box::new(control)),
                    rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                    rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                    configuration.as_ptr().cast(),
                    configuration.len(),
                )
            },
            0
        );
        let (engine, call) = crate::lifecycle::selected().unwrap();
        let acquisition = Acquisition::prepare(
            &definitions("[1000]\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\nstartup_state=on\nsafe_state=off\n"),
            backends(), Instant::now(),
        ).unwrap();
        engine
            .run(move |host| {
                host.install_sites(acquisition, crate::site::radio::RadioSite::default());
                Ok(())
            })
            .unwrap();
        until(|| {
            engine
                .run(|host| {
                    host.poll_sites(0);
                    Ok(())
                })
                .unwrap();
            !PROBE.lock().unwrap().written.is_empty()
        });
        drop(call);
        crate::fixture::CONTROL_STOP_RESULT.store(usize::from(drain_failure), Ordering::Release);
        assert_eq!(
            unsafe { api.stop.unwrap()() },
            if drain_failure { -1 } else { 0 }
        );
        assert_eq!(unsafe { api.stop.unwrap()() }, 0);
        let probe = PROBE.lock().unwrap();
        assert_eq!(probe.closed, 1);
        assert!(!probe.closed_on_control);
        assert_eq!(probe.written.last(), Some(&0.0));
    }
}

fn definitions(text: &str) -> BTreeMap<String, ResolvedSiteSettings> {
    let document = ConfigDocument::parse(text).unwrap();
    document
        .nodes()
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                ResolvedSiteSettings::resolve(&document, &NodeId::new(name).unwrap())
                    .unwrap()
                    .value,
            )
        })
        .collect()
}
fn host(node: &str) -> NodeHost<(), ()> {
    NodeHost::new(
        RuntimeGeneration::prepare(
            1,
            GenerationSettings {
                node: node.into(),
                device: "fixture".into(),
                receive_maximum: 960,
                transmit_maximum: 960,
                squelch_delay_ms: 0,
                status_snapshot_interval_ms: 50,
            },
            (),
            (),
        )
        .unwrap(),
    )
}
fn until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < end,
            "site acquisition did not complete fixture work"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn close(acquisition: &mut Acquisition, host: &mut NodeHost<(), ()>) {
    acquisition.pause(true);
    until(|| acquisition.barrier().is_some());
    assert_eq!(acquisition.barrier(), Some(Ok(())));
    assert!(acquisition.finish());
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    assert!(host.reclaim());
}

#[test]
fn site_acquisition_keeps_startup_request_and_separates_submission_from_faults() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let settings = definitions(
        "[1000]\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\nstartup_state=on\n",
    );
    let mut host = host("1000");
    let mut service = Acquisition::prepare(&settings, backends(), Instant::now()).unwrap();
    let pending = service
        .output_status("1000", "relay")
        .expect("configured output");
    assert_eq!(pending.requested, 1.0);
    assert_eq!(pending.submitted, None);
    assert_eq!(pending.quality, ReadingQuality::Unavailable);
    until(|| {
        service.poll(0, |_| host.control().work());
        service
            .output_status("1000", "relay")
            .unwrap()
            .submitted
            .is_some()
    });
    assert_eq!(PROBE.lock().unwrap().written, [1.0]);
    assert!(service.output("1000", "relay", 0.0).is_ok());
    until(|| {
        service.poll(1, |_| host.control().work());
        service
            .output_status("1000", "relay")
            .unwrap()
            .submitted
            .is_some_and(|(v, _)| v == 0.0)
    });
    let submitted = service.output_status("1000", "relay").unwrap().submitted;
    PROBE.lock().unwrap().failure = true;
    assert!(service.output("1000", "relay", 1.0).is_ok());
    until(|| {
        service.poll(2, |_| host.control().work());
        service.output_status("1000", "relay").unwrap().quality == ReadingQuality::Fault
    });
    let failed = service.output_status("1000", "relay").unwrap();
    assert_eq!(failed.requested, 1.0);
    assert_eq!(failed.submitted, submitted);
    assert!(!service.poll(2, |_| host.control().work())["1000"].outputs["relay"].available);
    assert!(service.output("1000", "relay", f64::NAN).is_err());
    assert!(service.output("1000", "relay", 2.0).is_err());
    assert!(service.output("1001", "relay", 0.0).is_err());
    assert!(service.output_status("1001", "relay").is_none());
    assert!(service.output_status("1000", "absent").is_none());
    PROBE.lock().unwrap().failure = false;
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_output_can_share_a_name_with_an_input_or_sensor() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for source in [
        "[input 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=8\n",
        "[sensor 1000 relay]\nenabled=yes\nbackend=hwmon\ndevice=fixture\nchannel=in0_input\n",
    ] {
        *PROBE.lock().unwrap() = Probe::default();
        let settings = definitions(&format!(
            "[1000]\n{source}[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\n"
        ));
        let mut host = host("1000");
        let mut service = Acquisition::prepare(&settings, backends(), Instant::now()).unwrap();
        assert_eq!(service.output("1000", "relay", 1.0), Ok(()));
        until(|| {
            service.poll(0, |_| host.control().work());
            service
                .output_status("1000", "relay")
                .unwrap()
                .submitted
                .is_some()
        });
        assert_eq!(
            service
                .output_status("1000", "relay")
                .unwrap()
                .submitted
                .unwrap()
                .0,
            1.0
        );
        close(&mut service, &mut host);
    }
}

#[test]
fn site_acquisition_slow_node_cannot_stall_or_refresh_another_nodes_measurement() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    workers::BLOCK_GPIO.store(true, Ordering::Release);
    workers::GPIO_ENTERED.store(false, Ordering::Release);
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            workers::BLOCK_GPIO.store(false, Ordering::Release);
        }
    }
    let settings = definitions(
        "[1000]\n[1001]\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\n[sensor 1001 supply]\nenabled=yes\nbackend=hwmon\ndevice=fixture\nchannel=in0_input\n",
    );
    let mut first = host("1000");
    let mut second = host("1001");
    let mut service = Acquisition::prepare(&settings, backends(), Instant::now()).unwrap();
    let release = Release;
    let mut acquire = |name: &str| {
        if name == "1000" {
            first.control().work()
        } else {
            second.control().work()
        }
    };
    service.poll(0, &mut acquire);
    until(|| workers::GPIO_ENTERED.load(Ordering::Acquire));
    let mut received = None;
    until(|| {
        let snapshots = service.poll(1, &mut acquire);
        received = snapshots
            .get("1001")
            .and_then(|v| v.sensors.get("supply"))
            .cloned();
        received
            .as_ref()
            .is_some_and(|v| v.quality == ReadingQuality::Valid)
    });
    let received = received.unwrap();
    assert_eq!(received.value, Some(12.5));
    for _ in 0..100 {
        let snapshots = service.poll(50, &mut acquire);
        assert_eq!(
            snapshots["1001"].sensors["supply"].observed_at_ms,
            received.observed_at_ms
        );
        assert_ne!(
            snapshots["1000"].inputs["door"].quality,
            ReadingQuality::Valid
        );
    }
    assert_eq!(PROBE.lock().unwrap().opened.len(), 2);
    drop(release);
    until(|| {
        service.poll(60, &mut acquire)["1000"].inputs["door"].quality == ReadingQuality::Valid
    });
    close(&mut service, &mut first);
    second.control().stop(0).unwrap();
    second.control().mark_detached(1).unwrap();
    assert!(second.reclaim());
}

#[test]
fn site_acquisition_expired_output_ack_is_not_reported_as_current_availability() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let epoch = Instant::now();
    let mut host = host("1000");
    let settings = definitions(
        "[1000]\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\n",
    );
    let mut service = Acquisition::prepare(&settings, backends(), epoch).unwrap();
    until(|| {
        service.poll(0, |_| host.control().work());
        service
            .output_status("1000", "relay")
            .unwrap()
            .submitted
            .is_some()
    });
    let submitted = service
        .output_status("1000", "relay")
        .unwrap()
        .submitted
        .unwrap();
    let snapshots = service.poll(submitted.1 + 5000, |_| host.control().work());
    assert!(!snapshots["1000"].outputs["relay"].available);
    assert_eq!(
        service.output_status("1000", "relay").unwrap().quality,
        ReadingQuality::Stale
    );
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_sensor_staleness_and_failure_preserve_last_real_reading() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let mut host = host("1000");
    let settings = definitions(
        "[1000]\n[sensor 1000 supply]\nenabled=yes\nbackend=hwmon\ndevice=fixture\nchannel=in0_input\n",
    );
    let mut service = Acquisition::prepare(&settings, backends(), Instant::now()).unwrap();
    let mut valid = None;
    until(|| {
        valid = Some(service.poll(0, |_| host.control().work())["1000"].sensors["supply"].clone());
        valid.as_ref().unwrap().quality == ReadingQuality::Valid
    });
    let valid = valid.unwrap();
    let deadline = valid.observed_at_ms.unwrap() + 5000;
    let stale = service.poll(deadline, |_| None)["1000"].sensors["supply"].clone();
    assert_eq!(stale.quality, ReadingQuality::Stale);
    assert_eq!(stale.value, Some(12.5));
    assert_eq!(stale.observed_at_ms, valid.observed_at_ms);
    PROBE.lock().unwrap().failure = true;
    until(|| {
        let failed =
            service.poll(deadline, |_| host.control().work())["1000"].sensors["supply"].clone();
        assert_eq!(failed.value, valid.value);
        assert_eq!(failed.observed_at_ms, valid.observed_at_ms);
        failed.quality == ReadingQuality::Fault
    });
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_public_definitions_ignore_missing_endpoints_and_fault_wrong_reading_types() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let mut host = host("1000");
    let mut settings = definitions(
        "[1000]\n[sensor 1000 supply]\nenabled=yes\nbackend=hwmon\ndevice=fixture\nchannel=in0_input\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=8\n",
    );
    let node = settings.get_mut("1000").unwrap();
    // These records are public: malformed callers must not fabricate readings.
    let mut sensor = node.sensors[0].clone();
    sensor.name = "missing-sensor".into();
    sensor.endpoint = None;
    node.sensors.push(sensor);
    let mut input = node.inputs[0].clone();
    input.name = "missing-input".into();
    input.endpoint = None;
    node.inputs.push(input);
    let mut output = node.outputs[0].clone();
    output.name = "missing-output".into();
    output.endpoint = None;
    node.outputs.push(output);
    std::mem::swap(&mut node.sensors[0].endpoint, &mut node.inputs[0].endpoint);
    let mut service = Acquisition::prepare(&settings, backends(), Instant::now()).unwrap();
    until(|| {
        let snapshot = service.poll(0, |_| host.control().work());
        let node = &snapshot["1000"];
        assert!(!node.sensors.contains_key("missing-sensor"));
        assert!(!node.inputs.contains_key("missing-input"));
        assert!(!node.outputs.contains_key("missing-output"));
        node.sensors["supply"].quality == ReadingQuality::Fault
            && node.inputs["door"].quality == ReadingQuality::Fault
    });
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_discards_read_completed_after_its_radio_generation_changed() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    workers::BLOCK_GPIO.store(true, Ordering::Release);
    workers::GPIO_ENTERED.store(false, Ordering::Release);
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            workers::BLOCK_GPIO.store(false, Ordering::Release);
        }
    }
    let release = Release;
    let mut host = host("1000");
    let settings = definitions(
        "[1000]\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\n",
    );
    let mut service = Acquisition::prepare(&settings, backends(), Instant::now()).unwrap();
    service.poll(0, |_| host.control().work());
    until(|| workers::GPIO_ENTERED.load(Ordering::Acquire));
    let replacement = RuntimeGeneration::prepare(
        2,
        GenerationSettings {
            node: "1000".into(),
            device: "fixture".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
            squelch_delay_ms: 0,
            status_snapshot_interval_ms: 50,
        },
        (),
        (),
    )
    .unwrap();
    host.control().publish(replacement, 1).unwrap();
    host.control().mark_detached(1).unwrap();
    drop(release);
    until(|| {
        let snapshot = service.poll(1, |_| None);
        assert_eq!(snapshot["1000"].inputs["door"].value, None);
        assert_eq!(
            snapshot["1000"].inputs["door"].quality,
            ReadingQuality::Unavailable
        );
        host.reclaim()
    });
    until(|| {
        service.poll(1, |_| host.control().work())["1000"].inputs["door"].quality
            == ReadingQuality::Valid
    });
    service.pause(true);
    until(|| service.barrier().is_some());
    assert!(service.finish());
    host.control().stop(0).unwrap();
    host.control().mark_detached(2).unwrap();
    assert!(host.reclaim());
}
