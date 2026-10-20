//! Isolated kernel-attribute fixtures; no physical IIO device is accessed.

use crate::site::backends::{
    SiteBackends,
    endpoint::{Direction, Endpoint, Sample},
};
use crate::site::iio::IioReader;
use rpt_advanced_core::{
    config::{ConfigDocument, SiteBackend, SiteEndpoint},
    site::ReadingQuality,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rptadv-site-iio-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("adc")).unwrap();
        Self(root)
    }

    fn put(&self, name: &str, value: &str) {
        fs::write(self.0.join("adc").join(name), value).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn site_endpoint_iio_uses_normalized_input_and_never_claims_output() {
    let fixture = Fixture::new();
    fixture.put("in_temp_input", "25000\n");
    let providers = std::sync::Arc::new(
        SiteBackends::load(&ConfigDocument::parse("[1000]\n").unwrap()).unwrap(),
    );
    let settings = SiteEndpoint {
        backend: SiteBackend::Iio,
        device: fixture.0.join("adc").to_str().unwrap().to_owned(),
        channel: "in_temp".into(),
    };
    let mut endpoint = Endpoint::open(
        std::sync::Arc::clone(&providers),
        &settings,
        Direction::Input { active_high: true },
    )
    .unwrap();
    assert_eq!(endpoint.read(), Ok(Sample::Number(25.0)));
    fixture.put("in_temp_input", "nan\n");
    assert_eq!(endpoint.read(), Err(ReadingQuality::Fault));
    assert!(!endpoint.pwm());
    assert_eq!(endpoint.safe(), Ok(()));
    assert_eq!(endpoint.write(0.0), Err(ReadingQuality::Unavailable));
    assert!(matches!(
        Endpoint::open(
            providers,
            &settings,
            Direction::Output {
                active_high: true,
                startup_on: false,
                safe_on: false
            }
        ),
        Err(ReadingQuality::Unavailable)
    ));
}

#[test]
fn site_iio_scale_offset_once_and_unit_normalization() {
    let fixture = Fixture::new();
    fixture.put("in_voltage0_raw", "1000\n");
    fixture.put("in_voltage_scale", "2\n");
    fixture.put("in_voltage_offset", "100\n");
    let mut reader = IioReader::open(&fixture.0, "adc", "in_voltage0").unwrap();
    assert_eq!(reader.read().unwrap(), 2.2);
    // Read current attributes, not a cached scale or stale sample.
    fixture.put("in_voltage_scale", "1\n");
    assert_eq!(reader.read().unwrap(), 1.1);
    fixture.put("in_voltage0_scale", "3\n");
    let mut specific = IioReader::open(&fixture.0, "adc", "in_voltage0").unwrap();
    assert_eq!(specific.read().unwrap(), 3.3);
}

#[test]
fn site_iio_processed_input_does_not_apply_raw_scale() {
    let fixture = Fixture::new();
    fixture.put("in_temp_input", "25000\n");
    fixture.put("in_temp_scale", "malformed\n");
    fixture.put("in_temp_offset", "100\n");
    let mut reader = IioReader::open(&fixture.0, "adc", "in_temp").unwrap();
    assert_eq!(reader.read().unwrap(), 25.0);
}

#[test]
fn site_iio_current_without_scale_is_already_milliamps() {
    let fixture = Fixture::new();
    fixture.put("in_current0_raw", "1250\n");
    let mut reader = IioReader::open(&fixture.0, "adc", "in_current0").unwrap();
    assert_eq!(reader.read().unwrap(), 1.25);
}

#[test]
fn site_iio_differential_voltage_uses_its_shared_scale() {
    let fixture = Fixture::new();
    fixture.put("in_voltage0-voltage1_raw", "-500\n");
    fixture.put("in_voltage-voltage_scale", "2\n");
    let mut reader = IioReader::open(&fixture.0, "adc", "in_voltage0-voltage1").unwrap();
    assert_eq!(reader.read().unwrap(), -1.0);
}

#[test]
fn site_iio_failed_or_nonfinite_observation_is_not_zero() {
    let fixture = Fixture::new();
    fixture.put("in_current1_raw", "10\n");
    fixture.put("in_current_scale", "100\n");
    let mut reader = IioReader::open(&fixture.0, "adc", "in_current1").unwrap();
    assert_eq!(reader.read().unwrap(), 1.0);
    for invalid in ["", "garbage", "NaN", "inf", "-inf", "1 2"] {
        fixture.put("in_current1_raw", invalid);
        assert!(reader.read().is_err(), "invalid raw sample {invalid:?}");
    }
    fixture.put("in_current1_raw", "1e308");
    fixture.put("in_current_scale", "1e308");
    assert!(reader.read().is_err());
    fixture.put("in_current1_raw", "10");
    fixture.put("in_current_scale", "NaN");
    assert!(reader.read().is_err());
    fixture.put("in_current1_raw", &"0".repeat(129));
    fixture.put("in_current_scale", "1");
    assert!(reader.read().is_err());
}

#[test]
fn site_iio_identity_never_selects_another_enumerated_device() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.put("in_voltage0_input", "2000");
    symlink("adc", fixture.0.join("stable-adc")).unwrap();
    let mut reader = IioReader::open(&fixture.0, "stable-adc", "in_voltage0").unwrap();
    fs::create_dir(fixture.0.join("other")).unwrap();
    fs::write(fixture.0.join("other/in_voltage0_input"), "9000").unwrap();
    fs::remove_file(fixture.0.join("stable-adc")).unwrap();
    symlink("other", fixture.0.join("stable-adc")).unwrap();
    assert_eq!(reader.read().unwrap(), 2.0);
    assert!(IioReader::open(&fixture.0, "missing", "in_voltage0").is_err());
}

#[test]
fn site_iio_requires_a_supported_input_with_units() {
    let fixture = Fixture::new();
    fixture.put("in_voltage0_raw", "42");
    // An unscaled ADC code is not a voltage measurement.
    assert!(IioReader::open(&fixture.0, "adc", "in_voltage0").is_err());
    assert!(IioReader::open(&fixture.0, "", "in_voltage0").is_err());
    for channel in [
        "../in_voltage0",
        "in_voltage0/raw",
        "out_voltage0",
        "in_mystery0",
        "in_voltagewrong",
    ] {
        assert!(IioReader::open(&fixture.0, "adc", channel).is_err());
    }
    fixture.put("in_temp_1_input", "22000");
    assert_eq!(
        IioReader::open(&fixture.0, "adc", "in_temp_1")
            .unwrap()
            .read()
            .unwrap(),
        22.0
    );
    fixture.put("in_current0_raw", "100");
    fixture.put("in_current_offset", "1");
    assert!(IioReader::open(&fixture.0, "adc", "in_current0").is_err());
    // A present but unreadable attribute must not be treated as a missing one.
    std::os::unix::fs::symlink("in_voltage0_input", fixture.0.join("adc/in_voltage0_input"))
        .unwrap();
    assert!(IioReader::open(&fixture.0, "adc", "in_voltage0").is_err());
}
