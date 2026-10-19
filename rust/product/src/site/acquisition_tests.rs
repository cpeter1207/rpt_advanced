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
    PROBE.lock().unwrap().failure = false;
    close(&mut service, &mut host);
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
