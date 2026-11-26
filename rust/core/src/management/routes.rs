//! The HTTP resource inventory, shared by routing, HAL discovery and method descriptions.
use crate::config::resources::{
    candidate::{ResourceId, ResourceKind as Kind, ResourceRegistry, ResourceScope},
    catalog::Layout,
};
use std::collections::{BTreeMap, BTreeSet};

/// Public read view selected by the resource path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    /// Sparse editable resources.
    Configuration,
    /// Resolved public domain values.
    Effective,
    /// Runtime observations from the actual owner.
    Status,
}

impl View {
    fn segment(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Effective => "effective",
            Self::Status => "status",
        }
    }
}

/// Resource identity independent of the storage filename.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// Link-only category.
    Navigation,
    /// Site or port metadata.
    Metadata,
    /// Existing fixed resource or logical section.
    Section(Kind),
    /// Ordered collection; Metadata denotes the port list.
    Collection(Kind),
    /// One existing collection member.
    Member(Kind),
    /// Existing owner's write-only credential instructions.
    Credentials(Kind),
    /// Request identified by service boot and client request UUIDs.
    Operation,
    /// Paged live protocol peers on the port.
    Peers,
    /// Paged private call lifecycles on the port.
    Calls,
    /// Port maintenance lease inventory for this boot.
    Maintenance,
    /// Conditional, connection-bound tuning lease.
    MaintenanceLease,
    /// Read-only operation inventory.
    Operations,
    /// Read-only change inventory.
    Changes,
    /// Durable desired selector used for explicit recovery preconditions.
    Desired,
    /// Last revision confirmed by the running process.
    Active,
    /// Durable last-known-good revision.
    LastKnownGood,
    /// Current storage/working state.
    Storage,
    /// Observed active owner generations.
    Owners,
    /// Human-readable interface documentation.
    Docs,
    /// Controller management health.
    Health,
    /// Code-derived OpenAPI description.
    OpenApi,
    /// Description of one HAL profile.
    Profile,
    /// JSON schema for one documented authoring profile.
    Schema,
    /// Definition of a HAL extension relation.
    Relation,
}

const READ: &[&str] = &["GET"];
const EDIT: &[&str] = &["GET", "PUT", "PATCH"];
const MEMBER: &[&str] = &["GET", "PUT", "PATCH", "DELETE"];
const COLLECTION: &[&str] = &["GET", "PUT", "PATCH", "POST", "DELETE"];

/// One route template used by requests and interface discovery.
#[derive(Debug)]
pub struct Route {
    /// Path relative to the authenticated API mount.
    pub path: String,
    /// Public projection of an owned resource.
    pub view: Option<View>,
    /// Kind and cardinality of this path.
    pub target: Target,
    /// Stable public profile name.
    pub profile: String,
    /// Supported HTTP methods, ordered for Allow.
    pub methods: &'static [&'static str],
}

impl Route {
    fn new(path: &str, view: Option<View>, target: Target, profile: &str) -> Self {
        let methods = match (view, target) {
            (Some(View::Effective | View::Status), _) => READ,
            (_, Target::Collection(_)) => COLLECTION,
            (_, Target::Member(_) | Target::MaintenanceLease) => MEMBER,
            (_, Target::Metadata) if path.contains("{port}") => MEMBER,
            (
                _,
                Target::Metadata | Target::Section(_) | Target::Credentials(_) | Target::Operation,
            ) => EDIT,
            _ => READ,
        };
        Self {
            path: path.to_owned(),
            view,
            target,
            profile: profile.to_owned(),
            methods,
        }
    }

    /// HAL relation name for a concrete child, independent of its physical URL nesting.
    pub fn relation(&self) -> &str {
        match self.target {
            Target::Collection(Kind::Metadata) => "ports",
            Target::Collection(kind) => match kind.profile().1 {
                Layout::Collection(path) => path.rsplit('/').next().expect("catalog collection"),
                _ => kind.as_str(),
            },
            Target::Section(Kind::EcholinkProxy) => "proxy",
            Target::Section(kind) | Target::Member(kind) => kind.as_str(),
            Target::Credentials(_) => "credentials",
            _ => &self.profile,
        }
    }

    /// Resolve a relative path strictly. UUIDs use their lowercase hyphenated encoding.
    pub fn match_path<'a>(&self, path: &'a str) -> Option<Parameters<'a>> {
        let mut actual = path.split('/');
        let mut params = Parameters::default();
        for part in self.path.split('/') {
            let segment = actual.next()?;
            match part {
                "{port}" => params.port = Some(canonical_id(segment)?),
                "{member}" => params.member = Some(canonical_id(segment)?),
                "{boot}" => params.boot = Some(canonical_id(segment)?),
                "{request}" | "{lease}" => params.request = Some(canonical_id(segment)?),
                "{name}"
                    if !segment.is_empty()
                        && segment
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') =>
                {
                    params.name = Some(segment)
                }
                _ if segment != part || part.starts_with('{') => return None,
                _ => {}
            }
        }
        actual.next().is_none().then_some(params)
    }
}

fn canonical_id(input: &str) -> Option<ResourceId> {
    let id = input.parse::<ResourceId>().ok()?;
    (id.to_string() == input).then_some(id)
}

/// Parsed identities from a route, independent of any human-chosen key.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Parameters<'a> {
    /// Port scope when present.
    pub port: Option<ResourceId>,
    /// Named or optional account member.
    pub member: Option<ResourceId>,
    /// Operation's boot identity.
    pub boot: Option<ResourceId>,
    /// Operation's client-generated request identity.
    pub request: Option<ResourceId>,
    /// Profile, schema or relation name.
    pub name: Option<&'a str>,
}

impl Parameters<'_> {
    /// Verify the complete owner chain before resolving or exposing state.
    pub fn present(self, route: &Route, registry: &ResourceRegistry) -> bool {
        let scope = match self.port {
            Some(id) => {
                let scope = ResourceScope::Port(id);
                if registry.binding(scope, Kind::Metadata).is_none() {
                    return false;
                }
                scope
            }
            None => ResourceScope::Site(registry.site_id()),
        };
        let Some(member) = self.member else {
            return !matches!(
                route.target,
                Target::Section(Kind::EcholinkProxy) | Target::Credentials(Kind::EcholinkProxy)
            ) || registry.binding(scope, Kind::Echolink).is_some();
        };
        let kind = match route.target {
            Target::Member(kind) => kind,
            Target::Section(Kind::EcholinkProxy) | Target::Credentials(Kind::EcholinkProxy) => {
                Kind::Echolink
            }
            Target::Credentials(kind) => kind,
            _ => return false,
        };
        if inherits_members(route.view, kind) && kind.is_named() {
            members(registry, scope, kind, route.view).contains(&member)
        } else {
            registry
                .by_id(member)
                .is_some_and(|binding| binding.scope == scope && binding.kind == kind)
        }
    }
}

fn inherits_members(view: Option<View>, kind: Kind) -> bool {
    view == Some(View::Effective)
        || (view == Some(View::Status)
            && matches!(
                kind,
                Kind::Event | Kind::Sensor | Kind::Input | Kind::Alarm | Kind::Output | Kind::Fan
            ))
}

/// Return a complete ordered inventory. A port's effective named collection exposes
/// each inherited declaration until the same local key shadows it.
pub fn members(
    registry: &ResourceRegistry,
    scope: ResourceScope,
    kind: Kind,
    view: Option<View>,
) -> Vec<ResourceId> {
    if kind == Kind::Metadata {
        return registry.port_keys().map(|(_, id)| id).collect();
    }
    if !kind.is_named() {
        return registry
            .binding(scope, kind)
            .map(|b| b.id)
            .into_iter()
            .collect();
    }
    let local = registry
        .members()
        .iter()
        .filter(|(b, _)| b.kind == kind && b.scope == scope);
    if !inherits_members(view, kind) || matches!(scope, ResourceScope::Site(_)) {
        return local.map(|(b, _)| b.id).collect();
    }
    let replacements: BTreeMap<_, _> = local.clone().map(|(b, k)| (k, b.id)).collect();
    let mut inherited = BTreeSet::new();
    let mut ids = Vec::new();
    for (binding, key) in registry
        .members()
        .iter()
        .filter(|(b, _)| b.kind == kind && matches!(b.scope, ResourceScope::Site(_)))
    {
        inherited.insert(key);
        ids.push(replacements.get(key).copied().unwrap_or(binding.id));
    }
    ids.extend(
        local
            .filter(|(_, k)| !inherited.contains(k))
            .map(|(b, _)| b.id),
    );
    ids
}

/// Construct the documented method and route matrix from the working resource catalog.
pub fn inventory() -> BTreeMap<String, Route> {
    let mut routes = BTreeMap::new();
    let mut add = |path: &str, view, target, profile: &str| {
        routes.insert(path.to_owned(), Route::new(path, view, target, profile));
    };
    for (path, target, profile) in [
        ("/", Target::Navigation, "api"),
        ("/operations/", Target::Operations, "operations"),
        (
            "/maintenance/ports/{port}/",
            Target::Maintenance,
            "maintenance",
        ),
        (
            "/maintenance/ports/{port}/{boot}/{lease}/",
            Target::MaintenanceLease,
            "maintenance-lease",
        ),
        (
            "/operations/{boot}/{request}/",
            Target::Operation,
            "operation",
        ),
        ("/changes/", Target::Changes, "changes"),
        ("/health/", Target::Health, "health"),
        ("/status/desired/", Target::Desired, "desired"),
        ("/status/active/", Target::Active, "active"),
        (
            "/status/last-known-good/",
            Target::LastKnownGood,
            "last-known-good",
        ),
        ("/status/storage/", Target::Storage, "storage"),
        ("/status/owners/", Target::Owners, "owners"),
        ("/docs", Target::Docs, "docs"),
        ("/openapi.json", Target::OpenApi, "openapi"),
        ("/profiles/{name}/", Target::Profile, "profile"),
        ("/schemas/{name}/", Target::Schema, "schema"),
        ("/rels/{name}/", Target::Relation, "relation"),
    ] {
        add(path, None, target, profile);
    }
    for view in [View::Configuration, View::Effective, View::Status] {
        let root = format!("/{}/", view.segment());
        add(&root, Some(view), Target::Navigation, view.segment());
        add(
            &format!("{root}ports/"),
            Some(view),
            Target::Collection(Kind::Metadata),
            "port-collection",
        );
        for (owner, site) in [("site/", true), ("ports/{port}/", false)] {
            let base = format!("{root}{owner}");
            if !site && view == View::Status {
                add(&format!("{base}peers/"), Some(view), Target::Peers, "peers");
                add(&format!("{base}calls/"), Some(view), Target::Calls, "calls");
            }
            let protocol = if site {
                "protocol-defaults"
            } else {
                "protocols"
            };
            add(
                &base,
                Some(view),
                Target::Metadata,
                if site { "site" } else { "port" },
            );
            add(
                &format!("{base}{protocol}/"),
                Some(view),
                Target::Navigation,
                "protocols",
            );
            for kind in
                Kind::catalog().filter(|k| !matches!(k, Kind::Metadata | Kind::EcholinkProxy))
            {
                let name = kind.as_str();
                let (path, collection) = match kind.profile().1 {
                    Layout::File(file) => {
                        (format!("{base}{}/", file.trim_end_matches(".json")), false)
                    }
                    Layout::Collection(dir) => (format!("{base}{dir}/"), true),
                    Layout::Protocol => (format!("{base}{protocol}/{name}/"), !site),
                };
                let leaf = if collection {
                    add(
                        &path,
                        Some(view),
                        Target::Collection(kind),
                        &format!("{name}-collection"),
                    );
                    let member = format!("{path}{{member}}/");
                    add(&member, Some(view), Target::Member(kind), name);
                    member
                } else {
                    add(&path, Some(view), Target::Section(kind), name);
                    path
                };
                if kind == Kind::Echolink {
                    let proxy = format!("{leaf}proxy/");
                    add(
                        &proxy,
                        Some(view),
                        Target::Section(Kind::EcholinkProxy),
                        "echolink-proxy",
                    );
                    if view == View::Configuration {
                        add(
                            &format!("{proxy}credentials/"),
                            Some(view),
                            Target::Credentials(Kind::EcholinkProxy),
                            "proxy-credentials",
                        );
                    }
                }
                if view == View::Configuration
                    && matches!(
                        kind,
                        Kind::Allstarlink
                            | Kind::Echolink
                            | Kind::PhonePatch
                            | Kind::Command
                            | Kind::Mailbox
                    )
                {
                    add(
                        &format!("{leaf}credentials/"),
                        Some(view),
                        Target::Credentials(kind),
                        &format!("{name}-credentials"),
                    );
                }
            }
        }
    }
    routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resources::{PortKey, candidate::ResourceBinding};
    fn id(n: u8) -> ResourceId {
        format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap()
    }

    #[test]
    fn canonical_routes_enforce_the_complete_registered_owner_chain() {
        let site = id(1);
        let port = id(2);
        let other = id(3);
        let echo = id(10);
        let phone = id(11);
        let child = id(12);
        let registry = ResourceRegistry::new(
            site,
            &[
                (port, PortKey::new("../private/owner")),
                (other, PortKey::new("Other")),
            ],
            &[
                ResourceBinding {
                    id: echo,
                    scope: ResourceScope::Port(port),
                    kind: Kind::Echolink,
                },
                ResourceBinding {
                    id: phone,
                    scope: ResourceScope::Port(port),
                    kind: Kind::PhonePatch,
                },
                ResourceBinding {
                    id: child,
                    scope: ResourceScope::Port(port),
                    kind: Kind::EcholinkProxy,
                },
            ],
        )
        .unwrap();
        let routes = inventory();
        for r in routes.values() {
            assert_eq!(r.methods.first(), Some(&"GET"));
            if r.methods.contains(&"POST") {
                assert!(r.methods.contains(&"DELETE"));
            }
            assert_eq!(r.methods.contains(&"PATCH"), r.methods.contains(&"PUT"));
            if matches!(r.view, Some(View::Effective | View::Status)) {
                assert_eq!(r.methods, READ);
            }
        }
        let route = &routes["/configuration/ports/{port}/protocols/echolink/{member}/proxy/"];
        assert_eq!(route.methods, EDIT);
        for (owner, member, exists) in [
            (port, echo, true),
            (port, phone, false),
            (other, echo, false),
            (port, child, false),
        ] {
            let path = format!("/configuration/ports/{owner}/protocols/echolink/{member}/proxy/");
            assert_eq!(
                route.match_path(&path).unwrap().present(route, &registry),
                exists
            );
        }
        let noncanonical = format!(
            "/configuration/ports/{port}/protocols/echolink/{}/proxy/",
            echo.to_string().to_uppercase()
        );
        assert!(route.match_path(&noncanonical).is_none());
        assert!(route.match_path(&route.path).is_none());
        for k in ["allstarlink", "echolink", "phone-patch"] {
            let path = format!("/configuration/ports/{{port}}/protocols/{k}/");
            assert!(routes[&path].methods.contains(&"POST"));
            let p = Parameters {
                port: Some(other),
                ..Default::default()
            };
            assert!(p.present(&routes[&path], &registry));
        }
        assert!(
            !routes
                .keys()
                .any(|k| k.contains("protocols/echolink-proxy"))
        );
    }
}
