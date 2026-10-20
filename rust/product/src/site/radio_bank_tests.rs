//! Cached radio owner snapshots distinguish acceptance, service and electrical readback.
use super::*;
use rpt_advanced_core::config::{ConfigDocument, NodeId, ResolvedSiteSettings};
use std::collections::BTreeMap;

#[path = "radio_reload_tests.rs"]
mod reload;

fn settings() -> BTreeMap<String, ResolvedSiteSettings> {
    let document = ConfigDocument::parse("[1000]\n[2000]\n[input 2000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=2\nactive_level=low\n[output 2000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=7\nactive_level=low\nstartup_state=on\n").unwrap();
    document
        .nodes()
        .into_iter()
        .map(|name| {
            (
                name.into(),
                ResolvedSiteSettings::resolve(&document, &NodeId::new(name).unwrap())
                    .unwrap()
                    .value,
            )
        })
        .collect()
}

#[test]
fn site_radio_bank_borrows_owner_and_requires_serviced_state_before_claiming_submission() {
    let mut site = RadioSite::prepare(&settings()).unwrap();
    let mut physical = snapshot();
    let mut commands = Vec::new();
    let observations = site.poll(2000, |owner, bank, operation| {
        assert_eq!((owner, bank), ("1000", 0));
        if let Operation::Snapshot = operation {
            return Ok(Some(physical));
        }
        commands.push(operation);
        Ok(None)
    });
    assert_eq!(observations["2000"].inputs["door"].value, Some(false));
    assert_eq!(
        observations["2000"].inputs["door"].observed_at_ms,
        Some(1950)
    );
    let [Operation::Bind(binding)] = &commands[..] else {
        panic!("one aggregated bank binding");
    };
    assert_eq!(binding.generation, 7);
    assert_eq!(binding.output_mask, 0x40);
    assert_eq!(binding.initial_high_mask, 0);
    assert_eq!(binding.safe_high_mask, 0x40);
    assert_eq!(site.output_status("2000", "relay").unwrap().submitted, None);
    site.poll(2001, |_, _, operation| {
        assert!(matches!(operation, Operation::Snapshot));
        Ok(Some(physical))
    }); // The binding has not advanced its generation yet.
    assert_eq!(site.output_status("2000", "relay").unwrap().submitted, None);
    // The token is globally assigned, not necessarily previous + 1.
    physical.generation = 19;
    physical.claimed_mask = 0x40;
    physical.serviced_generation = 19;
    physical.submitted_generation = 7;
    site.poll(2002, |_, _, operation| {
        assert!(matches!(operation, Operation::Snapshot));
        Ok(Some(physical))
    });
    assert_eq!(site.output_status("2000", "relay").unwrap().submitted, None);
    physical.submitted_generation = 19;
    physical.submitted_high_mask = 0x40;
    site.poll(2003, |_, _, operation| {
        assert!(matches!(operation, Operation::Snapshot));
        Ok(Some(physical))
    });
    assert_eq!(site.output_status("2000", "relay").unwrap().submitted, None);
    physical.submitted_high_mask = 0;
    physical.serviced_generation = 7;
    physical.submitted_generation = 19;
    site.poll(2010, |_, _, op| {
        assert!(matches!(op, Operation::Snapshot));
        Ok(Some(physical))
    });
    assert_eq!(site.output_status("2000", "relay").unwrap().submitted, None);
    physical.serviced_generation = 19;
    let observations = site.poll(2020, |_, _, op| {
        assert!(matches!(op, Operation::Snapshot));
        Ok(Some(physical))
    });
    assert!(observations["2000"].outputs["relay"].available);
    assert_eq!(
        site.output_status("2000", "relay").unwrap().submitted,
        Some((1.0, 2020))
    );
    site.output("2000", "relay", 0.0).unwrap();
    commands.clear();
    site.poll(2030, |_, _, op| {
        if matches!(op, Operation::Snapshot) {
            return Ok(Some(physical));
        }
        commands.push(op);
        Ok(None)
    });
    let [Operation::Publish(request)] = &commands[..] else {
        panic!("one steady request");
    };
    assert_eq!(
        (request.generation, request.output_mask, request.high_mask),
        (19, 0x40, 0x40)
    );
    assert_eq!(
        site.output_status("2000", "relay").unwrap().submitted,
        Some((1.0, 2020))
    );
    physical.requested_high_mask = 0x40;
    physical.submitted_high_mask = 0x40;
    physical.serviced_high_mask = 0x40;
    physical.last_command_error = 5; // Sticky history is not current failure.
    site.poll(2040, |_, _, op| {
        assert!(matches!(op, Operation::Snapshot));
        Ok(Some(physical))
    });
    assert_eq!(
        site.output_status("2000", "relay").unwrap().submitted,
        Some((0.0, 2040))
    );
    assert!(site.output("2000", "relay", 0.5).is_err());
    assert!(site.output("2000", "missing", 0.0).is_err());
    assert!(site.output("missing", "relay", 0.0).is_err());
    assert!(site.output_status("missing", "relay").is_none());
}

#[test]
fn site_radio_bank_rejects_alias_owners_and_reserved_bits_without_writes() {
    let mut settings = settings();
    let mut duplicate = settings["2000"].outputs[0].clone();
    duplicate.name = "alias".into();
    duplicate.endpoint.as_mut().unwrap().channel = "07".into();
    settings.get_mut("2000").unwrap().outputs.push(duplicate);
    assert!(RadioSite::prepare(&settings).is_err());
    settings.get_mut("2000").unwrap().outputs.pop();
    let mut site = RadioSite::prepare(&settings).unwrap();
    let mut physical = snapshot();
    physical.reserved_mask = 0x40;
    let observations = site.poll(2000, |_, _, op| {
        assert!(matches!(op, Operation::Snapshot));
        Ok(Some(physical))
    });
    assert!(!observations["2000"].outputs["relay"].available);
    assert_ne!(
        site.output_status("2000", "relay").unwrap().quality,
        ReadingQuality::Valid
    );
}

#[test]
fn site_radio_bank_ignores_external_bindings_and_reports_rejected_writes() {
    let mut definitions = settings();
    let node = definitions.get_mut("2000").unwrap();
    let mut input = node.inputs[0].clone();
    input.name = "external-input".into();
    input.endpoint.as_mut().unwrap().backend = rpt_advanced_core::config::SiteBackend::Gpiod;
    node.inputs.push(input);
    let mut output = node.outputs[0].clone();
    output.name = "external-output".into();
    output.endpoint.as_mut().unwrap().backend = rpt_advanced_core::config::SiteBackend::Gpiod;
    node.outputs.push(output);
    let mut site = RadioSite::prepare(&definitions).unwrap();
    let observations = site.poll(0, |owner, _, operation| {
        assert_eq!(owner, "1000");
        match operation {
            Operation::Snapshot => Ok(Some(snapshot())),
            Operation::Bind(_) => Err(-1),
            Operation::Publish(_) => panic!("no claimed binding"),
        }
    });
    assert!(!observations["2000"].inputs.contains_key("external-input"));
    assert!(!observations["2000"].outputs.contains_key("external-output"));
    assert!(!observations["2000"].outputs["relay"].available);
    let output = site.output_status("2000", "relay").unwrap();
    assert_eq!(output.quality, ReadingQuality::Fault);
    assert_eq!(output.submitted, None);
}

#[test]
fn site_radio_bank_cached_submission_does_not_keep_stopped_service_available() {
    for (observed_at_valid, observed_at_ms, snapshot_at_ms, quality) in [
        (1, 950, 5950, ReadingQuality::Stale),
        (0, 950, 1000, ReadingQuality::Unavailable),
        (1, 1001, 1000, ReadingQuality::Unavailable),
    ] {
        let mut site = RadioSite::prepare(&settings()).unwrap();
        let mut physical = snapshot();
        site.poll(2000, |_, _, operation| {
            Ok(matches!(operation, Operation::Snapshot).then_some(physical))
        });
        physical.generation = 19;
        physical.claimed_mask = 0x40;
        physical.submitted_generation = 19;
        physical.serviced_generation = 19;
        site.poll(2020, |_, _, operation| {
            assert!(matches!(operation, Operation::Snapshot));
            Ok(Some(physical))
        });
        let submitted = site.output_status("2000", "relay").unwrap().submitted;
        assert_eq!(submitted, Some((1.0, 2020)));
        physical.observed_at_valid = observed_at_valid;
        physical.observed_at_ms = observed_at_ms;
        physical.snapshot_at_ms = snapshot_at_ms;
        let observations = site.poll(7000, |_, _, operation| {
            Ok(matches!(operation, Operation::Snapshot).then_some(physical))
        });
        assert!(!observations["2000"].outputs["relay"].available);
        let state = site.output_status("2000", "relay").unwrap();
        assert_eq!(state.quality, quality);
        assert_eq!(state.submitted, submitted);
        physical.observed_at_valid = 1;
        physical.observed_at_ms = 8000;
        physical.snapshot_at_ms = 8000;
        let recovered = site.poll(8000, |_, _, operation| {
            Ok(matches!(operation, Operation::Snapshot).then_some(physical))
        });
        assert!(recovered["2000"].outputs["relay"].available);
    }
}
