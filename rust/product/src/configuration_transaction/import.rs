//! Resolve a fully captured working inventory against the accepted registry.
use super::*;
use rpt_advanced_core::config::resources::{
    PortKey,
    candidate::{
        CapturedResource, ModularCandidate, ResourceBinding, ResourceKind, ResourceRegistry,
        ResourceScope,
    },
    catalog::Member,
    decode_object,
};
use serde_json::Value;

/// Staged reload bindings and envelope bytes. Domain/profile validation precedes commit.
pub struct ReloadDraft {
    registry: ResourceRegistry,
    objects: BTreeMap<ResourceId, Vec<u8>>,
}
impl ReloadDraft {
    /// Borrow the import's authoritative identities and ordered members.
    pub fn registry(&self) -> &ResourceRegistry {
        &self.registry
    }
    /// Borrow the captured or ID-completed envelope for one resource.
    pub fn envelope(&self, id: ResourceId) -> Option<&[u8]> {
        self.objects.get(&id).map(Vec::as_slice)
    }
    /// Cross-check all surviving envelope identities and required scope metadata.
    pub fn assemble(&self) -> Result<ModularCandidate<'_>, TransactionError> {
        let captures = self
            .objects
            .iter()
            .map(|(id, json)| CapturedResource {
                binding: self
                    .registry
                    .by_id(*id)
                    .expect("import registered every envelope"),
                json,
            })
            .collect::<Vec<_>>();
        ModularCandidate::assemble(&self.registry, &captures)
            .map_err(|_| TransactionError::InvalidCandidate)
    }
}

fn allocate() -> Result<ResourceId, TransactionError> {
    rptadv_peer_support::working_files::transaction::new_identity()
        .map_err(capture_error)?
        .to_string()
        .parse()
        .map_err(|_| TransactionError::StorageFault)
}

/// Assign fresh stable IDs only to explicit new-token requests and newly
/// populated fixed slots. Existing UUIDs, owners, and exact keys are authoritative.
pub fn import_working(
    snapshot: &WorkingSnapshot,
    old: &ResourceRegistry,
) -> Result<ReloadDraft, TransactionError> {
    let invalid = TransactionError::InvalidCandidate;
    if snapshot.directories().is_none() {
        return Err(invalid);
    }
    let mut files = Vec::new();
    let existing_ports: BTreeMap<_, _> = old.port_keys().map(|(key, id)| (id, key)).collect();
    let mut ports = BTreeMap::new();
    let mut fresh_ports = Vec::new();
    let mut new_ports = BTreeMap::new();
    for (path, bytes) in snapshot.files() {
        let Some(CatalogPath::File(file)) = catalog::classify(path.to_str().ok_or(invalid)?) else {
            return Err(invalid);
        };
        let fields = decode_object(bytes).map_err(|_| invalid)?;
        if file.kind == ResourceKind::Metadata
            && let Some(port) = file.port
        {
            let (id, key) = match port {
                Member::Existing(id) => (id, (*existing_ports.get(&id).ok_or(invalid)?).clone()),
                Member::New(token) => {
                    let id = allocate()?;
                    let key = fields.get("key").and_then(Value::as_str).ok_or(invalid)?;
                    new_ports.insert(token, id);
                    fresh_ports.push(id);
                    (id, PortKey::new(key))
                }
            };
            ports.insert(id, key);
        }
        files.push((file, fields, bytes));
    }
    let mut objects = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let mut fresh = Vec::new();
    for (file, mut fields, original) in files {
        let scope = match file.port {
            None => ResourceScope::Site(old.site_id()),
            Some(Member::Existing(id)) if ports.contains_key(&id) => ResourceScope::Port(id),
            Some(Member::New(token)) => ResourceScope::Port(*new_ports.get(token).ok_or(invalid)?),
            _ => return Err(invalid),
        };
        let existing = match file.member {
            Some(Member::Existing(id)) => {
                let binding = old.by_id(id).ok_or(invalid)?;
                if binding.scope != scope || binding.kind != file.kind {
                    return Err(invalid);
                }
                Some(binding)
            }
            Some(Member::New(_)) => None,
            None => old.binding(scope, file.kind),
        };
        let (binding, bytes) = if let Some(binding) = existing {
            (binding, original.to_vec())
        } else {
            if fields.contains_key("id") {
                return Err(invalid);
            }
            let id = match (scope, file.kind) {
                (ResourceScope::Port(id), ResourceKind::Metadata) => id,
                _ => allocate()?,
            };
            let binding = ResourceBinding {
                id,
                scope,
                kind: file.kind,
            };
            fields.insert("id".into(), Value::String(id.to_string()));
            if file.kind.is_named() {
                let key = fields.get("key").and_then(Value::as_str).ok_or(invalid)?;
                fresh.push((binding, PortKey::new(key)));
            }
            (binding, Value::Object(fields).to_string().into_bytes())
        };
        if objects.insert(binding.id, bytes).is_some() {
            return Err(invalid);
        }
        bindings.insert(binding.id, binding);
    }
    // Absent ordinary overrides preserve their slot. A complete missing port,
    // protocol instance or named definition retires from the candidate.
    for b in old.bindings().filter(|b| {
        b.kind != ResourceKind::Metadata
            && (!b.kind.has_optional_presence()
                || matches!(b.scope, ResourceScope::Site(_)) && !b.kind.is_named())
    }) {
        if matches!(b.scope,ResourceScope::Port(id) if !ports.contains_key(&id)) {
            continue;
        }
        bindings.entry(b.id).or_insert(b);
    }
    let mut members: Vec<_> = old
        .members()
        .iter()
        .filter(|(b, _)| objects.contains_key(&b.id))
        .cloned()
        .collect();
    members.extend(fresh);
    let ordinary = bindings
        .into_values()
        .filter(|b| !b.kind.is_named() && b.kind != ResourceKind::Metadata)
        .collect::<Vec<_>>();
    let ports = old
        .port_keys()
        .map(|(_, id)| id)
        .chain(fresh_ports)
        .filter_map(|id| Some((id, ports.remove(&id)?)))
        .collect::<Vec<_>>();
    let registry = ResourceRegistry::with_members(old.site_id(), &ports, &ordinary, &members)
        .map_err(|_| invalid)?;
    let draft = ReloadDraft { registry, objects };
    draft.assemble()?;
    Ok(draft)
}
