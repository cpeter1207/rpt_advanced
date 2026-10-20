//! Reconfiguration exercises the public bounded owner, not a replacement mock.
use super::*;

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
    until(|| service.barrier().is_some());
    assert!(service.stage());
    until(|| service.barrier().is_some());
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
    close(&mut service, &mut host);
}
