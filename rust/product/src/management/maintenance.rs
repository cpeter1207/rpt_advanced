//! Boot-local tuning grants. Only the physical host can change a radio.
use super::*;
use rpt_advanced_core::management::patch::{self, PatchPolicy};
use rptadv_peer_support::https::ConnectionProof;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

const LIMIT: usize = 1024;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    active: bool,
    expected_active_revision: String,
    #[serde(default = "default_ttl")]
    ttl_ms: u64,
}
fn default_ttl() -> u64 {
    30_000
}

/// One boot-scoped capability. No physical or generation work is retained by this grant.
pub struct Grant {
    pub(crate) port: Id,
    pub(crate) generation: u64,
    pub(crate) revision: String,
    pub(crate) mount: String,
    actor: String,
    connection: ConnectionProof,
    deadline: Mutex<Option<Instant>>,
    pub(crate) gate: std::sync::OnceLock<Arc<std::sync::atomic::AtomicBool>>,
}
impl Grant {
    pub(crate) fn valid(&self) -> bool {
        let mut until = self.deadline.lock().expect("maintenance deadline");
        if self
            .gate
            .get()
            .is_some_and(|gate| gate.load(std::sync::atomic::Ordering::Acquire))
            || !self.connection.is_alive()
            || until.is_none_or(|t| Instant::now() >= t)
        {
            *until = None;
        }
        until.is_some()
    }
    pub(crate) fn revoke(&self) {
        *self.deadline.lock().expect("maintenance deadline") = None;
    }
    fn owned(&self, actor: &str, connection: &ConnectionProof) -> bool {
        self.actor == actor && self.connection.same_connection(connection)
    }
    fn refresh(&self, live: &Observed) -> bool {
        if live.active_revision.as_deref() != Some(self.revision.as_str())
            || live.owners.get(&self.port) != Some(&self.generation)
        {
            self.revoke();
        }
        self.valid()
    }
}

/// Typed values returned by the radio. EEPROM has exactly 64 words.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum QueryResult {
    /// Current level of the requested mixer.
    Mixer {
        /// Native level.
        level: u16,
    },
    /// Current bounded EEPROM tuning region.
    Eeprom {
        /// The exact 64 native words.
        words: Vec<u16>,
    },
    /// The corresponding public HAL view for status/configuration queries.
    View {
        /// Service-relative public representation.
        href: String,
    },
}

impl QueryResult {
    pub(super) fn valid(&self, target: Id) -> bool {
        match self {
            Self::Mixer { level } => *level <= 999,
            Self::Eeprom { words } => words.len() == 64,
            Self::View { href } => {
                let Some(path) = href.strip_prefix('/') else {
                    return false;
                };
                let Some(path) = path.strip_suffix('/') else {
                    return false;
                };
                let mut parts = path.rsplit('/');
                parts.next().is_some_and(|id| id == target.to_string())
                    && parts.next() == Some("ports")
                    && matches!(parts.next(), Some("status" | "effective"))
                    && parts.all(|s| {
                        !s.is_empty()
                            && s.bytes()
                                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
                    })
            }
        }
    }
}
struct Entry {
    grant: Arc<Grant>,
    ttl_ms: u64,
    version: u64,
}
#[derive(Default)]
pub(super) struct Book(BTreeMap<(Id, Id), Entry>);
impl<R: Runtime> Backend<R> {
    pub(super) fn maintenance_grant(
        &self,
        href: &str,
        port: Id,
        rev: &str,
        actor: &str,
        connection: &ConnectionProof,
    ) -> Result<Arc<Grant>, Fault> {
        let (route, p) = self.api.lookup(href).map_err(|_| Fault::Forbidden)?;
        if route.target != Target::MaintenanceLease
            || p.boot != Some(self.boot)
            || p.port != Some(port)
        {
            return Err(Fault::Forbidden);
        }
        let book = self.maintenance.lock().expect("maintenance leases");
        let row = book
            .0
            .get(&(port, p.request.ok_or(Fault::Forbidden)?))
            .ok_or(Fault::Forbidden)?;
        if !row.grant.owned(actor, connection)
            || row.grant.revision != rev
            || !row.grant.refresh(&self.runtime.observed())
        {
            return Err(Fault::Forbidden);
        }
        Ok(row.grant.clone())
    }
    pub(super) fn lease_request(
        &self,
        req: &AdmittedRequest,
        route: &Route,
        p: Parameters<'_>,
    ) -> Result<Response<Vec<u8>>, Fault> {
        let port = p.port.ok_or(Fault::Missing)?;
        let path = req.target.path();
        let rel = path.strip_prefix(&self.api.mount).ok_or(Fault::Missing)?;
        let collection = format!("/maintenance/ports/{port}/");
        let mut links = self.api.links(rel, &route.profile);
        let live = self.runtime.observed();
        let mut book = self.maintenance.lock().expect("maintenance leases");
        let present = self
            .cached
            .read()
            .expect("snapshot")
            .as_ref()
            .is_some_and(|s| p.present(route, &s.registry));
        let actor = req.actor.as_str();
        let response_doc = |status, doc: Value| {
            let tag = validator(path, "", &doc);
            let mut reply = response(status, doc, Some(route), "application/hal+json");
            if req.method == Method::GET {
                reply
                    .headers_mut()
                    .insert("etag", tag.parse().expect("validator"));
            }
            Ok(reply)
        };
        if route.target == Target::Maintenance {
            if !present {
                return Err(Fault::Missing);
            }
            links["rpt:lease"] = json!({"href":format!("{}{collection}{}/{{lease}}/",self.api.mount,self.boot),"templated":true});
            links["item"]=json!(book.0.iter().filter(|((id,_),row)|*id==port && row.grant.actor==actor && row.grant.refresh(&live)).map(|((_,id),_)|json!({"href":format!("{}{collection}{}/{id}/",self.api.mount,self.boot)})).collect::<Vec<_>>());
            return response_doc(StatusCode::OK, json!({"_links":links}));
        }
        if p.boot != Some(self.boot) {
            return Err(Fault::Gone);
        }
        let key = (port, p.request.ok_or(Fault::Missing)?);
        links["up"] = json!({"href":format!("{}{collection}",self.api.mount)});
        let document = |r: &Entry| json!({"_links":links,"configuration":{"active":r.grant.refresh(&live),"ttl_ms":r.ttl_ms,"expected_active_revision":r.grant.revision},"meta":{"version":r.version,"generation":r.grant.generation}});
        let old = book.0.get(&key);
        let prior = if let Some(row) = old {
            if row.grant.actor != actor {
                return Err(Fault::Forbidden);
            }
            let doc = document(row);
            if req.method == Method::GET {
                return response_doc(StatusCode::OK, doc);
            }
            if !row.grant.owned(actor, &req.connection) {
                return Err(Fault::Forbidden);
            }
            if req.headers.contains_key("if-none-match") {
                return Err(Fault::PreconditionFailed);
            }
            require(&req.headers, &validator(path, "", &doc))?;
            if req.method == Method::DELETE {
                row.grant.revoke();
                self.runtime.clear_maintenance(row.grant.clone())?;
                return response_doc(StatusCode::OK, document(row));
            }
            if !row.grant.valid() {
                return Err(Fault::Conflict);
            }
            doc
        } else {
            if !present || req.method != Method::PUT {
                return Err(Fault::Missing);
            }
            if req.headers.get("if-none-match").is_none_or(|v| v != "*") {
                return Err(if req.headers.contains_key("if-none-match") {
                    Fault::PreconditionFailed
                } else {
                    Fault::PreconditionRequired
                });
            }
            if book.0.len() >= LIMIT
                || book
                    .0
                    .iter()
                    .any(|((id, _), r)| *id == port && r.grant.refresh(&live))
            {
                return Err(Fault::Conflict);
            }
            json!({})
        };
        let policy = PatchPolicy {
            writable: &["/configuration"],
            server_owned: &["/_links", "/meta"],
            secret_slots: &[],
        };
        let staged = if req.method == Method::PATCH {
            patch::stage_patch(
                &json!({"configuration":prior["configuration"]}),
                req.body.as_bytes(),
                &policy,
                super::authoring::LIMITS,
            )
        } else {
            patch::stage_replacement(req.body.as_bytes(), &policy, super::authoring::LIMITS)
        }
        .map_err(|e| match e.kind {
            patch::PatchErrorKind::Syntax => Fault::Malformed,
            patch::PatchErrorKind::TestFailed => Fault::Conflict,
            _ => Fault::Invalid,
        })?;
        let settings: Settings = serde_json::from_value(staged.authoring["configuration"].clone())
            .map_err(|_| Fault::Invalid)?;
        let rev = settings.expected_active_revision;
        if !(1..=60_000).contains(&settings.ttl_ms) {
            return Err(Fault::Invalid);
        }
        if !req.connection.is_alive() || live.active_revision.as_deref() != Some(rev.as_str()) {
            return Err(Fault::Conflict);
        }
        let generation = *live.owners.get(&port).ok_or(Fault::Conflict)?;
        if settings.active && !self.runtime.permits_save(&rev) {
            return Err(Fault::Unavailable);
        }
        let until = Instant::now() + Duration::from_millis(settings.ttl_ms);
        let created = old.is_none();
        if let Some(row) = book.0.get_mut(&key) {
            let mut deadline = row.grant.deadline.lock().expect("maintenance deadline");
            if row.grant.revision != rev || deadline.is_none_or(|t| t <= Instant::now()) {
                return Err(Fault::Conflict);
            }
            *deadline = settings.active.then_some(until);
            drop(deadline);
            row.version += 1;
            row.ttl_ms = settings.ttl_ms;
            if !settings.active {
                self.runtime.clear_maintenance(row.grant.clone())?;
            }
        } else {
            if !settings.active {
                return Err(Fault::Invalid);
            }
            let grant = Arc::new(Grant {
                port,
                generation,
                revision: rev,
                mount: self.api.mount.clone(),
                actor: actor.to_owned(),
                connection: req.connection.clone(),
                deadline: Mutex::new(Some(until)),
                gate: Default::default(),
            });
            book.0.insert(
                key,
                Entry {
                    grant,
                    ttl_ms: settings.ttl_ms,
                    version: 1,
                },
            );
        }
        response_doc(
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            document(&book.0[&key]),
        )
    }
}
