//! Release-triggered message inheritance; flat and node scopes provide defaults only.

use super::{
    ConfigDocument, ConfigError, NodeId, Resolution, ResolvedMessageSettings, Schema, parse,
    settings,
};

/// One named tail, with the existing shared message/media defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTailSettings {
    /// Complete ordered content and inherited source settings.
    pub message: ResolvedMessageSettings,
    /// Minimum milliseconds since successful completion; zero allows every release.
    pub interval_ms: u64,
    /// Milliseconds after actual unkey; zero retains PTT at ordinary hang expiry.
    pub delay_ms: u64,
}

impl ResolvedTailSettings {
    /// Resolve an existing instance over node and global tail defaults on control.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        name: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let scopes = [
            format!("tail {} {name}", node.as_str()),
            format!("tail {}", node.as_str()),
            "tail".into(),
        ];
        if !document.sections().contains(&scopes[0]) {
            return Err(ConfigError::structure(
                &scopes[0],
                "unknown tail definition",
            ));
        }
        let duration = |key| {
            settings::lookup_valid_scopes(document, key, &scopes, |value| {
                parse::unsigned(value, 0, u64::MAX / 48)
            })
            .unwrap_or(0)
        };
        Ok(Resolution {
            warnings,
            value: Self {
                message: ResolvedMessageSettings::from_scopes(document, node, &scopes, "{media}")?,
                interval_ms: duration("interval_ms"),
                delay_ms: duration("delay_ms"),
            },
        })
    }
}
