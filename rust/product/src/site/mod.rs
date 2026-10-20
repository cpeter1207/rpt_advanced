//! Site acquisition outside station control and audio callbacks.

pub mod acquisition;
pub mod backends;
pub mod iio;
pub mod radio;
pub mod worker;

/// Resolve enabled logical nodes; defaults alone never acquire a physical endpoint.
pub(crate) fn configured(
    document: &rpt_advanced_core::config::ConfigDocument,
) -> Result<
    std::collections::BTreeMap<String, rpt_advanced_core::config::ResolvedSiteSettings>,
    rpt_advanced_core::runtime::RuntimeError,
> {
    use rpt_advanced_core::config::{NodeId, ResolvedNodeSettings, ResolvedSiteSettings};
    let mut settings = std::collections::BTreeMap::new();
    for name in document.nodes() {
        let node = NodeId::new(name)?;
        if ResolvedNodeSettings::resolve(document, &node)?
            .value
            .enabled
        {
            settings.insert(
                name.to_owned(),
                ResolvedSiteSettings::resolve(document, &node)?.value,
            );
        }
    }
    Ok(settings)
}
