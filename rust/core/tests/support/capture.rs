use crate::config::resources::{
    PortKey,
    candidate::{
        CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
        ResourceRegistry, ResourceScope,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) struct Fixture {
    pub(crate) registry: ResourceRegistry,
    bytes: Vec<(ResourceBinding, Vec<u8>)>,
}
impl Fixture {
    pub(crate) fn new(
        stations: &[(&str, &str)],
        resources: &[(&str, ResourceKind, &str, Value)],
    ) -> Self {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let site: ResourceId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap();
        let ports: Vec<_> = stations
            .iter()
            .map(|(id, key)| (id.parse().unwrap(), PortKey::new(*key)))
            .collect();
        let mut slots = Vec::new();
        let mut members = Vec::new();
        let mut bodies = BTreeMap::new();
        for (id, key) in std::iter::once(&(site, PortKey::new("Site"))).chain(ports.iter()) {
            let mut body = json!({"id":id.to_string(),"kind":"metadata","configuration":{
                "name":key.as_str(),"inherit_from":if *id==site {Value::Null} else {json!("site")}
            }});
            if *id != site {
                body["key"] = json!(key.as_str());
            }
            bodies.insert(*id, body);
        }
        for (owner, kind, key, settings) in resources {
            let owner_id = owner.parse().unwrap();
            if *kind == ResourceKind::Metadata {
                let metadata = bodies.get_mut(&owner_id).expect("declared owner");
                metadata["configuration"]
                    .as_object_mut()
                    .unwrap()
                    .extend(settings.as_object().unwrap().clone());
                continue;
            }
            let scope = if owner_id == site {
                ResourceScope::Site(site)
            } else {
                ResourceScope::Port(owner_id)
            };
            let mut identity = DefaultHasher::new();
            (owner, kind.as_str(), key).hash(&mut identity);
            let id = format!("{:032x}", identity.finish()).parse().unwrap();
            let binding = ResourceBinding {
                id,
                kind: *kind,
                scope,
            };
            let mut body = if *kind == ResourceKind::Event {
                settings.clone()
            } else {
                json!({"configuration":settings})
            };
            body["id"] = json!(id.to_string());
            body["kind"] = json!(kind.as_str());
            if kind.is_named() {
                assert!(!key.is_empty());
                members.push((binding, PortKey::new(*key)));
                body["key"] = json!(key);
            } else {
                assert!(key.is_empty());
                slots.push(binding);
            }
            assert!(
                bodies.insert(id, body).is_none(),
                "duplicate fixture identity"
            );
        }
        let registry = ResourceRegistry::with_members(site, &ports, &slots, &members).unwrap();
        let bytes: Vec<_> = registry
            .bindings()
            .map(|b| (b, serde_json::to_vec(&bodies[&b.id]).unwrap()))
            .collect();
        Self { registry, bytes }
    }
    pub(crate) fn candidate(&self) -> ModularCandidate<'_> {
        let captured: Vec<_> = self
            .bytes
            .iter()
            .map(|(binding, json)| CapturedResource {
                binding: *binding,
                json,
            })
            .collect();
        ModularCandidate::assemble(&self.registry, &captured).unwrap()
    }
}
