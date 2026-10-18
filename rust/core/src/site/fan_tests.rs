use super::*;
use crate::config::{ConfigDocument, NodeId, ResolvedSiteSettings};
use std::collections::BTreeMap;

fn settings(extra: &str) -> crate::config::FanSettings {
    ResolvedSiteSettings::resolve(&ConfigDocument::parse(&format!("[1000]\n[sensor 1000 heat]\n[sensor 1000 tach]\n[output 1000 relay]\nenabled=yes\nbackend=hwmon\ndevice=chip\nline=pwm1\n[fan 1000 cooling]\nenabled=yes\noutput=relay\nmode=ptt\n{extra}")).unwrap(), &NodeId::new("1000").unwrap()).unwrap().value.fans.remove(0)
}
fn ptt(on: bool) -> BTreeMap<NodeId, bool> {
    BTreeMap::from([(NodeId::new("1000").unwrap(), on)])
}
fn available(pwm: bool) -> FanOutput {
    FanOutput {
        available: true,
        pwm,
    }
}

#[test]
fn site_fan_demand_and_failure() {
    let policy = settings("");
    let mut fan = FanState::new(policy.clone());
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
    fan.reconfigure(policy);
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
        "mode=temperature\ntemperature_sensor=heat\non_temperature=40\noff_temperature=35\n",
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
        "mode=combined\ntemperature_sensor=heat\ntemperature_curve=20:0.2,40:0.8\nrun_on_ms=0\nrpm_sensor=tach\nminimum_rpm=500\nspinup_ms=1000\n",
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
