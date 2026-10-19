use super::*;
use crate::config::{ConfigDocument, NodeId, ResolvedSiteSettings};
use crate::site::{FanOutput, Observation, OutputRequest, ReadingQuality, SiteError};
use std::collections::BTreeMap;

fn settings(extra: &str) -> ResolvedSiteSettings {
    ResolvedSiteSettings::resolve(&ConfigDocument::parse(&format!("[1000]\n[sensor 1000 supply]\nenabled=yes\nbackend=iio\ndevice=adc\nchannel=v\nprecision=1\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=door\non_active=output:relay:pulse\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=relay\n[message trip]\ncontent=Warning\n[message repeat]\ncontent=Still unsafe\n[message clear]\ncontent=Recovered\n{extra}")).unwrap(), &NodeId::new("1000").unwrap()).unwrap().value
}
fn observation<T>(value: T, now: u64) -> Observation<T> {
    Observation {
        value: Some(value),
        observed_at_ms: Some(now),
        quality: ReadingQuality::Valid,
    }
}
fn observations(value: f64, now: u64) -> SiteObservations {
    SiteObservations {
        sensors: BTreeMap::from([("supply".into(), observation(value, now))]),
        inputs: BTreeMap::new(),
        outputs: BTreeMap::from([(
            "relay".into(),
            FanOutput {
                available: true,
                pwm: false,
            },
        )]),
    }
}

#[test]
fn site_output_pending_acquisition_preserves_startup_without_claiming_availability() {
    let mut policy = settings("");
    policy.outputs[0].startup_on = true;
    let mut site = SiteController::new(policy);
    let pending = SiteObservations::default();
    site.advance(0, &pending, &BTreeMap::new());
    let output = site.snapshot(0).outputs["relay"];
    assert!(
        output.requested_on,
        "startup demand must survive pending device acquisition"
    );
    assert!(output.fault, "pending must not claim available hardware");
    assert_eq!(
        site.command(
            SiteCommand::Output {
                name: "relay".into(),
                request: OutputRequest::Pulse
            },
            0
        ),
        Err(SiteError::Unavailable)
    );
    site.advance(1, &observations(12.5, 1), &BTreeMap::new());
    assert!(site.snapshot(1).outputs["relay"].requested_on);
    assert!(!site.snapshot(1).outputs["relay"].fault);
    site.advance(2, &pending, &BTreeMap::new());
    assert!(
        !site.snapshot(2).outputs["relay"].requested_on,
        "lost known owner must request safe state"
    );
    assert!(site.snapshot(2).outputs["relay"].fault);
}

#[test]
fn site_reload_preserves_pulse_fan_alarm_and_measurement_state() {
    let policy = settings(
        "[output 1000 cooling]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=fan\n[fan 1000 cooling]\nenabled=yes\noutput=cooling\nmode=ptt\nrun_on_ms=1000\n[alarm 1000 high]\nenabled=yes\nsource=supply\ncondition=above\ntrip_value=14\nclear_value=13\nlatched=yes\non_trip=inhibit:on\n",
    );
    let mut site = SiteController::new(policy.clone());
    let node = NodeId::new("1000").unwrap();
    let mut readings = observations(15.0, 1000);
    readings.outputs.insert(
        "cooling".into(),
        FanOutput {
            available: true,
            pwm: false,
        },
    );
    site.advance(1000, &readings, &BTreeMap::from([(node.clone(), true)]));
    site.command(
        SiteCommand::Output {
            name: "relay".into(),
            request: OutputRequest::Pulse,
        },
        1000,
    )
    .unwrap();
    site.command(SiteCommand::AlarmAcknowledge("high".into()), 1000)
        .unwrap();
    site.advance(1100, &readings, &BTreeMap::from([(node.clone(), false)]));
    let mut replacement = policy;
    replacement.sensors[0].precision = 3;
    site.reconfigure(replacement);
    let restored = site.snapshot(1200);
    assert_eq!(restored.outputs["relay"].pulse_deadline_ms, Some(1500));
    assert_eq!(restored.fans["cooling"].run_on_until_ms, Some(2100));
    assert_eq!(restored.sensors["supply"].maximum.unwrap().value, 15.0);
    assert!(restored.alarms["high"].acknowledged);
    assert!(restored.transmit_inhibited);
    let unkeyed = BTreeMap::from([(node, false)]);
    site.advance(1500, &readings, &unkeyed);
    assert!(!site.snapshot(1500).outputs["relay"].requested_on);
    site.advance(2100, &readings, &unkeyed);
    assert_eq!(site.snapshot(2100).fans["cooling"].duty, 0.0);
}

#[test]
fn site_reload_resets_changed_endpoints_without_replaying_old_outputs() {
    let policy = settings("");
    let mut site = SiteController::new(policy.clone());
    let mut readings = observations(14.0, 1000);
    readings
        .inputs
        .insert("door".into(), observation(false, 1000));
    site.advance(1000, &readings, &BTreeMap::new());
    site.command(SiteCommand::Inhibit(true), 1000).unwrap();
    site.command(
        SiteCommand::Output {
            name: "relay".into(),
            request: OutputRequest::Pulse,
        },
        1000,
    )
    .unwrap();
    let mut replacement = policy;
    replacement.sensors[0].endpoint.as_mut().unwrap().device = "different-adc".into();
    replacement.inputs[0].endpoint.as_mut().unwrap().device = "different-chip".into();
    replacement.outputs[0].endpoint.as_mut().unwrap().device = "different-chip".into();
    site.reconfigure(replacement.clone());
    let changed = site.snapshot(1200);
    assert_eq!(changed.outputs["relay"].pulse_deadline_ms, None);
    assert!(!changed.outputs["relay"].requested_on);
    assert!(changed.outputs["relay"].fault);
    assert!(changed.sensors["supply"].maximum.is_none());
    assert_eq!(changed.inputs["door"].quality, ReadingQuality::Unavailable);
    assert!(changed.transmit_inhibited);
    assert_eq!(
        site.advance(1200, &readings, &BTreeMap::new()),
        vec![SiteEffect::Output {
            name: "relay".into(),
            on: false
        }]
    );
    replacement.outputs[0].enabled = false;
    site.reconfigure(replacement);
    assert!(site.snapshot(1201).outputs["relay"].fault);
    site.reconfigure(ResolvedSiteSettings::default());
    assert!(site.snapshot(1202).outputs.is_empty());
    assert!(site.snapshot(1202).sensors.is_empty());
}

#[test]
fn site_controller_alarm_actions_are_transition_only_and_keep_protection() {
    let policy = settings(
        "[alarm 1000 high]\nenabled=yes\nsource=supply\ncondition=above\ntrip_value=14\nclear_value=13\nlatched=yes\nrepeat_interval_ms=100\non_trip=output:relay:on,inhibit:on\non_clear=output:relay:off,inhibit:off\ntrip_message=trip\nrepeat_message=repeat\nclear_message=clear\n",
    );
    let mut site = SiteController::new(policy);
    site.advance(0, &observations(13.96, 0), &BTreeMap::new());
    assert!(!site.snapshot(0).alarms["high"].active);
    let trip = site.advance(100, &observations(14.0, 100), &BTreeMap::new());
    assert_eq!(
        trip,
        vec![
            SiteEffect::Output {
                name: "relay".into(),
                on: true
            },
            SiteEffect::TransmitInhibit(true),
            SiteEffect::Message("trip".into())
        ]
    );
    assert!(site.snapshot(100).transmit_inhibited);
    let repeat = site.advance(200, &observations(14.0, 200), &BTreeMap::new());
    assert_eq!(repeat, vec![SiteEffect::Message("repeat".into())]);
    assert_eq!(
        site.command(SiteCommand::AlarmReset("high".into()), 200),
        Err(SiteError::Unsafe)
    );
    site.command(SiteCommand::Inhibit(false), 200).unwrap();
    assert!(site.snapshot(200).transmit_inhibited);
    site.command(SiteCommand::AlarmAcknowledge("high".into()), 200)
        .unwrap();
    assert!(
        site.advance(300, &observations(14.0, 300), &BTreeMap::new())
            .is_empty()
    );
    site.advance(400, &observations(12.0, 400), &BTreeMap::new());
    assert_eq!(
        site.command(SiteCommand::AlarmReset("high".into()), 400)
            .unwrap(),
        vec![
            SiteEffect::Output {
                name: "relay".into(),
                on: false
            },
            SiteEffect::TransmitInhibit(false),
            SiteEffect::Message("clear".into())
        ]
    );
}

#[test]
fn site_controller_debounce_and_extrema_reset() {
    let mut site = SiteController::new(settings(""));
    let mut readings = observations(13.0, 0);
    readings.inputs.insert("door".into(), observation(false, 0));
    site.advance(0, &readings, &BTreeMap::new());
    readings
        .inputs
        .insert("door".into(), observation(true, 100));
    site.advance(100, &readings, &BTreeMap::new());
    assert!(
        site.advance(200, &readings, &BTreeMap::new()).is_empty(),
        "cached input cannot prove a completed debounce"
    );
    readings
        .inputs
        .insert("door".into(), observation(true, 150));
    assert_eq!(
        site.advance(200, &readings, &BTreeMap::new()),
        vec![SiteEffect::Output {
            name: "relay".into(),
            on: true
        }]
    );
    assert!(site.advance(201, &readings, &BTreeMap::new()).is_empty());
    assert_eq!(
        site.advance(700, &readings, &BTreeMap::new()),
        vec![SiteEffect::Output {
            name: "relay".into(),
            on: false
        }]
    );
    site.command(SiteCommand::ResetExtrema("supply".into()), 700)
        .unwrap();
    assert!(site.snapshot(700).sensors["supply"].minimum.is_none());
    assert!(
        site.command(
            SiteCommand::Output {
                name: "missing".into(),
                request: OutputRequest::On
            },
            700
        )
        .is_err()
    );
}

#[test]
fn site_controller_alarm_uses_selected_unrounded_measurement() {
    let mut policy = settings(
        "[alarm 1000 high]\nenabled=yes\nsource=supply\ncondition=above\ntrip_value=9\nclear_value=8\nreading=smoothed\n",
    );
    policy.sensors[0].smoothing_ms = 1000;
    let mut site = SiteController::new(policy);
    site.advance(0, &observations(0.0, 0), &BTreeMap::new());
    site.advance(100, &observations(10.0, 100), &BTreeMap::new());
    assert!(!site.snapshot(100).alarms["high"].active);
    site.advance(3000, &observations(10.0, 3000), &BTreeMap::new());
    assert!(site.snapshot(3000).alarms["high"].active);
}

#[test]
fn site_controller_lost_input_is_unknown_without_clearing_alarm() {
    let mut site = SiteController::new(settings(
        "[alarm 1000 door_open]\nenabled=yes\nsource=door\ncondition=active\nlatched=yes\non_trip=inhibit:on\non_clear=inhibit:off\n",
    ));
    let mut readings = observations(12.0, 0);
    readings.inputs.insert("door".into(), observation(true, 0));
    site.advance(0, &readings, &BTreeMap::new());
    assert!(site.snapshot(0).alarms["door_open"].active);
    readings.inputs.clear();
    site.advance(100, &readings, &BTreeMap::new());
    assert_eq!(
        site.snapshot(100).inputs["door"].quality,
        ReadingQuality::Unavailable
    );
    assert_eq!(
        site.snapshot(100).alarms["door_open"].unsafe_condition,
        None
    );
    assert!(site.snapshot(100).transmit_inhibited);
    assert_eq!(
        site.command(SiteCommand::AlarmReset("door_open".into()), 100),
        Err(SiteError::Unsafe)
    );
}

#[test]
fn site_controller_failed_output_does_not_skip_protection_or_repeat_actions() {
    let mut site = SiteController::new(settings(
        "[alarm 1000 high]\nenabled=yes\nsource=supply\ncondition=above\ntrip_value=14\nclear_value=13\non_trip=output:relay:on,inhibit:on\ntrip_message=trip\n",
    ));
    site.advance(0, &observations(12.0, 0), &BTreeMap::new());
    let mut readings = observations(15.0, 100);
    readings.outputs.clear();
    assert_eq!(
        site.advance(100, &readings, &BTreeMap::new()),
        vec![
            SiteEffect::Fault {
                target: "alarm:high".into(),
                error: SiteError::Unavailable
            },
            SiteEffect::TransmitInhibit(true),
            SiteEffect::Message("trip".into())
        ]
    );
    assert!(site.advance(101, &readings, &BTreeMap::new()).is_empty());
}

#[test]
fn site_controller_fan_owner_blocks_direct_output_override() {
    let mut policy = settings("");
    policy.inputs[0].on_active.clear();
    let extra = ResolvedSiteSettings::resolve(
        &ConfigDocument::parse(
            "[1000]\n[output 1000 relay]\n[fan 1000 cooling]\nenabled=yes\noutput=relay\n",
        )
        .unwrap(),
        &NodeId::new("1000").unwrap(),
    )
    .unwrap()
    .value;
    policy.fans = extra.fans;
    let mut site = SiteController::new(policy);
    site.advance(0, &observations(12.0, 0), &BTreeMap::new());
    assert_eq!(
        site.command(
            SiteCommand::Output {
                name: "relay".into(),
                request: OutputRequest::On
            },
            1
        ),
        Err(SiteError::Unsafe)
    );
    assert_eq!(
        site.command(
            SiteCommand::Fan {
                name: "cooling".into(),
                duty: Some(1.0)
            },
            1
        )
        .unwrap(),
        vec![SiteEffect::Fan {
            name: "cooling".into(),
            duty: 1.0
        }]
    );
    assert_eq!(
        site.command(
            SiteCommand::Fan {
                name: "cooling".into(),
                duty: None
            },
            2
        )
        .unwrap(),
        vec![SiteEffect::Fan {
            name: "cooling".into(),
            duty: 0.0
        }]
    );
}
