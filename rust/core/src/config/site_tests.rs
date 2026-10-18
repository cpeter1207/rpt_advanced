use super::*;
use crate::config::{ConfigDocument, ConfigError, NodeId, Resolution};

fn resolve(text: &str) -> Result<Resolution<ResolvedSiteSettings>, ConfigError> {
    ResolvedSiteSettings::resolve(&ConfigDocument::parse(text)?, &NodeId::new("1000")?)
}

#[test]
fn site_config_inheritance_and_identity() {
    let parsed = resolve("[1000]\n[sensor]\nprecision=2\nbackend=iio\ndevice=must-not-inherit\nchannel=voltage0\n[sensor 1000]\nprecision=3\n[sensor 1000 supply]\nenabled=yes\nbackend=iio\ndevice=adc0\nchannel=voltage0\nprecision=4\ndtmf_id=1\n[sensor 1000 missing]\nenabled=yes\nprecision=bad\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=gpiochip0\nline=door\nactive_level=low\ndtmf_id=2\n[output 1000 fan]\nenabled=yes\nbackend=gpiod\ndevice=gpiochip0\nline=fan\n").unwrap();
    assert_eq!(parsed.value.sensors.len(), 2);
    let sensor = &parsed.value.sensors[0];
    assert!(sensor.enabled);
    assert_eq!(sensor.name, "supply");
    assert_eq!(sensor.precision, 4);
    assert_eq!(sensor.poll_interval_ms, 1000);
    assert_eq!(sensor.stale_after_ms, 5000);
    assert_eq!(sensor.smoothing_ms, 0);
    assert_eq!(sensor.dtmf_id.as_deref(), Some("1"));
    let missing = &parsed.value.sensors[1];
    assert!(!missing.enabled && missing.endpoint.is_none());
    assert_eq!(missing.precision, 3);
    assert!(!parsed.warnings.is_empty());
    assert!(!parsed.value.inputs[0].active_high);
    assert_eq!(parsed.value.inputs[0].debounce_ms, 50);
    assert_eq!(parsed.value.outputs[0].pulse_ms, 500);
    assert!(!parsed.value.outputs[0].startup_on && !parsed.value.outputs[0].safe_on);
    assert!(
        resolve("[1000]\n[sensor]\nenabled=yes\n[input 1000]\n[output]\n")
            .unwrap()
            .value
            .sensors
            .is_empty()
    );
}

#[test]
fn site_config_rejects_ambiguous_names_selectors_and_output_ownership() {
    for extra in [
        "[sensor 1000 shared]\n[input 1000 shared]\n",
        "[sensor 1000 a]\ndtmf_id=1\n[input 1000 b]\ndtmf_id=1\n",
        "[output 1000 a]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=fan\n[output 1000 b]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=fan\n",
    ] {
        assert!(resolve(&format!("[1000]\n{extra}")).is_err(), "{extra}");
    }
    let mut config = "[1000]\n".to_owned();
    for index in 1..=130 {
        config.push_str(&format!("[sensor 1000 sensor{index}]\ndtmf_id={index}\n"));
    }
    assert_eq!(resolve(&config).unwrap().value.sensors.len(), 130);
}

#[test]
fn site_config_invalid_values_warn_without_unsafe_calibration() {
    for value in ["NaN", "inf", "-inf", "0"] {
        let parsed = resolve(&format!("[1000]\n[sensor 1000 bad]\nenabled=yes\nbackend=iio\ndevice=adc0\nchannel=voltage0\ncalibration=two_point\ninput_low=0\nvalue_low=0\ninput_high={value}\nvalue_high=20\n")).unwrap();
        assert!(!parsed.value.sensors[0].enabled, "{value}");
        assert!(!parsed.warnings.is_empty());
    }
    let parsed = resolve("[1000]\n[sensor]\ninput_high=5\n[sensor 1000 valid]\nenabled=yes\nbackend=iio\ndevice=adc\nchannel=voltage\ncalibration=two_point\ninput_low=0\nvalue_low=0\ninput_high=NaN\nvalue_high=20\npoll_interval_ms=1000\nstale_after_ms=1000\nprecision=bad\ndtmf_id=01\n").unwrap();
    let sensor = &parsed.value.sensors[0];
    assert!(sensor.enabled);
    assert!(sensor.stale_after_ms > sensor.poll_interval_ms);
    assert_eq!(sensor.precision, 1);
    assert!(sensor.dtmf_id.is_none());
    assert!(!parsed.warnings.is_empty());
}

#[test]
fn site_config_empty_selectors_do_not_conflict() {
    let parsed =
        resolve("[1000]\n[sensor 1000 one]\ndtmf_id=\n[input 1000 two]\ndtmf_id=\n").unwrap();
    assert!(parsed.value.sensors[0].dtmf_id.is_none());
    assert!(parsed.value.inputs[0].dtmf_id.is_none());
}

#[test]
fn site_config_overflowing_calibration_is_disabled() {
    for points in [
        "input_low=-1e308\ninput_high=1e308\nvalue_low=0\nvalue_high=1\n",
        "input_low=0\ninput_high=1\nvalue_low=-1e308\nvalue_high=1e308\n",
    ] {
        let parsed = resolve(&format!("[1000]\n[sensor 1000 bad]\nenabled=yes\nbackend=iio\ndevice=adc\nchannel=v\ncalibration=two_point\n{points}")).unwrap();
        assert!(!parsed.value.sensors[0].enabled);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.key == "calibration")
        );
    }
}

#[test]
fn site_config_schema_validates_all_nodes_and_reports_effective_fallback() {
    let cross_node = "[1000]\n[2000]\n[output 1000 fan]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=fan\n[output 2000 other]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=fan\n";
    assert!(resolve(cross_node).is_err());
    for section in [
        "sensor 1000 bad extra",
        "input missing port",
        "output 1000 a]\n[output 1000 a",
    ] {
        assert!(resolve(&format!("[1000]\n[{section}]\n")).is_err());
    }
    let parsed = resolve("[1000]\n[sensor]\nprecision=2\n[sensor 1000]\nprecision=3\n[sensor 1000 a]\nprecision=no\nunknown_option=7\nbackend=gpiod\n").unwrap();
    assert_eq!(parsed.value.sensors[0].precision, 3);
    assert_eq!(
        parsed
            .warnings
            .iter()
            .find(|warning| warning.key == "precision")
            .unwrap()
            .fallback,
        "3"
    );
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.key == "backend")
    );
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.key == "unknown_option")
    );
}

#[test]
fn site_policy_configuration_rejects_unsafe_references_and_inherits_defaults() {
    let base = "[1000]\n[sensor 1000 heat]\n[input 1000 door]\n[output 1000 relay]\n";
    for bad in [
        "[alarm 1000 a]\nenabled=yes\nsource=missing\ncondition=unavailable\n",
        "[alarm 1000 a]\nenabled=yes\nsource=heat\ncondition=above\ntrip_value=10\nclear_value=20\n",
        "[alarm 1000 a]\nenabled=yes\nsource=door\ncondition=active\non_trip=output:missing:on\n",
        "[alarm 1000 a]\nenabled=yes\nsource=door\ncondition=active\ntrip_message=missing\n",
        "[fan 1000 f]\nenabled=yes\noutput=relay\nmode=temperature\ntemperature_sensor=heat\n",
        "[fan 1000 a]\nenabled=yes\noutput=relay\n[fan 1000 b]\nenabled=yes\noutput=relay\n",
    ] {
        assert!(resolve(&format!("{base}{bad}")).is_err(), "{bad}");
    }
    let parsed = resolve(&format!("{base}[alarm]\ntrip_delay_ms=100\n[alarm 1000]\ntrip_delay_ms=200\n[alarm 1000 a]\ntrip_delay_ms=bad\n[fan]\nrun_on_ms=1000\n[fan 1000 f]\nrun_on_ms=bad\n")).unwrap();
    assert_eq!(parsed.value.alarms[0].trip_delay_ms, 200);
    assert_eq!(parsed.value.fans[0].run_on_ms, 1000);
    assert_eq!(
        parsed
            .warnings
            .iter()
            .find(|warning| warning.key == "trip_delay_ms")
            .unwrap()
            .fallback,
        "200"
    );
}
