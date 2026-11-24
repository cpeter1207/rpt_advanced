//! Pure radio settings assembled from modular section overrides.
macro_rules! choices {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? })=>{
        $(#[$meta])*
        #[derive(Clone,Copy,Debug,PartialEq,Eq)]
        pub enum $name {$(#[doc=concat!("Public selection `",$wire,"`.")] $variant),+}
        impl $name {
            /// Canonical public spelling for this selection.
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire,)+ } }
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
pub use memories::{Memory, MemoryMode, MemoryTone, PartialSiteScan, ScanList};
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

/// Site defaults may rely on a real port to complete a recognized hardware or timing relationship.
struct SitePrerequisites<'a>(Option<&'a mut Vec<RadioProfileError>>);
impl SitePrerequisites<'_> {
    fn defer(&mut self, resource: ResourceBinding, field: &'static str) -> bool {
        if let Some(problems) = self.0.as_mut() {
            problems.push(RadioProfileError {
                resource,
                conflicting: None,
                field,
            });
            return true;
        }
        false
    }
}

/// Effective radio configuration for one exact local port.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioPortProfile {
    /// Stable identity used by physical and peer owners.
    pub id: ResourceId,
    /// Exact registry key.
    pub key: PortKey,
    /// Resolved metadata with its local display name.
    pub metadata: EffectiveMetadata,
    /// Effective radio settings shared with the site-default projection.
    pub settings: RadioSettings,
}
impl std::ops::Deref for RadioPortProfile {
    type Target = RadioSettings;
    fn deref(&self) -> &Self::Target {
        &self.settings
    }
}

/// Resolved public radio settings. Only a registered port carries a physical identity.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioSettings {
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

type BandSources = BTreeMap<(ResourceScope, ResourceKind), BTreeMap<String, ResourceBinding>>;
/// Verified radio definitions and recognized public overrides.
#[derive(Debug)]
pub struct ResolvedRadioProfiles {
    band_sources: BandSources,
    site: RadioSettings,
    partial_site_scans: BTreeMap<String, PartialSiteScan>,
    site_diagnostics: Vec<RadioProfileError>,
    site_warnings: Vec<CandidateWarning>,
    sources: BTreeMap<(ResourceScope, ResourceKind), ResourceBinding>,
    local: BTreeMap<ResourceId, NormalizedProfileResource>,
    ports: BTreeMap<ResourceId, RadioPortProfile>,
    warnings: Vec<CandidateWarning>,
}
impl ResolvedRadioProfiles {
    /// Inspect field-normalized site defaults even when their references need a port override.
    pub fn site(&self) -> &RadioSettings {
        &self.site
    }
    /// Field-normalized site scan rows waiting for selectors or members from a port.
    pub fn partial_site_scans(&self) -> &BTreeMap<String, PartialSiteScan> {
        &self.partial_site_scans
    }
    /// Relationships requiring completion or correction before stand-alone site admission.
    pub fn site_diagnostics(&self) -> &[RadioProfileError] {
        &self.site_diagnostics
    }
    /// Source-attributed warnings for site defaults independent of individual port admission.
    pub fn site_warnings(&self) -> &[CandidateWarning] {
        &self.site_warnings
    }
    /// Actual authors selected for dynamics band fields, including shared stage defaults.
    /// Paths use public configuration field names relative to the requested section.
    pub fn processing_band_sources(
        &self,
        scope: ResourceScope,
        kind: ResourceKind,
    ) -> impl Iterator<Item = (&str, ResourceBinding)> {
        self.band_sources
            .get(&(scope, kind))
            .into_iter()
            .flatten()
            .map(|(path, b)| (path.as_str(), *b))
    }
    /// Captured authoring resource, including an explicitly empty processing document.
    pub fn source(&self, scope: ResourceScope, kind: ResourceKind) -> Option<ResourceBinding> {
        self.sources.get(&(scope, kind)).copied()
    }
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
        let mut local = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut warnings = self.warnings().to_vec();
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
                    super::normalize::normalize(b, resource.fields(), schema, &mut warnings)
                }
                ResourceKind::RxSignaling | ResourceKind::TxSignaling => {
                    signals::normalize(b, resource.fields(), &mut warnings)
                }
                ResourceKind::Control => remote::control(b, resource.fields(), &mut warnings),
                ResourceKind::Power | ResourceKind::Limits => {
                    let schema = if b.kind == ResourceKind::Power {
                        remote::fields::POWER
                    } else {
                        remote::fields::LIMITS
                    };
                    super::normalize::normalize(b, resource.fields(), schema, &mut warnings)
                }
                ResourceKind::Memories => {
                    memories::normalize_memories(b, resource.fields(), &mut warnings)
                }
                ResourceKind::ScanLists => {
                    memories::normalize_scans(b, resource.fields(), &mut warnings)
                }
                ResourceKind::RxAudio | ResourceKind::TxAudio | ResourceKind::PeerAudio => {
                    let schema = match b.kind {
                        ResourceKind::RxAudio => processing::fields::RX_AUDIO,
                        ResourceKind::TxAudio => processing::fields::TX_AUDIO,
                        _ => processing::fields::LINK,
                    };
                    super::normalize::normalize(b, resource.fields(), schema, &mut warnings)
                }
                _ => continue,
            };
            if resource.was_captured() {
                sources.insert((b.scope, b.kind), b);
            }
            local.insert(
                b.id,
                NormalizedProfileResource {
                    binding: b,
                    configuration,
                },
            );
        }
        let mut site_warnings = warnings
            .iter()
            .filter(|w| w.resource.scope == site)
            .cloned()
            .collect();
        let mut site_diagnostics = Vec::new();
        let mut partial_site_scans = BTreeMap::new();
        let mut band_sources = BandSources::new();
        let site_settings = self.radio_settings(
            site,
            &local,
            &mut site_warnings,
            &mut site_diagnostics,
            Some(&mut partial_site_scans),
            &mut band_sources,
        );
        let mut device_claims = BTreeMap::new();
        let mut ports = BTreeMap::new();
        for (key, id) in registry.port_keys() {
            let scope = ResourceScope::Port(id);
            let mut diagnostics = Vec::new();
            let settings = self.radio_settings(
                scope,
                &local,
                &mut warnings,
                &mut diagnostics,
                None,
                &mut band_sources,
            );
            if let Some(error) = diagnostics.into_iter().next() {
                return Err(error);
            }
            for (kind, receive) in [
                (ResourceKind::RxSignaling, true),
                (ResourceKind::TxSignaling, false),
            ] {
                if let Some(binding) = registry.binding(scope, kind)
                    && let Some(resource) = local.get_mut(&binding.id)
                {
                    signals::prune_inherited_serial(
                        &mut resource.configuration,
                        receive,
                        &settings.signals,
                    );
                }
            }
            let metadata = self
                .port_metadata(id)
                .expect("assembled metadata")
                .effective(self.site_metadata());
            if metadata.enabled {
                let binding = registry
                    .binding(scope, ResourceKind::Metadata)
                    .expect("registered port identity");
                claims::reserve(
                    &mut device_claims,
                    &settings.interface,
                    &settings.signals,
                    &settings.remote,
                    binding,
                )?;
            }
            ports.insert(
                id,
                RadioPortProfile {
                    id,
                    key: key.clone(),
                    metadata,
                    settings,
                },
            );
        }
        Ok(ResolvedRadioProfiles {
            site: site_settings,
            band_sources,
            partial_site_scans,
            site_diagnostics,
            site_warnings,
            sources,
            local,
            ports,
            warnings,
        })
    }

    fn radio_settings(
        &self,
        scope: ResourceScope,
        local: &BTreeMap<ResourceId, NormalizedProfileResource>,
        warnings: &mut Vec<CandidateWarning>,
        diagnostics: &mut Vec<RadioProfileError>,
        partial_site_scans: Option<&mut BTreeMap<String, PartialSiteScan>>,
        band_sources: &mut BandSources,
    ) -> RadioSettings {
        let registry = self.registry();
        let site = ResourceScope::Site(registry.site_id());
        let site_only = scope == site;
        let scopes = [(!site_only).then_some(scope), Some(site)];
        let owner = registry
            .binding(scope, ResourceKind::Metadata)
            .expect("registered scope identity");
        let layers = |kind| {
            scopes.map(|scope| {
                scope
                    .and_then(|s| registry.binding(s, kind))
                    .and_then(|b| local.get(&b.id))
            })
        };
        let inputs = |kind| inputs::Inputs::from_resources(layers(kind));
        let mut general = RadioGeneral::read(&inputs(ResourceKind::Radio));
        let mut interface = Interface::read(
            &inputs(ResourceKind::Interface),
            warnings,
            &mut SitePrerequisites(site_only.then_some(&mut *diagnostics)),
        );
        let rx_audio = RxAudio::read(&inputs(ResourceKind::RxAudio));
        let tx_audio = TxAudio::read(&inputs(ResourceKind::TxAudio));
        let mut role = |kind| {
            let captured = scopes.map(|scope| {
                scope
                    .and_then(|scope| self.staged(scope, kind))
                    .is_some_and(|s| s.was_captured())
            });
            processing::resolve(kind, layers(kind), captured, warnings, &mut |path, b| {
                let prefix = if kind == ResourceKind::PeerAudio {
                    ""
                } else {
                    "/processing"
                };
                band_sources
                    .entry((scope, kind))
                    .or_default()
                    .insert(format!("{prefix}/{}", path.join("/")), b);
            })
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
                } else if site_only {
                    diagnostics.push(RadioProfileError {
                        resource: b,
                        conflicting: None,
                        field: "startup_memory",
                    });
                    Some(n as u32)
                } else {
                    super::normalize::warn(warnings, b, "startup_memory");
                    None
                }
            });
        let mut remote = RemotePolicy::read(
            &inputs(ResourceKind::Control),
            &inputs(ResourceKind::Limits),
            &inputs(ResourceKind::Power),
            &general,
            memories.len(),
            warnings,
        );
        if site_only
            && !remote.enabled
            && let Some((v, b)) = inputs(ResourceKind::Control).values(&["enabled"]).next()
            && v.as_bool() == Some(true)
        {
            diagnostics.push(RadioProfileError {
                resource: b,
                conflicting: None,
                field: "enabled",
            });
            remote.enabled = true;
        }
        let default_ptt = if !remote.enabled && interface.audio.exact_alsa_pcm() {
            SignalSource::NoActuator
        } else {
            SignalSource::SoundChipGpio
        };
        let signals = RadioSignals::read(
            &inputs(ResourceKind::RxSignaling),
            &inputs(ResourceKind::TxSignaling),
            default_ptt,
            &interface,
            site_only,
            warnings,
            diagnostics,
        );
        // The site exposes the authored connector roles. A port exposes the actual prepared wiring.
        let mut projected;
        let wiring = if site_only {
            projected = interface.clone();
            &mut projected
        } else {
            &mut interface
        };
        if let Err(error) = claims::apply(wiring, &signals, owner) {
            diagnostics.push(error);
        }
        let scan_lists = memories::scans(
            layers(ResourceKind::ScanLists),
            memories.len(),
            partial_site_scans,
            warnings,
            diagnostics,
        );
        RadioSettings {
            general,
            interface,
            rx_audio,
            tx_audio,
            signals,
            processing,
            remote,
            memories,
            scan_lists,
        }
    }
}
