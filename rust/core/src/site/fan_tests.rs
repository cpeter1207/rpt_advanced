use super::*;
use crate::{config::resources::candidate::ResourceKind, station_fixture::equipment};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn settings(extra: Value) -> crate::config::FanSettings {
    let mut fields = json!({"enabled":true,"output":"relay","mode":"ptt"});
    json_patch::merge(&mut fields, &extra);
    equipment(&[
        (ResourceKind::Sensor, "heat", json!({})),
        (ResourceKind::Sensor, "tach", json!({})),
        (
            ResourceKind::Output,
            "relay",
            json!({"enabled":true,"source":{"type":"hwmon","device":"chip","channel":"pwm1"}}),
        ),
        (ResourceKind::Fan, "cooling", fields),
    ])
    .fans
    .remove(0)
}
fn ptt(on: bool) -> BTreeMap<crate::config::resources::PortKey, bool> {
    BTreeMap::from([(crate::config::resources::PortKey::new("1000"), on)])
}
fn available(pwm: bool) -> FanOutput {
    FanOutput {
        available: true,
        pwm,
    }
}

#[test]
fn site_fan_demand_and_failure() {
    let mut fan = FanState::new(settings(json!({})));
    assert_eq!(
        fan.advance(0, &ptt(true), None, None, available(false))
            .duty,
        1.0
    );
    assert_eq!(
        fan.advance(1000, &ptt(false), None, None, available(false))
            .duty,
        1.0
    );
    assert_eq!(
        fan.advance(120999, &ptt(false), None, None, available(false))
            .duty,
        1.0
    );
    assert_eq!(
        fan.advance(121000, &ptt(false), None, None, available(false))
            .duty,
        0.0
    );
    let mut fan = FanState::new(settings(
        json!({"mode":"temperature","temperature_sensor":"heat","on_temperature":40,"off_temperature":35}),
    ));
    for (temperature, duty) in [(39.0, 0.0), (40.0, 1.0), (36.0, 1.0), (35.0, 0.0)] {
        assert_eq!(
            fan.advance(0, &ptt(false), Some(temperature), None, available(false))
                .duty,
            duty
        );
    }
    let failed = fan.advance(1, &ptt(false), None, None, available(false));
    assert_eq!(failed.duty, 1.0);
    assert!(failed.fault);
    fan.manual(Some(0.0)).unwrap();
    assert_eq!(
        fan.advance(2, &ptt(false), None, None, available(false))
            .duty,
        1.0
    );
    assert!(
        fan.advance(
            3,
            &ptt(false),
            Some(30.0),
            None,
            FanOutput {
                available: false,
                pwm: false
            }
        )
        .fault
    );
}

#[test]
fn site_fan_curve_combined_and_rpm_capability() {
    let mut fan = FanState::new(settings(
        json!({"mode":"combined","temperature_sensor":"heat","temperature_curve":[{"temperature":20.0,"duty":0.2},{"temperature":40.0,"duty":0.8}],"timing":{"run_on_ms":0,"spinup_ms":1000},"rpm_sensor":"tach","minimum_rpm":500}),
    ));
    assert_eq!(
        fan.advance(0, &ptt(false), Some(30.0), Some(0.0), available(true))
            .duty,
        0.5
    );
    assert!(
        !fan.advance(999, &ptt(false), Some(30.0), Some(0.0), available(true))
            .fault
    );
    assert!(
        fan.advance(1000, &ptt(false), Some(30.0), Some(0.0), available(true))
            .fault
    );
    assert!(
        !fan.advance(1001, &ptt(false), Some(30.0), Some(600.0), available(true))
            .fault
    );
    assert_eq!(
        fan.advance(1002, &ptt(true), Some(30.0), Some(600.0), available(true))
            .duty,
        1.0
    );
    assert!(
        fan.advance(1003, &ptt(false), Some(30.0), Some(600.0), available(false))
            .fault
    );
    assert!(fan.manual(Some(f64::NAN)).is_err());
    assert!(fan.manual(Some(1.1)).is_err());
}

#[test]
fn site_fan_disabled_stable_ptt_and_invalid_tach() {
    let mut disabled = FanState::new(settings(json!({"enabled":false})));
    assert_eq!(
        disabled.advance(100, &ptt(true), None, None, available(false)),
        FanSnapshot {
            duty: 0.0,
            fault: false,
            run_on_until_ms: None,
        }
    );
    let mut fan = FanState::new(settings(
        json!({"rpm_sensor":"tach","minimum_rpm":500,"timing":{"spinup_ms":0}}),
    ));
    assert!(
        !fan.advance(0, &ptt(true), None, Some(600.0), available(false))
            .fault
    );
    let running = fan.advance(100, &ptt(true), None, Some(f64::NAN), available(false));
    assert_eq!(running.duty, 1.0);
    assert_eq!(running.run_on_until_ms, None);
    assert!(running.fault);
}

#[test]
fn missing_selector_is_not_a_confirmed_transmitter_release() {
    let mut fan = FanState::new(settings(json!({"timing":{"run_on_ms":0}})));
    let initial = fan.advance(0, &BTreeMap::new(), None, None, available(false));
    assert!(initial.fault);
    assert_eq!(initial.duty, 0.0);
    assert_eq!(
        fan.advance(1, &ptt(true), None, None, available(false))
            .duty,
        1.0
    );
    let missing = fan.advance(2, &BTreeMap::new(), None, None, available(false));
    assert!(missing.fault);
    assert_eq!(missing.duty, 1.0);
    let released = fan.advance(3, &ptt(false), None, None, available(false));
    assert!(!released.fault);
    assert_eq!(released.duty, 0.0);
}
