//! Reconfiguration exercises the public bounded owner, not a replacement mock.
use super::*;
use std::collections::BTreeSet;

const RELAY: &str =
    "[1000]\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=7\n";

fn begin(
    service: &mut Acquisition,
    definitions: &BTreeMap<String, ResolvedSiteSettings>,
    epoch: Instant,
) -> bool {
    let candidate =
        Acquisition::prepare_reload(definitions, backends(), epoch, service.keys()).unwrap();
    service.begin(candidate, &Default::default()).is_ok()
}

#[test]
fn site_acquisition_reload_preserves_live_output_and_retires_only_after_safe() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let epoch = Instant::now();
    let mut host = host("1000");
    let mut service = Acquisition::prepare(&definitions(RELAY), backends(), epoch).unwrap();
    assert!(!service.stage());
    assert!(service.commit().is_err());
    service.output("1000", "relay", 1.0).unwrap();
    until(|| {
        service.poll(0, |_| host.control().work());
        service
            .output_status("1000", "relay")
            .unwrap()
            .submitted
            .is_some()
    });
    assert_eq!(PROBE.lock().unwrap().written, [1.0]);
    let replacement = definitions(&format!(
        "{RELAY}[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=fixture\nline=8\n[sensor 1000 volts]\nenabled=yes\nbackend=hwmon\ndevice=fixture\nchannel=in0_input\n"
    ));
    assert!(begin(&mut service, &replacement, epoch));
    assert!(!begin(&mut service, &replacement, epoch));
    assert!(!service.resume());
    until(|| service.barrier().is_some());
    assert!(service.commit().is_err());
    assert!(service.stage());
    until(|| service.barrier().is_some());
    assert!(!service.stage());
    assert!(service.commit().unwrap().is_empty());
    until(|| service.resume());
    until(|| {
        let snapshot = service.poll(1, |_| host.control().work());
        snapshot["1000"].sensors["volts"].quality == ReadingQuality::Valid
            && snapshot["1000"].inputs["door"].quality == ReadingQuality::Valid
    });
    assert_eq!(
        service.output_status("1000", "relay").unwrap().requested,
        1.0
    );
    assert!(PROBE.lock().unwrap().written.iter().all(|v| *v == 1.0));
    assert_eq!(PROBE.lock().unwrap().opened.len(), 3);
    assert_eq!(PROBE.lock().unwrap().closed, 0);

    assert!(begin(&mut service, &BTreeMap::new(), epoch));
    until(|| service.barrier().is_some());
    assert_eq!(PROBE.lock().unwrap().written.last(), Some(&0.0));
    assert_eq!(PROBE.lock().unwrap().closed, 0);
    assert!(service.stage());
    let retired = service.commit().unwrap();
    assert_eq!(retired.len(), 2);
    for mut worker in retired {
        assert!(worker.finish());
    }
    assert!(service.resume());
    assert!(service.poll(2, |_| host.control().work()).is_empty());
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_reload_safe_failure_retains_owner_and_resumes_old_demand() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let epoch = Instant::now();
    let mut host = host("1000");
    let mut service = Acquisition::prepare(&definitions(RELAY), backends(), epoch).unwrap();
    service.output("1000", "relay", 1.0).unwrap();
    until(|| {
        service.poll(0, |_| host.control().work());
        service
            .output_status("1000", "relay")
            .unwrap()
            .submitted
            .is_some()
    });
    PROBE.lock().unwrap().failure = true;
    assert!(begin(&mut service, &BTreeMap::new(), epoch));
    until(|| service.barrier().is_some());
    assert_eq!(service.barrier(), Some(Err(ReadingQuality::Fault)));
    assert!(!service.stage());
    assert!(!service.finish());
    assert!(service.commit().is_err());
    assert_eq!(PROBE.lock().unwrap().closed, 0);
    drop(service.abort());
    PROBE.lock().unwrap().failure = false;
    until(|| service.resume());
    until(|| {
        service.poll(1, |_| host.control().work());
        PROBE.lock().unwrap().written.len() == 2
    });
    assert_eq!(PROBE.lock().unwrap().written, [1.0, 1.0]);
    assert_eq!(PROBE.lock().unwrap().opened.len(), 1);
    assert_eq!(
        service.output_status("1000", "relay").unwrap().requested,
        1.0
    );
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_stale_preparation_is_rejected_without_opening_another_owner() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let epoch = Instant::now();
    let mut host = host("1000");
    let definitions = definitions(RELAY);
    let mut service = Acquisition::prepare(&definitions, backends(), epoch).unwrap();
    let stale =
        Acquisition::prepare_reload(&definitions, backends(), epoch, Default::default()).unwrap();
    let rejected = service
        .begin(stale, &Default::default())
        .expect_err("owner list changed");
    drop(rejected); // Idle candidate threads retire on this caller, never station control.
    assert!(PROBE.lock().unwrap().opened.is_empty());
    service.pause(false);
    assert!(!begin(&mut service, &definitions, epoch));
    close(&mut service, &mut host);
}

#[test]
fn site_acquisition_unchanged_owner_retains_demand_across_radio_generation_handoff() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for replaced in [BTreeSet::new(), ["1000".into()].into_iter().collect()] {
        *PROBE.lock().unwrap() = Probe::default();
        let epoch = Instant::now();
        let mut host = host("1000");
        let settings = definitions(RELAY);
        let mut service = Acquisition::prepare(&settings, backends(), epoch).unwrap();
        service.output("1000", "relay", 1.0).unwrap();
        until(|| {
            service.poll(0, |_| host.control().work());
            service
                .output_status("1000", "relay")
                .unwrap()
                .submitted
                .is_some()
        });
        let candidate =
            Acquisition::prepare_reload(&settings, backends(), epoch, service.keys()).unwrap();
        assert!(service.begin(candidate, &replaced).is_ok());
        until(|| service.barrier().is_some());
        assert!(service.stage());
        until(|| service.barrier().is_some());
        assert!(service.commit().unwrap().is_empty());
        until(|| service.resume());
        service.poll(1, |_| host.control().work());
        assert_eq!(
            service.output_status("1000", "relay").unwrap().requested,
            1.0
        );
        assert_eq!(PROBE.lock().unwrap().opened.len(), 1);
        assert_eq!(PROBE.lock().unwrap().written, [1.0]);
        close(&mut service, &mut host);
    }
}

#[test]
fn site_acquisition_reassigned_node_or_pin_starts_with_its_own_request() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for move_node in [true, false] {
        *PROBE.lock().unwrap() = Probe::default();
        let epoch = Instant::now();
        let mut host = host("1000");
        let mut settings = definitions(RELAY);
        let mut service = Acquisition::prepare(&settings, backends(), epoch).unwrap();
        service.output("1000", "relay", 1.0).unwrap();
        until(|| {
            service.poll(0, |_| host.control().work());
            service
                .output_status("1000", "relay")
                .unwrap()
                .submitted
                .is_some()
        });
        let node = if move_node {
            let moved = settings.remove("1000").unwrap();
            settings.insert("2000".into(), moved);
            "2000"
        } else {
            settings.get_mut("1000").unwrap().outputs[0]
                .endpoint
                .as_mut()
                .unwrap()
                .channel = "8".into();
            "1000"
        };
        assert!(begin(&mut service, &settings, epoch));
        until(|| service.barrier().is_some());
        assert!(service.stage());
        until(|| service.barrier().is_some());
        assert!(service.commit().unwrap().is_empty());
        let current = service.output_status(node, "relay").unwrap();
        assert_eq!(current.requested, 0.0);
        assert_eq!(current.submitted, None);
        until(|| service.resume());
        until(|| {
            service.poll(1, |_| host.control().work());
            service
                .output_status(node, "relay")
                .unwrap()
                .submitted
                .is_some()
        });
        assert_eq!(PROBE.lock().unwrap().written.last(), Some(&0.0));
        close(&mut service, &mut host);
    }
}

#[test]
fn site_acquisition_reload_metadata_preserves_demand_and_physical_ownership() {
    use rpt_advanced_core::site::{OutputRequest, OutputState};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for edit in ["startup_state=on\n", "safe_state=on\n"] {
        *PROBE.lock().unwrap() = Probe::default();
        let epoch = Instant::now();
        let mut host = host("1000");
        let mut service = Acquisition::prepare(&definitions(RELAY), backends(), epoch).unwrap();
        let mut pulse = OutputState::new(definitions(RELAY)["1000"].outputs[0].clone());
        pulse.advance(0, true);
        pulse.command(OutputRequest::Pulse, 0).unwrap();
        assert_eq!(pulse.snapshot().pulse_deadline_ms, Some(500));
        service.output("1000", "relay", 1.0).unwrap();
        until(|| {
            service.poll(0, |_| host.control().work());
            service
                .output_status("1000", "relay")
                .unwrap()
                .submitted
                .is_some()
        });
        let before = service.output_status("1000", "relay").unwrap();
        assert!(begin(
            &mut service,
            &definitions(&format!("{RELAY}{edit}")),
            epoch
        ));
        until(|| service.barrier().is_some());
        assert!(service.stage());
        until(|| service.barrier().is_some());
        assert!(service.commit().unwrap().is_empty());
        until(|| service.resume());
        let after = service.output_status("1000", "relay").unwrap();
        assert_eq!(after.requested, before.requested);
        assert_eq!(after.submitted, before.submitted);
        assert_eq!(after.quality, ReadingQuality::Valid);
        let observations = service.poll(1, |_| host.control().work());
        pulse.reconfigure(definitions(&format!("{RELAY}{edit}"))["1000"].outputs[0].clone());
        pulse.advance(1, observations["1000"].outputs["relay"].available);
        assert_eq!(pulse.snapshot().pulse_deadline_ms, Some(500));
        pulse.advance(500, observations["1000"].outputs["relay"].available);
        assert!(!pulse.snapshot().requested_on);
        assert_eq!(pulse.snapshot().pulse_deadline_ms, None);
        {
            let probe = PROBE.lock().unwrap();
            assert_eq!(probe.opened.len(), 1);
            assert_eq!(probe.closed, 0);
            assert_eq!(probe.written, [1.0]);
        }
        close(&mut service, &mut host);
        assert_eq!(
            PROBE.lock().unwrap().written.last(),
            Some(&f64::from(edit.starts_with("safe")))
        );
    }
}

#[test]
fn site_acquisition_aborted_metadata_change_retains_old_retirement_level() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let epoch = Instant::now();
    let mut host = host("1000");
    let mut service = Acquisition::prepare(&definitions(RELAY), backends(), epoch).unwrap();
    until(|| {
        service.poll(0, |_| host.control().work());
        service
            .output_status("1000", "relay")
            .unwrap()
            .submitted
            .is_some()
    });
    assert!(begin(
        &mut service,
        &definitions(&format!("{RELAY}safe_state=on\n")),
        epoch
    ));
    until(|| service.barrier().is_some());
    assert!(service.stage());
    until(|| service.barrier().is_some());
    drop(service.abort());
    until(|| service.resume());
    assert!(PROBE.lock().unwrap().safety.is_empty());
    close(&mut service, &mut host);
    assert_eq!(PROBE.lock().unwrap().written.last(), Some(&0.0));
}
