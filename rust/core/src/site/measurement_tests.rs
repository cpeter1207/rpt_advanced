use super::*;
use crate::config::{ConfigDocument, NodeId, ResolvedSiteSettings};
use crate::site::{Observation, ReadingQuality};

fn settings(extra: &str) -> crate::config::SensorSettings {
    let document = ConfigDocument::parse(&format!(
        "[1000]\n[sensor 1000 supply]\nenabled=yes\nbackend=iio\ndevice=adc0\nchannel=voltage0\n{extra}"
    )).unwrap();
    ResolvedSiteSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value
        .sensors
        .remove(0)
}

fn reading(value: f64, time: u64) -> Observation<f64> {
    Observation {
        value: Some(value),
        observed_at_ms: Some(time),
        quality: ReadingQuality::Valid,
    }
}

#[test]
fn site_measurement_scale_once() {
    let mut policy = settings(
        "calibration=two_point\ninput_low=0\nvalue_low=0\ninput_high=5\nvalue_high=20\nunit=A\n",
    );
    let mut state = SensorState::new(policy.clone());
    state.observe(reading(2.0, 1000), 1000);
    assert_eq!(state.snapshot(1000).current.value, Some(8.0));
    policy.precision = 0;
    state.reconfigure(policy);
    state.observe(reading(2.499, 2000), 2000);
    assert_eq!(state.snapshot(2000).current.value, Some(9.996));
    assert_eq!(state.snapshot(2000).minimum.unwrap().value, 8.0);
}

#[test]
fn site_measurement_quality_extrema() {
    let policy = settings("");
    let mut state = SensorState::new(policy.clone());
    assert_eq!(
        state.snapshot(0).current.quality,
        ReadingQuality::Unavailable
    );
    state.observe(reading(2.0, 1000), 1000);
    state.observe(
        Observation {
            value: None,
            observed_at_ms: Some(2000),
            quality: ReadingQuality::Fault,
        },
        2000,
    );
    let failed = state.snapshot(2000);
    assert_eq!(failed.current.value, Some(2.0));
    assert_eq!(failed.current.observed_at_ms, Some(1000));
    assert_eq!(failed.current.quality, ReadingQuality::Fault);
    state.observe(reading(1.0, 3000), 3000);
    assert_eq!(state.snapshot(7999).current.quality, ReadingQuality::Valid);
    assert_eq!(state.snapshot(8000).current.quality, ReadingQuality::Stale);
    assert_eq!(state.snapshot(8000).maximum.unwrap().observed_at_ms, 1000);
    assert_eq!(state.snapshot(8000).minimum.unwrap().observed_at_ms, 3000);
    state.reconfigure(policy.clone());
    assert_eq!(state.snapshot(8000).maximum.unwrap().value, 2.0);
    let mut changed = policy;
    changed.endpoint.as_mut().unwrap().channel = "voltage1".into();
    state.reconfigure(changed);
    assert!(state.snapshot(8000).minimum.is_none());
    assert!(state.snapshot(8000).current.value.is_none());
}

#[test]
fn site_measurement_smoothing_uses_elapsed_acquisition_time() {
    let mut state = SensorState::new(settings("smoothing_ms=1000\n"));
    state.observe(reading(0.0, 1000), 1000);
    state.observe(reading(10.0, 1500), 1500);
    assert!((state.snapshot(1500).smoothed.unwrap() - 3.9346934).abs() < 0.000001);
    state.observe(reading(10.0, 3000), 3000);
    assert!((state.snapshot(3000).smoothed.unwrap() - 8.6466472).abs() < 0.000001);
    assert_eq!(state.snapshot(3000).maximum.unwrap().value, 10.0);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        state.observe(reading(bad, 4000), 4000);
        assert_eq!(state.snapshot(4000).current.quality, ReadingQuality::Fault);
        assert_eq!(state.snapshot(4000).current.observed_at_ms, Some(3000));
    }
}

#[test]
fn site_measurement_cached_or_bad_timestamps_do_not_refresh_values() {
    let policy = settings("");
    let mut state = SensorState::new(policy);
    state.observe(reading(4.0, 1000), 1000);
    state.observe(reading(9.0, 1000), 2000);
    assert_eq!(state.snapshot(2000).current.value, Some(4.0));
    for bad_time in [999, 3000] {
        state.observe(reading(10.0, bad_time), 2000);
        let snapshot = state.snapshot(2000);
        assert_eq!(snapshot.current.quality, ReadingQuality::Fault);
        assert_eq!(snapshot.current.observed_at_ms, Some(1000));
        assert_eq!(snapshot.maximum.unwrap().value, 4.0);
    }
    state.reconfigure(settings(
        "calibration=two_point\ninput_low=0\ninput_high=5\nvalue_low=0\nvalue_high=20\n",
    ));
    assert!(state.snapshot(2000).maximum.is_none());
    state.observe(reading(1.0, 2000), 2000);
    assert_eq!(state.snapshot(2000).current.value, Some(4.0));
}
