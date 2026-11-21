//! In-memory assembly of UUID-bound local configuration resources.

use super::{
    JsonResourceError, LocalMetadata, PortKey, PortMetadata, ResourceResolution, ResourceWarning,
    ScopeParent, SiteMetadata, decode_object, into_string,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, btree_map::Entry};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// Stable resource identity assigned by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(Uuid);

impl ResourceId {
    /// Typed UUID for descriptor-relative immutable and journal operations.
    pub fn uuid(self) -> Uuid {
        self.0
    }
}

impl FromStr for ResourceId {
    type Err = InvalidResourceId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text)
            .map(Self)
            .map_err(|_| InvalidResourceId)
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An invalid UUID, with the supplied text omitted from diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidResourceId;

impl fmt::Display for InvalidResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("resource ID must be a UUID")
    }
}

impl std::error::Error for InvalidResourceId {}

/// Resource owner and its role. The UUID is also that owner's metadata ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceScope {
    /// The single site owner.
    Site(ResourceId),
    /// An exact local port owner.
    Port(ResourceId),
}

impl ResourceScope {
    pub(super) fn id(self) -> ResourceId {
        let (Self::Site(id) | Self::Port(id)) = self;
        id
    }

    fn metadata_binding(self) -> ResourceBinding {
        ResourceBinding {
            id: self.id(),
            scope: self,
            kind: ResourceKind::Metadata,
        }
    }
}

/// Kinds accepted by the modular section envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    /// Scope identity metadata and common settings.
    Metadata,
    /// General radio settings.
    Radio,
    /// Receive audio.
    RxAudio,
    /// Receive signaling.
    RxSignaling,
    /// Transmit audio.
    TxAudio,
    /// Transmit signaling.
    TxSignaling,
    /// The radio interface.
    Interface,
    /// Protocol-neutral peer audio.
    PeerAudio,
    /// Radio control.
    Control,
    /// Remote-base memories.
    Memories,
    /// Remote-base scan lists.
    ScanLists,
    /// AllStarLink defaults or an explicit instance.
    Allstarlink,
    /// EchoLink defaults or an explicit instance.
    Echolink,
    /// Phone-patch defaults or an explicit instance.
    PhonePatch,
    /// command resource.
    Command,
    /// limits resource.
    Limits,
    /// power resource.
    Power,
    /// event resource.
    Event,
    /// macro resource.
    Macro,
    /// message resource.
    Message,
    /// Scalar speech, Morse, ducking and clock defaults for this scope.
    TelemetrySettings,
    /// Named periodic identification recipe.
    Identifier,
    /// Named release announcement.
    Announcement,
    /// Named composable tail.
    Tail,
    /// Named receiver or peer courtesy.
    Courtesy,
    /// sensor resource.
    Sensor,
    /// input resource.
    Input,
    /// output resource.
    Output,
    /// alarm resource.
    Alarm,
    /// fan resource.
    Fan,
    /// mailbox resource.
    Mailbox,
    /// permanent-group resource.
    PermanentGroup,
    /// echolink-proxy resource.
    EcholinkProxy,
}

impl ResourceKind {
    const PROTOCOLS: [Self; 3] = [Self::Allstarlink, Self::Echolink, Self::PhonePatch];

    /// Identify protocol defaults and instances using the candidate's presence rules.
    pub(super) fn is_protocol(self) -> bool {
        Self::PROTOCOLS.contains(&self)
    }
}

/// Authoritative resource ID, owner and kind supplied by the registry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ResourceBinding {
    /// Stable ID of this resource.
    pub id: ResourceId,
    /// Scope that owns this resource.
    pub scope: ResourceScope,
    /// Kind of resource within that scope.
    pub kind: ResourceKind,
}

impl fmt::Display for ResourceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let role = match self.scope {
            ResourceScope::Site(_) => "site",
            ResourceScope::Port(_) => "port",
        };
        write!(formatter, "{role} {}", self.kind.as_str())
    }
}

impl fmt::Debug for ResourceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Exact port-key lookup and unique resource slots for one site.
#[derive(Clone, PartialEq, Eq)]
pub struct ResourceRegistry {
    pub(super) port_order: Vec<(ResourceId, PortKey)>,
    site: ResourceId,
    ports: BTreeMap<PortKey, ResourceId>,
    slots: BTreeMap<(ResourceScope, ResourceKind), ResourceId>,
    by_id: BTreeMap<ResourceId, ResourceBinding>,
    named_keys: BTreeMap<(ResourceScope, ResourceKind, PortKey), ResourceId>,
    members: Vec<(ResourceBinding, PortKey)>,
}

impl ResourceRegistry {
    /// Register the site, ports and additional bindings as one immutable registry.
    ///
    /// The site and every port own a metadata slot with their respective ID.
    pub fn new(
        site: ResourceId,
        ports: &[(ResourceId, PortKey)],
        bindings: &[ResourceBinding],
    ) -> Result<Self, CandidateError> {
        Self::with_members(site, ports, bindings, &[])
    }

    /// Register repeatable members in collection order, independently of exact key spelling.
    pub fn with_members(
        site: ResourceId,
        ports: &[(ResourceId, PortKey)],
        bindings: &[ResourceBinding],
        members: &[(ResourceBinding, PortKey)],
    ) -> Result<Self, CandidateError> {
        let site_binding = ResourceScope::Site(site).metadata_binding();
        let mut registry = Self {
            site,
            ports: BTreeMap::new(),
            port_order: ports.to_vec(),
            by_id: BTreeMap::from([(site, site_binding)]),
            named_keys: BTreeMap::new(),
            members: Vec::new(),
            slots: BTreeMap::from([((site_binding.scope, site_binding.kind), site_binding.id)]),
        };
        for (id, key) in ports {
            let binding = ResourceScope::Port(*id).metadata_binding();
            registry.insert(binding)?;
            if registry.ports.insert(key.clone(), *id).is_some() {
                return Err(binding.error(CandidateProblem::DuplicatePortKey));
            }
        }
        for &binding in bindings {
            if registry
                .binding(binding.scope, ResourceKind::Metadata)
                .is_none()
            {
                return Err(binding.error(CandidateProblem::InvalidOwner));
            }
            if binding.kind.is_named() {
                return Err(binding.error(CandidateProblem::UnexpectedBinding));
            }
            registry.insert(binding)?;
        }
        for binding in bindings.iter().filter(|b| {
            b.kind == ResourceKind::EcholinkProxy && matches!(b.scope, ResourceScope::Port(_))
        }) {
            if registry
                .binding(binding.scope, ResourceKind::Echolink)
                .is_none()
            {
                return Err(binding.error(CandidateProblem::InvalidOwner));
            }
        }
        for (binding, key) in members {
            if !binding.kind.is_named()
                || registry
                    .binding(binding.scope, ResourceKind::Metadata)
                    .is_none()
            {
                return Err(binding.error(CandidateProblem::InvalidOwner));
            }
            if registry.by_id.insert(binding.id, *binding).is_some() {
                return Err(binding.error(CandidateProblem::DuplicateId));
            }
            if registry
                .named_keys
                .insert((binding.scope, binding.kind, key.clone()), binding.id)
                .is_some()
            {
                return Err(binding.error(CandidateProblem::DuplicateSlot));
            }
            registry.members.push((*binding, key.clone()));
        }
        Ok(registry)
    }

    fn insert(&mut self, binding: ResourceBinding) -> Result<(), CandidateError> {
        if self.by_id.insert(binding.id, binding).is_some() {
            return Err(binding.error(CandidateProblem::DuplicateId));
        }
        if self
            .slots
            .insert((binding.scope, binding.kind), binding.id)
            .is_some()
        {
            return Err(binding.error(CandidateProblem::DuplicateSlot));
        }
        Ok(())
    }

    /// Return the site's UUID, which is also its metadata resource ID.
    pub fn site_id(&self) -> ResourceId {
        self.site
    }

    /// Iterate exact keys and UUIDs in the persisted port collection order.
    pub fn port_keys(&self) -> impl Iterator<Item = (&PortKey, ResourceId)> + '_ {
        self.port_order.iter().map(|(id, key)| (key, *id))
    }

    /// Find a port by the exact registered key.
    pub fn port_id(&self, key: &PortKey) -> Option<ResourceId> {
        self.ports.get(key).copied()
    }

    /// Find the expected binding for this scope and kind.
    pub fn binding(&self, scope: ResourceScope, kind: ResourceKind) -> Option<ResourceBinding> {
        let id = *self.slots.get(&(scope, kind))?;
        Some(ResourceBinding { id, scope, kind })
    }

    /// Iterate all registered bindings, including the site and port metadata.
    pub fn bindings(&self) -> impl ExactSizeIterator<Item = ResourceBinding> + '_ {
        self.by_id.values().copied()
    }

    /// Find any registered ID, including a named member.
    pub fn by_id(&self, id: ResourceId) -> Option<ResourceBinding> {
        self.by_id.get(&id).copied()
    }

    /// Find a repeatable member by its exact owner, kind and key.
    pub fn named_binding(
        &self,
        scope: ResourceScope,
        kind: ResourceKind,
        key: &PortKey,
    ) -> Option<ResourceBinding> {
        self.by_id(*self.named_keys.get(&(scope, kind, key.clone()))?)
    }

    /// Select one complete mailbox identity at this scope, falling back to the complete site
    /// identity only when a local member of that number is absent.
    pub fn effective_mailbox(
        &self,
        scope: ResourceScope,
        number: &PortKey,
    ) -> Option<ResourceBinding> {
        self.by_id(scope.id())
            .filter(|binding| binding.scope == scope && binding.kind == ResourceKind::Metadata)?;
        self.named_binding(scope, ResourceKind::Mailbox, number)
            .or_else(|| {
                self.named_binding(
                    ResourceScope::Site(self.site),
                    ResourceKind::Mailbox,
                    number,
                )
            })
    }

    /// Ordered named bindings and exact local keys.
    pub fn members(&self) -> &[(ResourceBinding, PortKey)] {
        &self.members
    }
}

impl fmt::Debug for ResourceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceRegistry")
            .finish_non_exhaustive()
    }
}

/// Structural failure constructing a registry or assembling its capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateProblem {
    /// More than one resource attempts to use the same UUID.
    DuplicateId,
    /// More than one port attempts to use the same exact key.
    DuplicatePortKey,
    /// More than one resource fills one owner/kind slot.
    DuplicateSlot,
    /// The owner does not exist in the stated scope.
    InvalidOwner,
    /// A capture's identity does not match an expected binding.
    UnexpectedBinding,
    /// The same resource is present twice in the capture.
    DuplicateCapture,
    /// A site or port lacks its required local metadata.
    MissingMetadata,
    /// A required envelope field is absent, unusable or inconsistent.
    InvalidEnvelope(&'static str),
    /// The resource has ambiguous JSON or unusable local metadata.
    Decode(JsonResourceError),
}

/// Structural error attributed to a resource without exposing authored data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateError {
    /// The relevant resource context.
    pub resource: ResourceBinding,
    /// The reason assembly failed.
    pub problem: CandidateProblem,
}

impl ResourceBinding {
    fn error(self, problem: CandidateProblem) -> CandidateError {
        CandidateError {
            resource: self,
            problem,
        }
    }
}

impl fmt::Display for CandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: ", self.resource)?;
        match self.problem {
            CandidateProblem::DuplicateId => formatter.write_str("duplicate resource ID"),
            CandidateProblem::DuplicatePortKey => formatter.write_str("duplicate port key"),
            CandidateProblem::DuplicateSlot => formatter.write_str("duplicate owner/kind slot"),
            CandidateProblem::InvalidOwner => formatter.write_str("unknown owner for scope"),
            CandidateProblem::UnexpectedBinding => {
                formatter.write_str("unexpected capture binding")
            }
            CandidateProblem::DuplicateCapture => formatter.write_str("duplicate capture"),
            CandidateProblem::MissingMetadata => {
                formatter.write_str("required metadata is missing")
            }
            CandidateProblem::InvalidEnvelope(field) => {
                write!(formatter, "invalid envelope field {field}")
            }
            CandidateProblem::Decode(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CandidateError {}

/// Value-free warning from the resource envelope or typed metadata body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateWarning {
    /// The registry binding of the resource with ignored input.
    pub resource: ResourceBinding,
    /// The existing resource warning classification.
    pub warning: ResourceWarning,
}

impl fmt::Display for CandidateWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.resource, self.warning)
    }
}

/// Captured bytes associated with the caller's expected binding.
#[derive(Clone, Copy)]
pub struct CapturedResource<'a> {
    /// Identity, scope and kind independent of the bytes.
    pub binding: ResourceBinding,
    /// Captured resource document.
    pub json: &'a [u8],
}

/// Unvalidated local section or protocol body held for later domain validation.
pub struct StagedResource {
    captured: bool,
    binding: ResourceBinding,
    fields: Map<String, Value>,
}

impl StagedResource {
    /// The stable identity held by this staged resource.
    pub fn binding(&self) -> ResourceBinding {
        self.binding
    }

    /// Whether the input carried an explicit resource, including an empty body.
    pub fn was_captured(&self) -> bool {
        self.captured
    }

    pub(super) fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    /// Whether this resource has an empty local body.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl fmt::Debug for StagedResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedResource")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

/// Assembled metadata and opaque local section bodies for one registry.
pub struct ModularCandidate<'registry> {
    registry: &'registry ResourceRegistry,
    site: SiteMetadata,
    ports: BTreeMap<ResourceId, PortMetadata>,
    staged: BTreeMap<ResourceId, StagedResource>,
    warnings: Vec<CandidateWarning>,
}

impl<'registry> ModularCandidate<'registry> {
    /// Assemble captured documents without modifying the registry or the input.
    ///
    /// Missing ordinary sections retain their identities with empty overrides.
    /// Protocol defaults and instances are staged only when captured.
    pub fn assemble(
        registry: &'registry ResourceRegistry,
        captures: &[CapturedResource<'_>],
    ) -> Result<Self, CandidateError> {
        let mut captured = BTreeMap::new();
        let mut warnings = Vec::new();
        for &capture in captures {
            let binding = capture.binding;
            if registry.by_id(binding.id) != Some(binding) {
                return Err(binding.error(CandidateProblem::UnexpectedBinding));
            }
            let Entry::Vacant(slot) = captured.entry(binding.id) else {
                return Err(binding.error(CandidateProblem::DuplicateCapture));
            };
            slot.insert(decode_envelope(registry, capture, &mut warnings)?);
        }

        let site_binding = ResourceScope::Site(registry.site).metadata_binding();
        let site = SiteMetadata(take_metadata(
            &mut captured,
            site_binding,
            ScopeParent::BuiltIn,
            &mut warnings,
        )?);
        let mut ports = BTreeMap::new();
        for &port in registry.ports.values() {
            let binding = ResourceScope::Port(port).metadata_binding();
            let metadata = take_metadata(&mut captured, binding, ScopeParent::Site, &mut warnings)?;
            ports.insert(port, PortMetadata(metadata));
        }

        let mut staged = BTreeMap::new();
        for binding in registry.bindings() {
            let ResourceBinding { scope, kind, id } = binding;
            if kind == ResourceKind::Metadata {
                continue;
            }
            let fields = captured.remove(&id);
            if fields.is_some() || !kind.has_optional_presence() {
                staged.insert(
                    id,
                    StagedResource {
                        binding: ResourceBinding { id, scope, kind },
                        captured: fields.is_some(),
                        fields: fields.unwrap_or_default(),
                    },
                );
            }
        }
        Ok(Self {
            registry,
            site,
            ports,
            staged,
            warnings,
        })
    }

    /// The verified registry from which every binding in this candidate originates.
    pub fn registry(&self) -> &ResourceRegistry {
        self.registry
    }

    /// The site's sparse and effective typed metadata.
    pub fn site_metadata(&self) -> &SiteMetadata {
        &self.site
    }

    /// The specified port's sparse metadata, resolvable against the site's metadata.
    pub fn port_metadata(&self, port: ResourceId) -> Option<&PortMetadata> {
        self.ports.get(&port)
    }

    /// Borrow a named member's staging body within core domain preparation.
    pub(super) fn member_fields(&self, id: ResourceId) -> Option<&Map<String, Value>> {
        self.staged.get(&id).map(StagedResource::fields)
    }

    /// Inspect local-body presence for a bound ordinary section or protocol.
    pub fn staged(&self, scope: ResourceScope, kind: ResourceKind) -> Option<&StagedResource> {
        let binding = self.registry.binding(scope, kind)?;
        self.staged.get(&binding.id)
    }

    pub(super) fn staged_member(&self, binding: ResourceBinding) -> Option<&StagedResource> {
        self.staged
            .get(&binding.id)
            .filter(|r| r.binding == binding)
    }

    /// Iterate the port's explicitly present protocols, regardless of its enabled setting.
    pub fn port_protocols(&self, port: ResourceId) -> impl Iterator<Item = ResourceBinding> + '_ {
        ResourceKind::PROTOCOLS.into_iter().filter_map(move |kind| {
            self.staged(ResourceScope::Port(port), kind)
                .map(StagedResource::binding)
        })
    }

    /// Recoverable envelope and metadata diagnostics.
    pub fn warnings(&self) -> &[CandidateWarning] {
        &self.warnings
    }
}

impl fmt::Debug for ModularCandidate<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModularCandidate")
            .finish_non_exhaustive()
    }
}

fn decode_envelope(
    registry: &ResourceRegistry,
    capture: CapturedResource<'_>,
    warnings: &mut Vec<CandidateWarning>,
) -> Result<Map<String, Value>, CandidateError> {
    let binding = capture.binding;
    let mut envelope = decode_object(capture.json)
        .map_err(|error| binding.error(CandidateProblem::Decode(error)))?;
    let invalid = |field| binding.error(CandidateProblem::InvalidEnvelope(field));
    let declared_id = envelope
        .remove("id")
        .and_then(into_string)
        .and_then(|id| id.parse().ok());
    if declared_id != Some(binding.id) {
        return Err(invalid("id"));
    }
    if envelope.remove("kind").as_ref().and_then(Value::as_str) != Some(binding.kind.as_str()) {
        return Err(invalid("kind"));
    }
    if binding.kind.is_named() {
        let key = envelope
            .remove("key")
            .and_then(into_string)
            .ok_or_else(|| invalid("key"))?;
        if registry.named_binding(binding.scope, binding.kind, &PortKey::new(key)) != Some(binding)
        {
            return Err(invalid("key"));
        }
        if binding.kind == ResourceKind::Event {
            return Ok(envelope);
        }
    } else if let Some(key) = envelope.remove("key")
        && !(matches!(binding.scope, ResourceScope::Port(_))
            && binding.kind == ResourceKind::Metadata
            && into_string(key).and_then(|key| registry.port_id(&PortKey::new(key)))
                == Some(binding.id))
    {
        return Err(invalid("key"));
    }
    let Some(Value::Object(fields)) = envelope.remove("configuration") else {
        return Err(invalid("configuration"));
    };
    warnings.extend(std::iter::repeat_n(
        CandidateWarning {
            resource: binding,
            warning: ResourceWarning::UnknownField,
        },
        envelope.len(),
    ));
    Ok(fields)
}

fn take_metadata(
    captured: &mut BTreeMap<ResourceId, Map<String, Value>>,
    binding: ResourceBinding,
    parent: ScopeParent,
    warnings: &mut Vec<CandidateWarning>,
) -> Result<LocalMetadata, CandidateError> {
    let fields = captured
        .remove(&binding.id)
        .ok_or_else(|| binding.error(CandidateProblem::MissingMetadata))?;
    let ResourceResolution {
        value,
        warnings: local_warnings,
    } = LocalMetadata::normalize(fields, parent)
        .map_err(|error| binding.error(CandidateProblem::Decode(error)))?;
    warnings.extend(local_warnings.into_iter().map(|warning| CandidateWarning {
        resource: binding,
        warning,
    }));
    Ok(value)
}

#[cfg(test)]
#[path = "candidate_tests.rs"]
mod tests;
