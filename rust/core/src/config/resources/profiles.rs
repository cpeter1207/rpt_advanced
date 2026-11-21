//! Typed, public modular protocol settings resolved from identified local bodies.

mod allstarlink;
/// Public DTMF administration, muting and regeneration settings.
pub mod command_policy;
/// Final public ownership and account projections for private records.
pub mod credentials;
mod echolink;
/// Checked station equipment and physical interface bindings.
pub mod equipment;
mod jitter;
/// Public identities keyed by their real mailbox number and declaring resource.
pub mod mailbox;
mod normalize;
mod phone;
/// Named Fluent and media message preparation for a real management scope.
pub mod public_messages;
pub mod radio;

pub use allstarlink::{AllStarLinkAccountIdentity, AllStarLinkProfileSettings};
pub use echolink::{EchoLinkDirectoryIdentity, EchoLinkProxyIdentity};
pub use jitter::{JitterField, JitterProfile, JitterTiming};
pub use normalize::TimingSource;
pub use phone::{
    PhoneAccountIdentity, PhoneLatencyTiming, PhonePatchProfileSettings, PhonePlayoutDestination,
    PhoneProtocol, PhoneProviderSettings, PhoneVadSettings, VadMode,
};

use super::{
    EffectiveMetadata, PortKey,
    candidate::{
        CandidateWarning, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
        ResourceScope,
    },
};
use crate::config::EchoLinkSettings;
use normalize::{Layers, normalize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fmt};

/// Recognized local authoring fields of one bound resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedProfileResource {
    /// Registry-owned resource binding.
    pub binding: ResourceBinding,
    /// Recognized sparse settings before descendant-specific relationship checks.
    pub configuration: Map<String, Value>,
}

/// Recognized sparse fields of a protocol resource.
pub type NormalizedProtocolResource = NormalizedProfileResource;

/// Explicit protocol with typed settings and independent playout policy.
#[derive(Clone, Debug, PartialEq)]
pub struct ProtocolProfile<T> {
    /// Registry-owned instance binding.
    pub binding: ResourceBinding,
    /// Effective public account and protocol settings.
    pub settings: T,
    /// Common playout policy and its validity chain.
    pub jitter: JitterProfile,
}

impl ProtocolProfile<EchoLinkSettings> {
    /// Inspect directory credential affinity from this public account.
    pub fn directory_identity(&self) -> Option<EchoLinkDirectoryIdentity<'_>> {
        let settings = &self.settings;
        (!settings.callsign.is_empty() && settings.node_number != 0).then_some(
            EchoLinkDirectoryIdentity {
                callsign: &settings.callsign,
                node_number: settings.node_number,
                directory_servers: &settings.directory_servers,
            },
        )
    }
}

/// Public protocol and metadata settings for one port.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedProtocolPort {
    /// Exact registered port key.
    pub key: PortKey,
    /// Effective metadata.
    pub metadata: EffectiveMetadata,
    /// Independent peer-audio binding when registered, including an empty local override.
    pub peer_audio: Option<ResourceBinding>,
    /// Explicit AllStarLink instance.
    pub allstarlink: Option<ProtocolProfile<AllStarLinkProfileSettings>>,
    /// Explicit EchoLink instance.
    pub echolink: Option<ProtocolProfile<EchoLinkSettings>>,
    /// Independently captured proxy child of this port's EchoLink account.
    pub echolink_proxy: Option<ResourceBinding>,
    /// Explicit phone-patch instance.
    pub phone_patch: Option<ProtocolProfile<PhonePatchProfileSettings>>,
    /// Relationship diagnostics scoped to this effective port.
    pub warnings: Vec<CandidateWarning>,
}

/// Independently captured protocol settings at one scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedProtocolAccounts {
    /// Explicit AllStarLink instance.
    pub allstarlink: Option<ProtocolProfile<AllStarLinkProfileSettings>>,
    /// Explicit EchoLink instance.
    pub echolink: Option<ProtocolProfile<EchoLinkSettings>>,
    /// Independently captured proxy child of this scope's EchoLink account.
    pub echolink_proxy: Option<ResourceBinding>,
    /// Explicit phone-patch instance.
    pub phone_patch: Option<ProtocolProfile<PhonePatchProfileSettings>>,
}

fn proxy_identity(
    echo: Option<&ProtocolProfile<EchoLinkSettings>>,
    binding: Option<ResourceBinding>,
) -> Option<EchoLinkProxyIdentity<'_>> {
    let binding = binding?;
    let account = echo?;
    let parent = account.directory_identity()?;
    let settings = &account.settings;
    (!settings.proxy_host.is_empty()).then_some(EchoLinkProxyIdentity {
        binding,
        callsign: parent.callsign,
        node_number: parent.node_number,
        host: &settings.proxy_host,
        port: settings.proxy_port,
    })
}
impl ResolvedProtocolAccounts {
    /// Credential affinity of this scope's real proxy child and parent account.
    pub fn proxy_identity(&self) -> Option<EchoLinkProxyIdentity<'_>> {
        proxy_identity(self.echolink.as_ref(), self.echolink_proxy)
    }
}
impl ResolvedProtocolPort {
    /// Credential affinity of the port's proxy child, including an inactive proxy.
    pub fn proxy_identity(&self) -> Option<EchoLinkProxyIdentity<'_>> {
        proxy_identity(self.echolink.as_ref(), self.echolink_proxy)
    }
}

/// A protocol relationship with no safe resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolProfileProblem {
    /// Enabled ports claim the same AllStarLink network node.
    DuplicateAllStarLinkNode,
    /// Enabled EchoLink instances claim the same callsign or network node.
    DuplicateEchoLinkAccount,
    /// Enabled EchoLink transport endpoints conflict.
    EchoLinkEndpointConflict,
}

/// A value-free conflict between known protocol resource bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolProfileError {
    /// Affected protocol resource.
    pub resource: ResourceBinding,
    /// Other resource making the conflicting claim.
    pub other: ResourceBinding,
    /// Conflict classification.
    pub problem: ProtocolProfileProblem,
}

impl fmt::Display for ProtocolProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let problem = match self.problem {
            ProtocolProfileProblem::DuplicateAllStarLinkNode => "duplicate AllStarLink node number",
            ProtocolProfileProblem::DuplicateEchoLinkAccount => {
                "duplicate EchoLink account identity"
            }
            ProtocolProfileProblem::EchoLinkEndpointConflict => {
                "conflicting EchoLink transport endpoint"
            }
        };
        write!(formatter, "{}: {problem}", self.resource)
    }
}

impl std::error::Error for ProtocolProfileError {}

/// Owned public local and effective protocol profiles for one candidate.
#[derive(Debug, Default)]
pub struct ResolvedProtocolProfiles {
    site: ResolvedProtocolAccounts,
    local: BTreeMap<ResourceId, NormalizedProfileResource>,
    ports: BTreeMap<ResourceId, ResolvedProtocolPort>,
    warnings: Vec<CandidateWarning>,
}

impl ResolvedProtocolProfiles {
    /// Typed public account defaults owned by the site, even when there are no ports.
    pub fn site_accounts(&self) -> &ResolvedProtocolAccounts {
        &self.site
    }

    /// Find the normalized local body by registered protocol resource ID.
    pub fn local(&self, id: ResourceId) -> Option<&NormalizedProfileResource> {
        self.local.get(&id)
    }

    /// Iterate normalized local bodies for captured defaults and explicit instances.
    pub fn locals(&self) -> impl Iterator<Item = &NormalizedProfileResource> {
        self.local.values()
    }

    /// Find effective protocol settings by registered port ID.
    pub fn port(&self, id: ResourceId) -> Option<&ResolvedProtocolPort> {
        self.ports.get(&id)
    }

    /// Iterate port profiles with their stable owner UUIDs.
    pub fn ports(&self) -> impl Iterator<Item = (ResourceId, &ResolvedProtocolPort)> {
        self.ports.iter().map(|(&id, port)| (id, port))
    }

    /// Recoverable candidate and profile diagnostics with their source bindings.
    pub fn warnings(&self) -> &[CandidateWarning] {
        &self.warnings
    }
}

impl ModularCandidate<'_> {
    /// Normalize public local protocol bodies and resolve all explicit port instances.
    pub fn resolve_protocol_profiles(
        &self,
    ) -> Result<ResolvedProtocolProfiles, ProtocolProfileError> {
        let registry = self.registry();
        let site = ResourceScope::Site(registry.site_id());
        let mut result = ResolvedProtocolProfiles {
            warnings: self.warnings().to_vec(),
            ..ResolvedProtocolProfiles::default()
        };
        for binding in registry.bindings() {
            let schema = match binding.kind {
                ResourceKind::Allstarlink => allstarlink::FIELDS,
                ResourceKind::Echolink => echolink::FIELDS,
                ResourceKind::EcholinkProxy => echolink::PROXY_FIELDS,
                ResourceKind::PhonePatch => phone::FIELDS,
                _ => continue,
            };
            if let Some(resource) = self.staged(binding.scope, binding.kind) {
                let configuration =
                    normalize(binding, resource.fields(), schema, &mut result.warnings);
                result.local.insert(
                    binding.id,
                    NormalizedProfileResource {
                        binding,
                        configuration,
                    },
                );
            }
        }
        result.site = resolve_accounts(
            |kind| {
                let local = result.local.get(&registry.binding(site, kind)?.id)?;
                Some(Layers { local, site: None })
            },
            &mut result.warnings,
        );
        for (key, id) in registry.port_keys() {
            let mut warnings = Vec::new();
            let scope = ResourceScope::Port(id);
            let layers = |kind| {
                let local = result.local.get(&registry.binding(scope, kind)?.id)?;
                let site = registry
                    .binding(site, kind)
                    .and_then(|binding| result.local.get(&binding.id));
                Some(Layers { local, site })
            };
            let ResolvedProtocolAccounts {
                allstarlink,
                echolink,
                echolink_proxy,
                phone_patch,
            } = resolve_accounts(layers, &mut warnings);
            if let Some(echo) = &echolink {
                let local = result
                    .local
                    .get_mut(&echo.binding.id)
                    .expect("explicit EchoLink body");
                echolink::trim_inactive(echo.settings.transport, &mut local.configuration);
            }
            let metadata = self
                .port_metadata(id)
                .expect("assembled port metadata")
                .effective(self.site_metadata());
            result.warnings.extend(warnings.iter().copied());
            result.ports.insert(
                id,
                ResolvedProtocolPort {
                    key: key.clone(),
                    metadata,
                    peer_audio: registry.binding(scope, ResourceKind::PeerAudio),
                    allstarlink,
                    echolink,
                    echolink_proxy,
                    phone_patch,
                    warnings,
                },
            );
        }
        validate_ownership(result.ports.values())?;
        Ok(result)
    }
}

fn resolve_accounts<'a>(
    layers: impl Fn(ResourceKind) -> Option<Layers<'a>>,
    warnings: &mut Vec<CandidateWarning>,
) -> ResolvedProtocolAccounts {
    let allstarlink = layers(ResourceKind::Allstarlink).map(|layers| {
        profile(&layers, warnings, |layers, _| {
            AllStarLinkProfileSettings::resolve(layers)
        })
    });
    let proxy = layers(ResourceKind::EcholinkProxy);
    let echolink = layers(ResourceKind::Echolink).map(|layers| {
        profile(&layers, warnings, |parent, warnings| {
            echolink::resolve(parent, proxy.as_ref(), warnings)
        })
    });
    let echolink_proxy = echolink
        .as_ref()
        .and(proxy.map(|layers| layers.local.binding));
    let phone_patch = layers(ResourceKind::PhonePatch)
        .map(|layers| profile(&layers, warnings, PhonePatchProfileSettings::resolve));

    ResolvedProtocolAccounts {
        allstarlink,
        echolink,
        echolink_proxy,
        phone_patch,
    }
}

fn profile<T>(
    layers: &Layers<'_>,
    warnings: &mut Vec<CandidateWarning>,
    resolve: impl FnOnce(&Layers<'_>, &mut Vec<CandidateWarning>) -> T,
) -> ProtocolProfile<T> {
    ProtocolProfile {
        binding: layers.local.binding,
        settings: resolve(layers, warnings),
        jitter: JitterProfile::resolve(layers, warnings),
    }
}

fn validate_ownership<'a>(
    ports: impl Iterator<Item = &'a ResolvedProtocolPort>,
) -> Result<(), ProtocolProfileError> {
    let mut allstarlink = BTreeMap::new();
    let mut echo_callsigns = BTreeMap::new();
    let mut echo_numbers = BTreeMap::new();
    let mut echo_endpoints: Vec<&ProtocolProfile<EchoLinkSettings>> = Vec::new();
    for port in ports.filter(|port| port.metadata.enabled) {
        if let Some(profile) = &port.allstarlink
            && let Some(node) = profile.settings.node_number
            && let Some(other) = allstarlink.insert(node, profile.binding)
        {
            return Err(ProtocolProfileError {
                resource: profile.binding,
                other,
                problem: ProtocolProfileProblem::DuplicateAllStarLinkNode,
            });
        }
        let Some(profile) = port
            .echolink
            .as_ref()
            .filter(|profile| profile.settings.enabled)
        else {
            continue;
        };
        let echo = &profile.settings;
        if let Some(other) = echo_callsigns
            .insert(echo.callsign.as_str(), profile.binding)
            .or_else(|| echo_numbers.insert(echo.node_number, profile.binding))
        {
            return Err(ProtocolProfileError {
                resource: profile.binding,
                other,
                problem: ProtocolProfileProblem::DuplicateEchoLinkAccount,
            });
        }
        for previous in &echo_endpoints {
            if echo.shares_endpoint(&previous.settings) {
                return Err(ProtocolProfileError {
                    resource: profile.binding,
                    other: previous.binding,
                    problem: ProtocolProfileProblem::EchoLinkEndpointConflict,
                });
            }
        }
        echo_endpoints.push(profile);
    }
    Ok(())
}

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;

#[cfg(test)]
mod public_policy_tests;
