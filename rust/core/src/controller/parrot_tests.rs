use super::{ParrotCapture, ParrotLevels};

#[test]
fn parrot_levels_measure_peak_and_rms_with_nearest_integer_rounding() {
    let levels = ParrotLevels::measure(&[1.0, 0.5]).unwrap();
    assert_eq!(levels.peak_dbfs, 0);
    assert_eq!(levels.rms_dbfs, -2);
}

#[test]
fn parrot_levels_floor_digital_silence_and_reject_empty_audio() {
    assert_eq!(
        ParrotLevels::measure(&[0.0, 0.0]).unwrap(),
        ParrotLevels {
            peak_dbfs: -120,
            rms_dbfs: -120,
        }
    );
    assert_eq!(ParrotLevels::measure(&[]), None);
}

#[test]
fn parrot_capture_mixes_one_burst_until_all_sources_unkey() {
    let (mut capture, mut control) = ParrotCapture::new();
    capture.observe(true, &[0.25, 0.5]);
    capture.observe(true, &[0.125]);
    capture.observe(false, &[]);

    let clip = control.take_completed().unwrap();
    assert_eq!(clip.samples(), &[0.25, 0.5, 0.125]);
}

#[test]
fn parrot_capture_clamps_mix_and_retains_only_thirty_seconds() {
    let (mut capture, mut control) = ParrotCapture::new();
    capture.observe(true, &[1.5, -1.5]);
    capture.observe(true, &vec![0.5; super::MAX_PARROT_SAMPLES]);
    capture.observe(true, &[0.75, 0.75]);
    capture.observe(false, &[]);

    let clip = control.take_completed().unwrap();
    assert_eq!(clip.samples().len(), super::MAX_PARROT_SAMPLES);
    assert_eq!(clip.samples()[..2], [1.0, -1.0]);
    assert_eq!(*clip.samples().last().unwrap(), 0.5);
}

#[test]
fn parrot_capture_drops_burst_when_no_recycled_slot_is_available() {
    let (mut capture, mut control) = ParrotCapture::new();
    capture.observe(true, &[0.25]);
    capture.observe(false, &[]);
    capture.observe(true, &[0.75]);
    capture.observe(false, &[]);
    assert_eq!(control.take_completed().unwrap().samples(), &[0.25]);
    let clip = control.take_completed().unwrap();
    assert_eq!(clip.samples(), &[0.75]);

    capture.observe(true, &[0.25]);
    capture.observe(false, &[]);
    assert!(control.take_completed().is_none());

    control.recycle(clip);
    capture.observe(true, &[0.5]);
    capture.observe(false, &[]);
    assert_eq!(control.take_completed().unwrap().samples(), &[0.5]);
}
