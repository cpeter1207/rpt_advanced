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

#[test]
fn public_capability_uses_exact_port_mask_and_published_pin_name() {
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::ResourceKind,
        profiles::equipment::{BoundInterface, InterfaceCapability},
    };
    let mut reference = BoundInterface {
        port: "929e52be-6854-4c46-85f2-000000000001".parse().unwrap(),
        key: PortKey::new("local / a"),
        property: "/gpio/pins/2".into(),
    };
    let port = reference.port;
    let cached = |id, bank: u32| {
        assert_eq!(id, port);
        assert_eq!(bank, 0);
        Some(abi::UrpAstSiteSnapshot {
            generation: 1,
            input_mask: 2,
            output_mask: 4,
            ..snapshot()
        })
    };
    assert_eq!(
        cached_pin_capability(&reference, ResourceKind::Sensor, cached),
        Some(InterfaceCapability::Boolean)
    );
    assert_eq!(
        cached_pin_capability(&reference, ResourceKind::Output, cached),
        None
    );
    reference.property = "/gpio/pins/02".into();
    assert_eq!(
        cached_pin_capability(&reference, ResourceKind::Input, cached),
        None
    );
    reference.property = "/gpio/pins/3".into();
    assert_eq!(
        cached_pin_capability(&reference, ResourceKind::Output, cached),
        Some(InterfaceCapability::DigitalOutput)
    );
}

#[test]
fn modular_interface_observes_registered_uuid_and_preserves_logical_owner() {
    use rpt_advanced_core::config::{
        InputSettings, ResolvedSiteSettings,
        resources::{PortKey, candidate::ResourceId, profiles::equipment::BoundInterface},
    };
    let hardware: ResourceId = "4ab24693-b134-4694-80ad-f7ee94dfc963".parse().unwrap();
    let logical_site = "4031b0c6-a609-4ea0-8b9c-4a35a18f44ef";
    let settings = ResolvedSiteSettings {
        inputs: vec![InputSettings {
            name: "door".into(),
            enabled: true,
            endpoint: None,
            interface: Some(BoundInterface {
                port: hardware,
                key: PortKey::new("station / West"),
                property: "/gpio/pins/2".into(),
            }),
            active_high: true,
            debounce_ms: 0,
            dtmf_id: None,
            on_active: vec![],
            on_inactive: vec![],
        }],
        ..Default::default()
    };
    let mut site = RadioSite::prepare(&std::collections::BTreeMap::from([(
        logical_site.to_owned(),
        settings,
    )]))
    .unwrap();
    let observations = site.poll(1000, |owner, bank, operation| {
        assert_eq!(
            owner,
            hardware.to_string(),
            "physical bank must use registered port UUID"
        );
        assert_eq!(bank, 0);
        assert!(matches!(operation, Operation::Snapshot));
        Ok(Some(snapshot()))
    });
    assert_eq!(observations[logical_site].inputs["door"].value, Some(true));
}
