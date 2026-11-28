//! HAL documents derived from the same inventory as dispatch and description.
use super::{Fault, Snapshot};
use rpt_advanced_core::{
    config::resources::candidate::{
        ResourceBinding as Binding, ResourceId as Id, ResourceKind as Kind, ResourceScope as Scope,
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
        Self::with_webrtc(mount, false)
    }

    /// Register signaling routes only when a live provider handles them.
    pub fn with_webrtc(mount: &str, available: bool) -> Result<Self, Fault> {
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
        let routes = routes::inventory_with_webrtc(available);
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

    pub(super) fn href(&self, path: &str) -> Value {
        json!({"href":format!("{}{path}",self.mount)})
    }

    fn authoring_href(&self, b: Binding, s: &Snapshot) -> Result<String, Fault> {
        let port = match b.scope {
            Scope::Site(_) => None,
            Scope::Port(id) => Some(id),
        };
        let target = if b.kind == Kind::Metadata {
            Target::Metadata
        } else if b.kind.is_named()
            || (port.is_some()
                && matches!(
                    b.kind,
                    Kind::Allstarlink | Kind::Echolink | Kind::PhonePatch
                ))
        {
            Target::Member(b.kind)
        } else {
            Target::Section(b.kind)
        };
        let member = if b.kind == Kind::EcholinkProxy && port.is_some() {
            Some(
                s.registry
                    .binding(b.scope, Kind::Echolink)
                    .ok_or(Fault::Invalid)?
                    .id,
            )
        } else if matches!(target, Target::Member(_)) {
            Some(b.id)
        } else {
            None
        };
        let route = self
            .routes
            .values()
            .find(|r| {
                r.view == Some(View::Configuration)
                    && r.target == target
                    && r.path.contains("{port}") == port.is_some()
            })
            .ok_or(Fault::Invalid)?;
        Ok(format!(
            "{}{}",
            self.mount,
            expand(
                &route.path,
                Parameters {
                    port,
                    member,
                    ..Default::default()
                }
            )
        ))
    }
    pub fn links(&self, path: &str, profile: &str) -> Value {
        json!({"self":self.href(path),"profile":self.href(&format!("/profiles/{profile}/")),
            "curies":[{"name":"rpt","href":format!("{}/rels/{{rel}}/",self.mount),"templated":true}]})
    }

    fn link_status_views(&self, links: &mut Value, query: &str) {
        use super::status::{SNAPSHOT, STREAM, scoped_view};
        links["rpt:status-snapshot"] = self.href(&scoped_view(SNAPSHOT, query));
        links["rpt:status-stream"] = self.href(&scoped_view(STREAM, query));
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
                if subject.target.is_webrtc() {
                    let response = super::schema::response_name(subject);
                    let request = super::schema::name(subject, subject.methods.contains(&"POST"));
                    links["describedby"] = self.href(&format!("/schemas/{response}/"));
                    links["rpt:request"] = self.href(&format!("/schemas/{request}/"));
                }
                if matches!(
                    subject.target,
                    Target::StatusSnapshot | Target::StatusStream
                ) {
                    body["status_selection"] =
                        super::status::profile(subject.target == Target::StatusStream);
                    links["describedby"] = self.href("/schemas/status-snapshot-response/");
                    self.link_status_views(&mut links, "");
                }
            }
            Target::Relation => {
                let name = p.name.ok_or(Fault::Missing)?;
                let registered = self.routes.values().any(|r| {
                    r.relation() == name || (name == "close" && r.target == Target::WebRtcSession)
                });
                if !registered
                    && ![
                        "start-macro",
                        "status-all-ports",
                        "lease",
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
                if r.target == Target::Metadata
                    && let Some(port) = p.port
                {
                    links["rpt:maintenance"] = self.href(&format!("/maintenance/ports/{port}/"));
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
                if matches!(r.target, Target::Peers | Target::Calls) {
                    let port = p.port.ok_or(Fault::Missing)?;
                    let (offset, limit) = page.unwrap_or((0, 20));
                    let (items, more) = live.domain.page(port, r.target, offset, limit);
                    body["_embedded"] = json!({"item": items});
                    links["canonical"] = self.href(relative);
                    if page.is_some() {
                        links["self"] =
                            self.href(&format!("{relative}?offset={offset}&limit={limit}"));
                    }
                    if more {
                        links["next"] = self.href(&format!(
                            "{relative}?offset={}&limit={limit}",
                            offset.saturating_add(limit)
                        ));
                    }
                    if offset > 0 {
                        links["prev"] = self.href(&format!(
                            "{relative}?offset={}&limit={limit}",
                            offset.saturating_sub(limit)
                        ));
                    }
                } else if let Target::Collection(kind) = r.target {
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
                    let mut status = live.domain.fields(scope, binding, r.target);
                    status["active_revision"] = json!(live.active_revision);
                    status["owner_generation"] = json!(p.port.and_then(|id| live.owners.get(&id)));
                    body["status"] = status;
                } else {
                    let projections = &snapshot
                        .views
                        .result
                        .as_ref()
                        .map_err(|_| Fault::Unavailable)?
                        .public;
                    let logical = matches!(r.target, Target::Section(_));
                    let kind = match r.target {
                        Target::Section(k) | Target::Member(k) => k,
                        _ => Kind::Metadata,
                    };
                    let data = match (r.view, binding) {
                        (Some(View::Effective), id) => {
                            match id
                                .and_then(|id| projections.effective.get(&(scope, id)))
                                .or_else(|| projections.fixed_effective.get(&(scope, kind)))
                            {
                                Some(fields) => fields.clone(),
                                None if logical && kind.has_optional_presence() => json!({}),
                                None => return Err(Fault::Unavailable),
                            }
                        }
                        (Some(View::Configuration), Some(id)) => match projections.local.get(&id) {
                            Some(fields) => Value::Object(fields.clone()),
                            None if logical && !snapshot.versions.contains_key(&id) => {
                                json!({})
                            }
                            _ => return Err(Fault::Unavailable),
                        },
                        (_, None) if logical => json!({}),
                        _ => return Err(Fault::Missing),
                    };
                    let member = kind.is_named().then_some(binding).flatten();
                    if let Some(references) = projections.message_links.get(&(scope, kind, member))
                    {
                        links["rpt:messages"] = Value::Array(
                            references
                                .iter()
                                .map(|b| {
                                    self.authoring_href(*b, snapshot)
                                        .map(|href| json!({"href":href}))
                                })
                                .collect::<Result<_, _>>()?,
                        );
                    }
                    if r.view == Some(View::Effective) {
                        let key = (scope, kind, if kind.is_named() { binding } else { None });
                        let mut origins = serde_json::Map::new();
                        let mut hrefs = BTreeMap::new();
                        if let Some(sources) = projections.sources.get(&key) {
                            for (path, src) in sources {
                                let mut href = |b: Binding| -> Result<String, Fault> {
                                    if let std::collections::btree_map::Entry::Vacant(e) =
                                        hrefs.entry(b.id)
                                    {
                                        e.insert(self.authoring_href(b, snapshot)?);
                                    }
                                    Ok(hrefs[&b.id].clone())
                                };
                                use super::projections::Origin;
                                let source = match src {
                                    Origin::Resource(b) => {
                                        json!({"kind":"resource","href":href(*b)?})
                                    }
                                    Origin::Derived(bindings) => {
                                        json!({"kind":"derived","sources":bindings.iter().map(|b|href(*b).map(|v|json!({"href":v}))).collect::<Result<Vec<_>,_>>()?})
                                    }
                                    Origin::BuiltIn => json!({"kind":"built-in"}),
                                };
                                origins.insert(path.clone(), source);
                            }
                        }
                        body["meta"]["sources"] = Value::Object(origins);
                        if matches!(scope, Scope::Site(_))
                            && let Some(fields) = projections.site_diagnostics.get(&kind)
                        {
                            body["meta"]["deferred_relationships"] = json!(fields);
                        }
                    }
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
                    && let Ok(projections) = &snapshot.views.result
                    && let Some(event) = projections.public.event_links.get(&(scope, id))
                {
                    if r.view == Some(View::Effective) && !event.timezones.is_empty() {
                        body["meta"]["timezones"] = json!(event.timezones);
                    }
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
        if r.view == Some(View::Status) {
            if r.path == "/status/" {
                use super::status::{SNAPSHOT, scoped_view};
                self.link_status_views(&mut links, "");
                links["rpt:status-all-ports"] =
                    self.href(&scoped_view(SNAPSHOT, "site=true&all_ports=true"));
            } else if r.target == Target::Metadata {
                let query = p
                    .port
                    .map_or_else(|| "site=true".to_owned(), |port| format!("port={port}"));
                self.link_status_views(&mut links, &query);
                if let Some(port) = p.port
                    && live.status_ready
                    && live.owners.contains_key(&port)
                    && let Some(collection) = self
                        .routes
                        .values()
                        .find(|route| route.target == Target::WebRtcSessions)
                {
                    links["rpt:webrtc-sessions"] = self.href(&expand(&collection.path, p));
                }
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
