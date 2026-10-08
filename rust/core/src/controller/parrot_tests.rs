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

#[test]
fn parrot_capture_sanitizes_nonfinite_audio_and_recycles_empty_bursts() {
    let (mut capture, mut control) = ParrotCapture::new();
    capture.observe(true, &[f32::NAN, f32::INFINITY]);
    capture.observe(false, &[]);
    assert_eq!(control.take_completed().unwrap().samples(), &[0.0, 0.0]);

    capture.observe(true, &[]);
    capture.observe(false, &[]);
    assert!(control.take_completed().is_none());
}

#[test]
fn parrot_capture_returns_a_clip_buffer_when_the_completion_queue_is_full() {
    let (mut completed, completed_consumer) = rtrb::RingBuffer::new(1);
    completed
        .push(super::CapturedParrot {
            samples: vec![0.25],
        })
        .unwrap();
    let (recycled, recycled_consumer) = rtrb::RingBuffer::new(1);
    let mut capture = ParrotCapture {
        active: true,
        recording: true,
        current: Some(vec![0.25]),
        free: Vec::new(),
        completed,
        recycled: recycled_consumer,
    };
    let _control = super::ParrotCaptureControl {
        completed: completed_consumer,
        recycled,
    };

    capture.observe(false, &[]);

    assert!(!capture.recording);
    assert!(capture.current.is_none());
    assert_eq!(capture.free.len(), 1);
    assert!(capture.free[0].is_empty());
}
