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
            phone: None,
        })
        .unwrap();
    let (recycled, recycled_consumer) = rtrb::RingBuffer::new(1);
    let mut capture = ParrotCapture {
        active: true,
        recording: true,
        phone: None,
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

thread_local! {
    static AFTER_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
pub(super) fn after_capture() {
    let hook = AFTER_CAPTURE.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[test]
fn mixed_retirement_revisions_never_qualify_a_retained_private_clip() {
    let domain = crate::link::RetirementDomain::default();
    let mut source = super::ParrotPhoneSource {
        age_bound: Some(crate::link::SourceTimeBound {
            host_clock_epoch: 7,
            not_after_ns: 30,
        }),
        retirement_revision: domain.revision(),
    };
    source.include(super::ParrotPhoneSource {
        age_bound: source.age_bound,
        retirement_revision: Some(1),
    });
    assert!(!source.current(Some(&domain)));
    assert_eq!(source.retirement_revision, None);
    assert_eq!(source.age_bound.unwrap().not_after_ns, 30);
}

#[test]
fn cancelling_without_a_capture_buffer_preserves_capacity_for_the_next_burst() {
    let (mut capture, mut control) = ParrotCapture::new();
    capture.cancel_burst();
    capture.observe(true, &[0.5]);
    capture.cancel_burst();
    capture.cancel_burst();
    assert_eq!(capture.free.len(), 2);
    capture.observe(false, &[]);
    assert!(control.take_completed().is_none());
    capture.observe(true, &[0.25]);
    capture.observe(false, &[]);
    assert_eq!(control.take_completed().unwrap().samples(), &[0.25]);
}

#[test]
fn retirement_during_capture_copy_discards_the_clip_and_recycles_its_buffer() {
    let domain = crate::link::RetirementDomain::default();
    let reply = crate::link::PeerSignals::new().reply_target();
    reply.bind_retirement_domain(&domain);
    let phone = super::ParrotPhoneSource {
        age_bound: None,
        retirement_revision: domain.revision(),
    };
    let (mut capture, mut control) = ParrotCapture::new();
    AFTER_CAPTURE.with(|slot| *slot.borrow_mut() = Some(Box::new(move || reply.retire())));
    capture.observe_with_phone(true, &[0.5], Some(phone), Some(&domain));
    assert!(!capture.recording);
    assert_eq!(capture.free.len(), 2);
    capture.observe_with_phone(false, &[], None, Some(&domain));
    assert!(control.take_completed().is_none());
    capture.observe(true, &[0.25]);
    capture.observe(false, &[]);
    assert_eq!(control.take_completed().unwrap().samples(), &[0.25]);
}
