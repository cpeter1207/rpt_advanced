//! Physical-pin mapping for site I/O borrowed from a configured radio owner.
use crate::abi;
use rpt_advanced_core::{
    config::SiteBackend,
    site::{Observation, ReadingQuality},
};

#[path = "radio_bank.rs"]
mod bank;
pub use bank::{Operation, RadioSite};

/// Select an existing bank and bit, never a new radio/device handle.
pub fn pin(backend: SiteBackend, line: &str, output: bool) -> Result<(u32, u32), ReadingQuality> {
    let line = line
        .parse::<u32>()
        .map_err(|_| ReadingQuality::Unavailable)?;
    match (backend, line, output) {
        (SiteBackend::Cm119, 1..=8, _) => Ok((0, 1 << (line - 1))),
        (SiteBackend::Parallel, 2..=9, true) => Ok((1, 1 << (line - 2))),
        (SiteBackend::Parallel, 10, false) => Ok((1, 0x4000)),
        (SiteBackend::Parallel, 12, false) => Ok((1, 0x2000)),
        (SiteBackend::Parallel, 13, false) => Ok((1, 0x1000)),
        (SiteBackend::Parallel, 15, false) => Ok((1, 0x0800)),
        _ => Err(ReadingQuality::Unavailable),
    }
}

/// Resolve a public GPIO/parallel property on the declared bank, using the native pin map.
pub(super) fn property_pin(
    property: &str,
    output: bool,
) -> Result<Option<(u32, u32)>, ReadingQuality> {
    let Some((backend, line)) = property
        .strip_prefix("/gpio/pins/")
        .map(|line| (SiteBackend::Cm119, line))
        .or_else(|| {
            property
                .strip_prefix("/parallel/pins/")
                .map(|line| (SiteBackend::Parallel, line))
        })
    else {
        return Ok(None);
    };
    // Only the exact published property name identifies this pin and its exclusive output claim.
    if line.starts_with('0') || !line.bytes().all(|c| c.is_ascii_digit()) {
        return Err(ReadingQuality::Unavailable);
    }
    pin(backend, line, output).map(Some)
}

/// Check only a cached capability mask published by the physical radio owner.
/// Caller supplies the selected real port and bank's cache; this function never opens hardware.
pub fn cached_pin_capability(
    owner: &rpt_advanced_core::config::resources::profiles::equipment::BoundInterface,
    kind: rpt_advanced_core::config::resources::candidate::ResourceKind,
    cache: impl FnOnce(
        &rpt_advanced_core::config::resources::PortKey,
        u32,
    ) -> Option<abi::UrpAstSiteSnapshot>,
) -> Option<rpt_advanced_core::config::resources::profiles::equipment::InterfaceCapability> {
    use rpt_advanced_core::config::resources::{
        candidate::ResourceKind, profiles::equipment::InterfaceCapability,
    };
    let output = match kind {
        ResourceKind::Output => true,
        ResourceKind::Input | ResourceKind::Sensor => false,
        _ => return None,
    };
    let (bank, mask) = property_pin(&owner.property, output).ok()??;
    validate(&cache(&owner.key, bank)?, mask, output).ok()?;
    Some(if output {
        InterfaceCapability::DigitalOutput
    } else {
        InterfaceCapability::Boolean
    })
}

/// Reject unavailable/reserved pins using the selected owner's published capability.
pub fn validate(
    snapshot: &abi::UrpAstSiteSnapshot,
    mask: u32,
    output: bool,
) -> Result<(), ReadingQuality> {
    let available = if output {
        snapshot.output_mask
    } else {
        snapshot.input_mask
    };
    if snapshot.generation == 0 || mask == 0 || mask & (!available | snapshot.reserved_mask) != 0 {
        return Err(ReadingQuality::Unavailable);
    }
    Ok(())
}

/// Translate actual input age onto station time and apply configured polarity once.
pub fn input(
    snapshot: &abi::UrpAstSiteSnapshot,
    mask: u32,
    active_high: bool,
    now_ms: u64,
) -> Observation<bool> {
    let mut result = Observation {
        value: None,
        observed_at_ms: None,
        quality: ReadingQuality::Unavailable,
    };
    if validate(snapshot, mask, false).is_err() || snapshot.observed_at_valid != 1 {
        return result;
    }
    let Some(age) = snapshot.snapshot_at_ms.checked_sub(snapshot.observed_at_ms) else {
        return result;
    };
    result.value = Some((snapshot.input_high_mask & mask != 0) == active_high);
    result.observed_at_ms = Some(now_ms.saturating_sub(age));
    result.quality = match snapshot.input_quality {
        1 if age < 5000 => ReadingQuality::Valid,
        1 => ReadingQuality::Stale,
        2 => ReadingQuality::Fault,
        _ => ReadingQuality::Unavailable,
    };
    result
}

#[cfg(test)]
#[path = "radio_tests.rs"]
mod tests;
