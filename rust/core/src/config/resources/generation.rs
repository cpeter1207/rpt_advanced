//! Owned, validated domain settings for one modular configuration generation.
use super::{
    EffectiveMetadata,
    candidate::{ResourceRegistry, ResourceScope},
    commands::ScopedCommands,
    events::ScopedEvents,
    profiles::{
        ResolvedProtocolProfiles,
        command_policy::ScopedCommandPolicy,
        equipment::ScopedEquipment,
        mailbox::ScopedMailboxes,
        permanent::PermanentGroups,
        public_messages::{
            ScopedMessages,
            telemetry::{PreparedTelemetry, ScopedTelemetry},
        },
        radio::ResolvedRadioProfiles,
    },
};
use crate::{composition::Composition, config::ResolvedSiteSettings};
use std::{collections::BTreeMap, sync::Arc};

/// Resolvers selected together for the invoking site or one registered port.
pub struct ScopeConfiguration {
    /// Display, locale and enabled state selected through this scope's parent.
    pub metadata: EffectiveMetadata,
    /// Command language and named macro bindings.
    pub commands: ScopedCommands,
    /// Muting, regeneration and administration timing.
    pub policy: ScopedCommandPolicy,
    /// Complete public identities to match against authenticated private PINs.
    pub mailboxes: ScopedMailboxes,
    /// Installed/localized catalog and compiled named messages.
    pub messages: Arc<ScopedMessages>,
    /// Checked message recipes used by command bindings in this scope.
    pub command_messages: Arc<BTreeMap<String, Composition>>,
    /// Text-only EchoLink status recipe when configured.
    pub echo_status: Option<Composition>,
    /// Speech, Morse and controller telemetry timing for this scope.
    pub telemetry: ScopedTelemetry,
    /// Checked identifiers, announcements, courtesies, tails and playback recipes.
    pub media: PreparedTelemetry,
    /// Equipment policies and bindings, resolved in the invoking scope.
    pub equipment: ScopedEquipment,
    /// Stable route declarations resolved against actual protocol aliases.
    pub permanent: PermanentGroups,
    /// Same-namespace compiled calendar and controller-start actions.
    pub events: ScopedEvents,
}

/// Prepared domain data independent of the capture buffers and filesystem edit guard.
pub struct RuntimeConfiguration {
    /// Registry identities and exact port keys from this candidate.
    pub registry: ResourceRegistry,
    /// Typed settings for every actual radio and the site authoring defaults.
    pub radio: ResolvedRadioProfiles,
    /// Independently attached protocol accounts and playout policy.
    pub protocols: ResolvedProtocolProfiles,
    /// Site and port settings from the same preparation as the public projections.
    pub scopes: BTreeMap<ResourceScope, ScopeConfiguration>,
    /// Each physical site resource is acquired only by its declaring owner.
    pub acquisition: BTreeMap<ResourceScope, ResolvedSiteSettings>,
}
