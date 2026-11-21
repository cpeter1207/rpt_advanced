//! A disposable edit to the verified registry; the guarded transaction owns publication.
use super::Fault;
use crate::configuration_manifest::LoadedPublicManifest;
use rpt_advanced_core::config::resources::{
    PortKey,
    candidate::{
        CapturedResource, ModularCandidate, ResourceBinding as Binding, ResourceId as Id,
        ResourceKind as Kind, ResourceScope as Scope,
    },
    manifest::{ContentHash, ObjectReference, PublicManifest},
};
use rptadv_peer_support::working_files::{immutable::artifact_sha256, transaction::new_identity};
use serde_json::{Map, Value, json};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

pub(super) fn fresh() -> Result<Id, Fault> {
    new_identity()
        .map_err(|_| Fault::Storage)?
        .to_string()
        .parse()
        .map_err(|_| Fault::Storage)
}

struct Row<'a> {
    binding: Binding,
    reference: Option<ObjectReference>,
    envelope: Option<Cow<'a, [u8]>>,
}

/// Internal staging bytes never cross the public representation boundary.
pub(super) struct Graph<'a> {
    site: Id,
    ports: Vec<(Id, PortKey)>,
    members: Vec<(Id, PortKey)>,
    rows: BTreeMap<Id, Row<'a>>,
}

impl<'a> Graph<'a> {
    pub fn selected(loaded: &'a LoadedPublicManifest) -> Self {
        let manifest = loaded.manifest();
        let registry = manifest.registry();
        let bytes: BTreeMap<_, _> = loaded.verified_objects().collect();
        let rows = registry
            .bindings()
            .map(|b| {
                (
                    b.id,
                    Row {
                        binding: b,
                        reference: manifest.reference(b.id).cloned(),
                        envelope: bytes.get(&b.id).copied().map(Cow::Borrowed),
                    },
                )
            })
            .collect();
        Self {
            site: registry.site_id(),
            ports: registry
                .port_keys()
                .map(|(k, id)| (id, k.clone()))
                .collect(),
            members: registry
                .members()
                .iter()
                .map(|(b, k)| (b.id, k.clone()))
                .collect(),
            rows,
        }
    }

    pub fn import(
        draft: &'a crate::configuration_transaction::import::ReloadDraft,
        old: &'a LoadedPublicManifest,
    ) -> Result<Self, Fault> {
        let registry = draft.registry();
        let manifest = old.manifest();
        let mut rows = BTreeMap::new();
        for b in registry.bindings() {
            let bytes = draft.envelope(b.id);
            if let Some(bytes) = bytes
                && !crate::configuration_private::public_input_envelope(b, bytes)
            {
                return Err(Fault::Invalid);
            }
            let (reference, envelope) = match bytes {
                None => (None, None),
                Some(bytes) => {
                    let hash = ContentHash::from_bytes(artifact_sha256(bytes));
                    match manifest.reference(b.id).filter(|r| r.sha256() == &hash) {
                        Some(r) => (Some(r.clone()), Some(Cow::Borrowed(bytes))),
                        None => (
                            Some(ObjectReference::new(
                                fresh()?.to_string().parse().map_err(|_| Fault::Storage)?,
                                hash,
                            )),
                            Some(Cow::Owned(bytes.to_vec())),
                        ),
                    }
                }
            };
            rows.insert(
                b.id,
                Row {
                    binding: b,
                    reference,
                    envelope,
                },
            );
        }
        Ok(Self {
            site: registry.site_id(),
            ports: registry
                .port_keys()
                .map(|(k, id)| (id, k.clone()))
                .collect(),
            members: registry
                .members()
                .iter()
                .map(|(b, k)| (b.id, k.clone()))
                .collect(),
            rows,
        })
    }

    pub fn members(&self, scope: Scope, kind: Kind) -> Vec<Id> {
        if kind == Kind::Metadata {
            return self.ports.iter().map(|(id, _)| *id).collect();
        }
        if kind.is_named() {
            return self
                .members
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| {
                    self.rows[id].binding.scope == scope && self.rows[id].binding.kind == kind
                })
                .collect();
        }
        self.slot(scope, kind).map(|b| b.id).into_iter().collect()
    }

    fn slot(&self, scope: Scope, kind: Kind) -> Option<Binding> {
        self.rows
            .values()
            .map(|row| row.binding)
            .find(|b| b.scope == scope && b.kind == kind)
    }

    fn insert(&mut self, binding: Binding) -> Result<Binding, Fault> {
        if self.rows.contains_key(&binding.id) {
            return Err(Fault::Conflict);
        }
        self.rows.insert(
            binding.id,
            Row {
                binding,
                reference: None,
                envelope: None,
            },
        );
        Ok(binding)
    }

    pub fn ensure_section(&mut self, scope: Scope, kind: Kind) -> Result<Binding, Fault> {
        if let Some(binding) = self.slot(scope, kind) {
            return Ok(binding);
        }
        self.insert(Binding {
            id: fresh()?,
            scope,
            kind,
        })
    }

    pub fn create(
        &mut self,
        scope: Scope,
        kind: Kind,
        key: Option<PortKey>,
    ) -> Result<Binding, Fault> {
        if kind == Kind::Metadata {
            let key = key.ok_or(Fault::Invalid)?;
            if self.ports.iter().any(|(_, existing)| *existing == key) {
                return Err(Fault::Conflict);
            }
            let id = fresh()?;
            let scope = Scope::Port(id);
            let binding = self.insert(Binding { id, scope, kind })?;
            self.ports.push((id, key));
            for ordinary in
                Kind::catalog().filter(|k| *k != Kind::Metadata && !k.has_optional_presence())
            {
                self.ensure_section(scope, ordinary)?;
            }
            return Ok(binding);
        }
        if kind.is_named() {
            let key = key.ok_or(Fault::Invalid)?;
            if self.members.iter().any(|(id, k)| {
                *k == key
                    && self.rows[id].binding.kind == kind
                    && self.rows[id].binding.scope == scope
            }) {
                return Err(Fault::Conflict);
            }
            let binding = self.insert(Binding {
                id: fresh()?,
                scope,
                kind,
            })?;
            self.members.push((binding.id, key));
            return Ok(binding);
        }
        if key.is_some() || self.slot(scope, kind).is_some() {
            return Err(Fault::Conflict);
        }
        let binding = self.ensure_section(scope, kind)?;
        if kind == Kind::Echolink {
            let proxy = self.ensure_section(scope, Kind::EcholinkProxy)?;
            self.replace(proxy, Map::new())?;
        }
        Ok(binding)
    }

    pub fn set_members(&mut self, scope: Scope, kind: Kind, requested: &[Id]) -> Result<(), Fault> {
        let existing = self.members(scope, kind);
        let allowed: BTreeSet<_> = existing.iter().copied().collect();
        let kept: BTreeSet<_> = requested.iter().copied().collect();
        if kept.len() != requested.len() || !kept.is_subset(&allowed) {
            return Err(Fault::Conflict);
        }
        let removed: BTreeSet<_> = allowed.difference(&kept).copied().collect();
        let retire_parent = kind == Kind::Echolink && !removed.is_empty();
        self.rows.retain(|id,row|{
            let removed_with_port=matches!(row.binding.scope,Scope::Port(owner) if kind==Kind::Metadata && removed.contains(&owner));
            let removed_with_account=retire_parent && row.binding.scope==scope && row.binding.kind==Kind::EcholinkProxy;
            !(removed.contains(id) || removed_with_port || removed_with_account)
        });
        self.members.retain(|(id, _)| self.rows.contains_key(id));
        if kind == Kind::Metadata {
            let mut keys: BTreeMap<_, _> = self.ports.drain(..).collect();
            self.ports = requested
                .iter()
                .map(|id| (*id, keys.remove(id).expect("validated port")))
                .collect();
        } else if kind.is_named() {
            let mut keys: BTreeMap<_, _> = self
                .members
                .iter()
                .filter(|(id, _)| allowed.contains(id))
                .cloned()
                .collect();
            let mut order = requested.iter();
            for entry in self
                .members
                .iter_mut()
                .filter(|(id, _)| allowed.contains(id))
            {
                let id = *order.next().expect("validated collection order");
                *entry = (id, keys.remove(&id).expect("validated named member"));
            }
        }
        Ok(())
    }

    pub fn replace(&mut self, binding: Binding, fields: Map<String, Value>) -> Result<(), Fault> {
        let mut envelope = if binding.kind == Kind::Event {
            Value::Object(fields)
        } else {
            json!({"configuration":fields})
        };
        envelope["id"] = json!(binding.id.to_string());
        envelope["kind"] = json!(binding.kind.as_str());
        let key = if binding.kind == Kind::Metadata {
            self.ports.iter()
        } else {
            self.members.iter()
        }
        .find(|(id, _)| *id == binding.id)
        .map(|(_, key)| key.as_str());
        if let Some(key) = key {
            envelope["key"] = json!(key);
        }
        let bytes = serde_json::to_vec(&envelope).map_err(|_| Fault::Invalid)?;
        let row = self.rows.get_mut(&binding.id).ok_or(Fault::Missing)?;
        if row.envelope.as_deref() != Some(&bytes) {
            let hash = ContentHash::from_bytes(artifact_sha256(&bytes));
            row.reference = Some(ObjectReference::new(
                fresh()?.to_string().parse().map_err(|_| Fault::Storage)?,
                hash,
            ));
            row.envelope = Some(Cow::Owned(bytes));
        }
        Ok(())
    }

    pub fn manifest(&self, revision: Id) -> Result<PublicManifest, Fault> {
        let name = revision.to_string();
        PublicManifest::with_members(
            name.parse().map_err(|_| Fault::Invalid)?,
            name.parse().map_err(|_| Fault::Invalid)?,
            self.site,
            &self.ports,
            self.rows.values().map(|r| (r.binding, r.reference.clone())),
            &self.members,
        )
        .map_err(|_| Fault::Conflict)
    }

    pub fn assemble<'m>(
        &self,
        manifest: &'m PublicManifest,
    ) -> Result<ModularCandidate<'m>, Fault> {
        let captures = self
            .rows
            .values()
            .filter_map(|r| {
                Some(CapturedResource {
                    binding: r.binding,
                    json: r.envelope.as_deref()?,
                })
            })
            .collect::<Vec<_>>();
        ModularCandidate::assemble(manifest.registry(), &captures).map_err(|_| Fault::Invalid)
    }

    pub fn changed(&self) -> Vec<(Id, &[u8])> {
        self.rows
            .iter()
            .filter_map(|(id, r)| match &r.envelope {
                Some(Cow::Owned(bytes)) => Some((*id, bytes.as_slice())),
                _ => None,
            })
            .collect()
    }
}
