use super::ParrotLevels;

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
