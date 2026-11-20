//! Pure radio settings assembled from modular section overrides.
macro_rules! choices {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? })=>{
        $(#[$meta])*
        #[derive(Clone,Copy,Debug,PartialEq,Eq)]
        pub enum $name {$(#[doc=concat!("Public selection `",$wire,"`.")] $variant),+}
        impl $name {
            fn read(value:&str)->Self{match value{$($wire=>Self::$variant,)+ _=>unreachable!("normalized station selector")}}
        }
    }
}
mod claims;
mod inputs;
mod remote;
pub use remote::{RemoteAccess, RemoteBackend, RemotePolicy, RemoteSettings};
mod signals;
pub use signals::*;
mod memories;
mod station;
pub use station::*;
mod processing;
pub use processing::{ProcessingProfile, ProcessingRoles};

use super::{
    CandidateWarning, EffectiveMetadata, ModularCandidate, NormalizedProfileResource, PortKey,
    ResourceBinding, ResourceId, ResourceKind, ResourceScope,
};
pub use memories::{Memory, MemoryMode, MemoryTone, ScanList};
use std::{collections::BTreeMap, fmt};

/// Unresolvable relationship in an effective radio profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioProfileError {
    /// Resource containing the unresolved reference or duplicated selector.
    pub resource: ResourceBinding,
    /// Previously registered owner when two resources claim the same identity.
    pub conflicting: Option<ResourceBinding>,
    /// Fixed field token, safe for diagnostics.
    pub field: &'static str,
}
impl fmt::Display for RadioProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: unresolved radio setting {}",
            self.resource, self.field
        )
    }
}
impl std::error::Error for RadioProfileError {}

/// Effective radio configuration for one exact local port.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioPortProfile {
    /// Stable identity used by physical and peer owners.
    pub id: ResourceId,
    /// Exact registry key.
    pub key: PortKey,
    /// Resolved metadata with its local display name.
    pub metadata: EffectiveMetadata,
    /// General radio and remote scan/tune settings.
    pub general: RadioGeneral,
    /// Hardware selectors, mixer intentions and configured wiring.
    pub interface: Interface,
    /// Receiver programme policy.
    pub rx_audio: RxAudio,
    /// Transmitter programme policy.
    pub tx_audio: TxAudio,
    /// Carrier, speech, subaudible and PTT signaling.
    pub signals: RadioSignals,
    /// Independent RX-local, incoming peer, and voice/telemetry processing.
    pub processing: ProcessingRoles,
    /// Remote control, RF limits, and power bounds.
    pub remote: RemotePolicy,
    /// Memory selectors follow the order of the replacing array.
    pub memories: Vec<Memory>,
    /// Named scans with independently ordered memory selectors.
    pub scan_lists: BTreeMap<String, ScanList>,
}

/// Verified radio definitions and recognized public overrides.
#[derive(Debug, Default)]
pub struct ResolvedRadioProfiles {
    local: BTreeMap<ResourceId, NormalizedProfileResource>,
    ports: BTreeMap<ResourceId, RadioPortProfile>,
    warnings: Vec<CandidateWarning>,
}
impl ResolvedRadioProfiles {
    /// Read normalized sparse authoring fields by resource UUID.
    pub fn local(&self, id: ResourceId) -> Option<&NormalizedProfileResource> {
        self.local.get(&id)
    }
    /// Read one typed port by its registered UUID.
    pub fn port(&self, id: ResourceId) -> Option<&RadioPortProfile> {
        self.ports.get(&id)
    }
    /// Source-attributed warnings without supplied values.
    pub fn warnings(&self) -> &[CandidateWarning] {
        &self.warnings
    }
}

impl ModularCandidate<'_> {
    /// Resolve radio resources for every registered port.
    pub fn resolve_radio_profiles(&self) -> Result<ResolvedRadioProfiles, RadioProfileError> {
        let registry = self.registry();
        let site = ResourceScope::Site(registry.site_id());
        let mut result = ResolvedRadioProfiles {
            warnings: self.warnings().to_vec(),
            ..Default::default()
        };
        for b in registry.bindings() {
            let Some(resource) = self.staged(b.scope, b.kind) else {
                continue;
            };
            let configuration = match b.kind {
                ResourceKind::Radio | ResourceKind::Interface => {
                    let schema = if b.kind == ResourceKind::Radio {
                        station::GENERAL
                    } else {
                        station::INTERFACE
                    };
                    super::normalize::normalize(b, resource.fields(), schema, &mut result.warnings)
                }
                ResourceKind::RxSignaling | ResourceKind::TxSignaling => {
                    signals::normalize(b, resource.fields(), &mut result.warnings)
                }
                ResourceKind::Control => {
                    remote::control(b, resource.fields(), &mut result.warnings)
                }
                ResourceKind::Power | ResourceKind::Limits => {
                    let schema = if b.kind == ResourceKind::Power {
                        remote::fields::POWER
                    } else {
                        remote::fields::LIMITS
                    };
                    super::normalize::normalize(b, resource.fields(), schema, &mut result.warnings)
                }
                ResourceKind::Memories => {
                    memories::normalize_memories(b, resource.fields(), &mut result.warnings)
                }
                ResourceKind::ScanLists => {
                    memories::normalize_scans(b, resource.fields(), &mut result.warnings)
                }
                ResourceKind::RxAudio | ResourceKind::TxAudio | ResourceKind::PeerAudio => {
                    let schema = match b.kind {
                        ResourceKind::RxAudio => processing::fields::RX_AUDIO,
                        ResourceKind::TxAudio => processing::fields::TX_AUDIO,
                        _ => processing::fields::LINK,
                    };
                    super::normalize::normalize(b, resource.fields(), schema, &mut result.warnings)
                }
                _ => continue,
            };
            result.local.insert(
                b.id,
                NormalizedProfileResource {
                    binding: b,
                    configuration,
                },
            );
        }
        let mut device_claims = BTreeMap::new();
        for (key, id) in registry.port_keys() {
            let layers = |kind| {
                [ResourceScope::Port(id), site].map(|scope| {
                    registry
                        .binding(scope, kind)
                        .and_then(|b| result.local.get(&b.id))
                })
            };
            let inputs = |kind| inputs::Inputs::from_resources(layers(kind));
            let mut general = RadioGeneral::read(&inputs(ResourceKind::Radio));
            let mut interface =
                Interface::read(&inputs(ResourceKind::Interface), &mut result.warnings);
            let rx_audio = RxAudio::read(&inputs(ResourceKind::RxAudio));
            let tx_audio = TxAudio::read(&inputs(ResourceKind::TxAudio));
            let signals = RadioSignals::read(
                &inputs(ResourceKind::RxSignaling),
                &inputs(ResourceKind::TxSignaling),
                &interface,
                &mut result.warnings,
            )?;
            let port_binding = registry
                .binding(ResourceScope::Port(id), ResourceKind::Metadata)
                .expect("registered port identity");
            claims::apply(&mut interface, &signals, port_binding)?;
            let mut role = |kind| {
                let captured = [ResourceScope::Port(id), site]
                    .map(|scope| self.staged(scope, kind).is_some_and(|s| s.was_captured()));
                processing::resolve(kind, layers(kind), captured, &mut result.warnings)
            };
            let processing = ProcessingRoles {
                local: role(ResourceKind::RxAudio),
                link: role(ResourceKind::PeerAudio),
                voice_telemetry: role(ResourceKind::TxAudio),
            };
            let memories = memories::effective(layers(ResourceKind::Memories));
            general.startup_memory = inputs(ResourceKind::Radio)
                .values(&["startup_memory"])
                .find_map(|(value, b)| {
                    let n = value.as_u64()?;
                    if n as usize <= memories.len() {
                        Some(n as u32)
                    } else {
                        super::normalize::warn(&mut result.warnings, b, "startup_memory");
                        None
                    }
                });
            let remote = RemotePolicy::read(
                &inputs(ResourceKind::Control),
                &inputs(ResourceKind::Limits),
                &inputs(ResourceKind::Power),
                &general,
                memories.len(),
                &mut result.warnings,
            );
            let scan_lists = memories::scans(
                layers(ResourceKind::ScanLists),
                memories.len(),
                &mut result.warnings,
            )?;
            let metadata = self
                .port_metadata(id)
                .expect("assembled metadata")
                .effective(self.site_metadata());
            if metadata.enabled {
                claims::reserve(
                    &mut device_claims,
                    &interface,
                    &signals,
                    &remote,
                    port_binding,
                )?;
            }
            result.ports.insert(
                id,
                RadioPortProfile {
                    id,
                    key: key.clone(),
                    metadata,
                    processing,
                    general,
                    interface,
                    rx_audio,
                    tx_audio,
                    signals,
                    remote,
                    memories,
                    scan_lists,
                },
            );
        }
        Ok(result)
    }
}
