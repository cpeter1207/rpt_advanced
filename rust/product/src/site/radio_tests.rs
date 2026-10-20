use super::*;

#[path = "radio_bank_tests.rs"]
mod bank;

fn snapshot() -> abi::UrpAstSiteSnapshot {
    abi::UrpAstSiteSnapshot {
        struct_size: size_of::<abi::UrpAstSiteSnapshot>() as u32,
        abi_version: 1,
        generation: 7,
        input_mask: 2,
        output_mask: 0xc0,
        input_high_mask: 2,
        input_quality: 1,
        observed_at_ms: 950,
        observed_at_valid: 1,
        snapshot_at_ms: 1000,
        ..unsafe { std::mem::zeroed() }
    }
}

#[test]
fn site_radio_physical_pins_and_reserved_masks_use_the_existing_owner() {
    for (backend, line, output, expected) in [
        (SiteBackend::Cm119, "1", false, (0, 1)),
        (SiteBackend::Cm119, "8", true, (0, 128)),
        (SiteBackend::Parallel, "2", true, (1, 1)),
        (SiteBackend::Parallel, "9", true, (1, 128)),
        (SiteBackend::Parallel, "10", false, (1, 0x4000)),
        (SiteBackend::Parallel, "12", false, (1, 0x2000)),
        (SiteBackend::Parallel, "13", false, (1, 0x1000)),
        (SiteBackend::Parallel, "15", false, (1, 0x0800)),
    ] {
        assert_eq!(pin(backend, line, output), Ok(expected));
    }
    for (backend, line, output) in [
        (SiteBackend::Cm119, "0", false),
        (SiteBackend::Cm119, "9", true),
        (SiteBackend::Parallel, "11", false),
        (SiteBackend::Parallel, "10", true),
        (SiteBackend::Parallel, "2", false),
        (SiteBackend::Gpiod, "1", false),
        (SiteBackend::Cm119, "bad", false),
    ] {
        assert!(pin(backend, line, output).is_err());
    }
    let mut physical = snapshot();
    assert_eq!(validate(&physical, 2, false), Ok(()));
    assert_eq!(validate(&physical, 0xc0, true), Ok(()));
    assert!(validate(&physical, 0, false).is_err());
    assert!(validate(&physical, 1, false).is_err());
    physical.reserved_mask = 0x40;
    assert!(validate(&physical, 0xc0, true).is_err());
    physical.generation = 0;
    assert!(validate(&physical, 0x80, true).is_err());
}

#[test]
fn site_radio_observation_preserves_age_and_never_normalizes_a_failed_read() {
    let mut physical = snapshot();
    let measured = input(&physical, 2, false, 2000);
    assert_eq!(measured.value, Some(false));
    assert_eq!(measured.observed_at_ms, Some(1950));
    assert_eq!(measured.quality, ReadingQuality::Valid);
    physical.snapshot_at_ms = 1100;
    assert_eq!(input(&physical, 2, false, 2100).observed_at_ms, Some(1950));
    physical.input_quality = 2;
    assert_eq!(
        input(&physical, 2, false, 2100).quality,
        ReadingQuality::Fault
    );
    physical.input_quality = 1;
    physical.snapshot_at_ms = 5950;
    assert_eq!(
        input(&physical, 2, false, 6950).quality,
        ReadingQuality::Stale
    );
    physical.observed_at_valid = 0;
    let missing = input(&physical, 2, true, 6950);
    assert_eq!(missing.value, None);
    assert_eq!(missing.observed_at_ms, None);
    assert_eq!(missing.quality, ReadingQuality::Unavailable);
    physical.observed_at_valid = 1;
    physical.observed_at_ms = physical.snapshot_at_ms + 1;
    assert_eq!(
        input(&physical, 2, true, 7000).quality,
        ReadingQuality::Unavailable
    );
    physical.observed_at_ms = physical.snapshot_at_ms;
    physical.input_quality = 0;
    assert_eq!(
        input(&physical, 2, true, 7000).quality,
        ReadingQuality::Unavailable
    );
}
