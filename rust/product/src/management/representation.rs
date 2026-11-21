//! HAL documents derived from the same inventory as dispatch and description.
use super::{Fault, Snapshot};
use rpt_advanced_core::{
    config::resources::candidate::{
        ResourceId as Id, ResourceKind as Kind, ResourceScope as Scope,
    },
    management::routes::{self, Parameters, Route, Target, View},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) struct Api {
    pub mount: String,
    pub routes: BTreeMap<String, Route>,
    openapi: Value,
}

pub(super) fn binding_id(s: &Snapshot, r: &Route, p: Parameters<'_>) -> Option<Id> {
    let scope = p
        .port
        .map_or(Scope::Site(s.registry.site_id()), Scope::Port);
    match r.target {
        Target::Metadata => Some(p.port.unwrap_or(s.registry.site_id())),
        Target::Member(_) => p.member,
        Target::Section(k) | Target::Credentials(k) => {
            if k.is_named() {
                p.member
            } else {
                s.registry.binding(scope, k).map(|b| b.id)
            }
        }
        _ => None,
    }
}

impl Api {
    pub fn new(mount: &str) -> Result<Self, Fault> {
        if !mount.starts_with('/')
            || mount.split('/').skip(1).any(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        {
            return Err(Fault::Invalid);
        }
        let routes = routes::inventory();
        let openapi = super::openapi::document(mount, &routes);
        Ok(Self {
            mount: mount.to_owned(),
            routes,
            openapi,
        })
    }

    pub fn lookup<'a, 'b>(&'a self, path: &'b str) -> Result<(&'a Route, Parameters<'b>), Fault> {
        let relative = path.strip_prefix(&self.mount).ok_or(Fault::Missing)?;
        if let Some(r) = self.routes.get(relative).filter(|r| !r.path.contains('{')) {
            return Ok((r, Parameters::default()));
        }
        self.routes
            .values()
            .find_map(|r| Some((r, r.match_path(relative)?)))
            .ok_or(Fault::Missing)
    }

    fn href(&self, path: &str) -> Value {
        json!({"href":format!("{}{path}",self.mount)})
    }

    pub fn links(&self, path: &str, profile: &str) -> Value {
        json!({"self":self.href(path),"profile":self.href(&format!("/profiles/{profile}/")),
            "curies":[{"name":"rpt","href":format!("{}/rels/{{rel}}/",self.mount),"templated":true}]})
    }

    pub fn read(
        &self,
        r: &Route,
        p: Parameters<'_>,
        relative: &str,
        s: Option<&Snapshot>,
        live: &super::Observed,
        page: Option<(usize, usize)>,
    ) -> Result<Value, Fault> {
        let mut links = self.links(relative, &r.profile);
        let mut body = json!({});
        match r.target {
            Target::OpenApi => return Ok(self.openapi.clone()),
            Target::Schema | Target::Profile => {
                let name = p.name.ok_or(Fault::Missing)?;
                if r.target == Target::Schema && name == "json-patch" {
                    return Ok(super::schema::patch());
                }
                if r.target == Target::Schema && name.ends_with("-response") {
                    let subject = self
                        .routes
                        .values()
                        .find(|v| super::schema::response_name(v) == name)
                        .ok_or(Fault::Missing)?;
                    return Ok(super::schema::response_document(subject));
                }
                let (profile, create) = if r.target == Target::Schema
                    && let Some(base) = name.strip_suffix("-create")
                {
                    (base, true)
                } else {
                    (name, false)
                };
                let subject = self
                    .routes
                    .values()
                    .find(|v| v.profile == profile && (!create || v.methods.contains(&"POST")))
                    .ok_or(Fault::Missing)?;
                if r.target == Target::Schema {
                    return Ok(super::schema::document(subject, create));
                }
                links["describedby"] = self.href(&format!("/schemas/{name}/"));
                body["profile"] = json!(name);
            }
            Target::Relation => {
                let name = p.name.ok_or(Fault::Missing)?;
                if !self.routes.values().any(|r| r.relation() == name)
                    && ![
                        "start-macro",
                        "end-macro",
                        "warning-macro",
                        "site",
                        "ports",
                        "status",
                        "operations",
                        "configuration",
                        "effective",
                        "changes",
                        "health",
                        "sections",
                        "protocols",
                        "collections",
                        "credentials",
                        "schema",
                        "source",
                        "desired",
                        "request",
                    ]
                    .contains(&name)
                {
                    return Err(Fault::Missing);
                }
                body["relation"] = json!(name);
            }
            Target::Navigation if r.view.is_none() => {
                for (rel, path) in [
                    ("rpt:site", "/configuration/site/"),
                    ("rpt:ports", "/configuration/ports/"),
                    ("rpt:configuration", "/configuration/"),
                    ("rpt:effective", "/effective/"),
                    ("rpt:status", "/status/"),
                    ("rpt:operations", "/operations/"),
                    ("rpt:changes", "/changes/"),
                    ("rpt:health", "/health/"),
                    ("describedby", "/openapi.json"),
                    ("help", "/docs"),
                ] {
                    links[rel] = self.href(path);
                }
            }
            Target::Navigation if r.path == "/status/" && s.is_none() => {
                body["status"] = state(s, live);
                links["up"] = self.href("/");
                for name in ["desired", "active", "last-known-good", "storage", "owners"] {
                    links[format!("rpt:{name}")] = self.href(&format!("/status/{name}/"));
                }
            }
            Target::Docs => {
                body["title"] = json!("Repeater controller management");
                links["describedby"] = self.href("/openapi.json");
            }
            Target::Active => {
                body["active_revision"] = json!(live.active_revision);
                links["up"] = self.href("/status/");
            }
            Target::LastKnownGood => {
                body["last_known_good"] = json!(s.and_then(|s| s.last_good.as_ref()));
                links["up"] = self.href("/status/");
            }
            Target::Owners => {
                body["owners"] = state(s, live)["owners"].clone();
                links["up"] = self.href("/status/");
            }
            Target::Storage => {
                body["secrets_state"] = json!(s.map(|s| s.secrets_state));
                body["storage"] = json!(s.map_or("storage_fault", |s| s.storage));
                body["working_fingerprint"] = json!(s.and_then(|s| s.working.as_ref()));
                links["up"] = self.href("/status/");
            }
            Target::Desired => {
                let snapshot = s.ok_or(Fault::Unavailable)?;
                body["desired_revision"] = json!(snapshot.revision);
                links["up"] = self.href("/status/");
            }
            Target::Health | Target::Changes => {
                body["status"] = state(s, live);
                links["up"] = self.href("/");
            }
            Target::Operations => {
                links["up"] = self.href("/");
                links["item"] = json!([]);
            }
            Target::Operation => return Err(Fault::Missing),
            _ => {
                let snapshot = s.ok_or(Fault::Unavailable)?;
                if !p.present(r, &snapshot.registry) {
                    return Err(Fault::Missing);
                }
                let scope = p
                    .port
                    .map_or(Scope::Site(snapshot.registry.site_id()), Scope::Port);
                let binding = binding_id(snapshot, r, p);
                body["meta"] = json!({"configuration_revision":snapshot.revision});
                if let Some(id) = binding {
                    body["id"] = json!(id.to_string());
                    if let Some(version) = snapshot.versions.get(&id) {
                        body["meta"]["resource_version"] = json!(version);
                    }
                    if matches!(r.target, Target::Metadata | Target::Member(_)) {
                        if let Some(key) = snapshot
                            .registry
                            .port_keys()
                            .find_map(|(key, p)| (id == p).then_some(key))
                            .or_else(|| {
                                snapshot
                                    .registry
                                    .members()
                                    .iter()
                                    .find_map(|(b, k)| (b.id == id).then_some(k))
                            })
                        {
                            body["key"] = json!(key.as_str());
                        }
                    }
                    if r.view == Some(View::Effective) {
                        let inherited = snapshot
                            .registry
                            .by_id(id)
                            .is_some_and(|b| b.scope != scope);
                        let local = relative.replacen("/effective/", "/configuration/", 1);
                        let source = if inherited {
                            let tail = local
                                .strip_prefix("/configuration/ports/")
                                .and_then(|v| v.split_once('/'))
                                .map(|(_, v)| v)
                                .ok_or(Fault::Invalid)?;
                            format!("/configuration/site/{tail}")
                        } else {
                            local
                        };
                        links["rpt:source"] = self.href(&source);
                    }
                }
                links["rpt:changes"] = self.href("/changes/");
                if r.view == Some(View::Configuration) {
                    links["rpt:effective"] =
                        self.href(&relative.replacen("/configuration/", "/effective/", 1));
                    links["rpt:status"] =
                        self.href(&relative.replacen("/configuration/", "/status/", 1));
                    if matches!(r.target, Target::Credentials(_)) {
                        links
                            .as_object_mut()
                            .expect("links")
                            .remove("rpt:effective");
                        links.as_object_mut().expect("links").remove("rpt:status");
                    }
                }
                if let Target::Collection(kind) = r.target {
                    let ids = routes::members(&snapshot.registry, scope, kind, r.view);
                    let items: Vec<_> = ids
                        .iter()
                        .map(|id| self.href(&format!("{relative}{id}/")))
                        .collect();
                    links["item"] = json!(items);
                    body[if r.view == Some(View::Effective) {
                        "effective"
                    } else {
                        "configuration"
                    }] = json!({"members":ids.iter().map(ToString::to_string).collect::<Vec<_>>()});
                    if let Some((offset, count)) = page {
                        body["_embedded"] = json!({"item":ids.iter().skip(offset).take(count).map(|id|json!({"id":id.to_string(),"_links":{"self":self.href(&format!("{relative}{id}/"))}})).collect::<Vec<_>>()});
                        links["canonical"] = self.href(relative);
                        links["self"] =
                            self.href(&format!("{relative}?offset={offset}&limit={count}"));
                        if offset.saturating_add(count) < ids.len() {
                            links["next"] = self.href(&format!(
                                "{relative}?offset={}&limit={count}",
                                offset + count
                            ));
                        }
                        if offset > 0 {
                            links["prev"] = self.href(&format!(
                                "{relative}?offset={}&limit={count}",
                                offset.saturating_sub(count)
                            ));
                        }
                    }
                } else if r.target == Target::Navigation {
                    if r.path == "/status/" {
                        body["status"] = state(s, live);
                        for name in ["desired", "active", "last-known-good", "storage", "owners"] {
                            links[format!("rpt:{name}")] = self.href(&format!("/status/{name}/"));
                        }
                    }
                } else if matches!(r.target, Target::Credentials(_)) {
                    let id = binding.ok_or(Fault::Missing)?;
                    let values = snapshot.configured.get(&id).ok_or(Fault::Missing)?;
                    body["meta"]["credentials"] = json!(values);
                    body["meta"]["secrets_state"] = json!(snapshot.secrets_state);
                } else if r.view == Some(View::Status) {
                    body["status"] = state(s, live);
                } else {
                    let logical = matches!(r.target, Target::Section(_));
                    let kind = match r.target {
                        Target::Section(k) | Target::Member(k) => k,
                        _ => Kind::Metadata,
                    };
                    let data = match (r.view, binding) {
                        (Some(View::Effective), id) => {
                            match id
                                .and_then(|id| snapshot.projections.effective.get(&(scope, id)))
                                .or_else(|| {
                                    snapshot.projections.fixed_effective.get(&(scope, kind))
                                }) {
                                Some(fields) => fields.clone(),
                                None if logical && kind.has_optional_presence() => json!({}),
                                None => return Err(Fault::Unavailable),
                            }
                        }
                        (Some(View::Configuration), Some(id)) => {
                            match snapshot.projections.local.get(&id) {
                                Some(fields) => Value::Object(fields.clone()),
                                None if logical && !snapshot.versions.contains_key(&id) => {
                                    json!({})
                                }
                                _ => return Err(Fault::Unavailable),
                            }
                        }
                        (_, None) if logical => json!({}),
                        _ => return Err(Fault::Missing),
                    };
                    if kind == Kind::Event {
                        body.as_object_mut()
                            .expect("body")
                            .extend(data.as_object().ok_or(Fault::Invalid)?.clone());
                    } else {
                        body[if r.view == Some(View::Effective) {
                            "effective"
                        } else {
                            "configuration"
                        }] = data;
                    }
                    if let Some(id) = binding
                        && let Some(flags) = snapshot.configured.get(&id)
                    {
                        body["meta"]["credentials"] = json!(flags);
                        body["meta"]["secrets_state"] = json!(snapshot.secrets_state);
                    }
                }
                if let Some(id) = binding
                    && let Some(event) = snapshot.projections.event_links.get(&(scope, id))
                {
                    let macro_link =
                        |b: &rpt_advanced_core::config::resources::candidate::ResourceBinding| {
                            let scope = match b.scope {
                                Scope::Site(_) => "site".to_owned(),
                                Scope::Port(id) => format!("ports/{id}"),
                            };
                            self.href(&format!("/configuration/{scope}/command/macros/{}/", b.id))
                        };
                    links["rpt:start-macro"] = macro_link(&event.start);
                    if let Some(end) = &event.end {
                        links["rpt:end-macro"] = macro_link(end);
                    }
                    links["rpt:warning-macro"] =
                        Value::Array(event.warnings.iter().map(macro_link).collect());
                }
                self.children(r, p, relative, snapshot, &mut links);
            }
        }
        body["_links"] = links;
        Ok(body)
    }

    pub fn parent_link(&self, r: &Route, p: Parameters<'_>) -> Value {
        let mut template = r.path.trim_end_matches('/');
        let mut parent = "/";
        while let Some((up, _)) = template.rsplit_once('/') {
            let test = format!("{up}/");
            if self.routes.contains_key(&test) {
                parent = &r.path[..test.len()];
                break;
            }
            template = up;
        }
        self.href(&expand(parent, p))
    }

    fn children(
        &self,
        r: &Route,
        p: Parameters<'_>,
        relative: &str,
        s: &Snapshot,
        links: &mut Value,
    ) {
        if relative != "/" {
            links["up"] = self.parent_link(r, p);
        }
        let mut children = Vec::new();
        for child in self
            .routes
            .values()
            .filter(|v| v.view == r.view && v.path != r.path && v.path.starts_with(&r.path))
        {
            let tail = &child.path[r.path.len()..];
            if tail.contains('{') || !p.present(child, &s.registry) {
                continue;
            }
            let mut parts = tail.trim_end_matches('/').split('/').collect::<Vec<_>>();
            parts.pop();
            let mut intermediate = r.path.clone();
            let indirect = parts.iter().any(|part| {
                intermediate.push_str(part);
                intermediate.push('/');
                self.routes.contains_key(&intermediate)
            });
            if indirect
                && !(r.target == Target::Metadata && matches!(child.target, Target::Collection(_)))
            {
                continue;
            }
            if matches!(child.target, Target::Credentials(_))
                && !binding_id(s, child, p).is_some_and(|id| s.configured.contains_key(&id))
            {
                continue;
            }
            let href = self.href(&expand(&child.path, p));
            links[format!("rpt:{}", child.relation())] = href.clone();
            children.push(href);
        }
        if !children.is_empty() {
            links["rpt:sections"] = Value::Array(children);
        }
    }
}

fn expand(path: &str, p: Parameters<'_>) -> String {
    let mut href = path.to_owned();
    for (name, id) in [("{port}", p.port), ("{member}", p.member)] {
        if let Some(id) = id {
            href = href.replace(name, &id.to_string());
        }
    }
    href
}

fn state(s: Option<&Snapshot>, o: &super::Observed) -> Value {
    json!({"storage":s.map_or("storage_fault",|s|s.storage),"secrets_state":s.map(|s|s.secrets_state),"desired_revision":s.map(|s|&s.revision),
        "last_known_good":s.and_then(|s|s.last_good.as_ref()),"active_revision":o.active_revision,"owners":o.owners.iter().map(|(id,g)|(id.to_string(),json!(g))).collect::<serde_json::Map<_,_>>(),
        "working_fingerprint":s.and_then(|s|s.working.as_ref()),"historical_activation":s.map(|s|s.activation)})
}
