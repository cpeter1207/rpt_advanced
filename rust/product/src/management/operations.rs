//! Boot-scoped request identities and durable admission before any side effect.
mod protected;
use super::*;
use protected::Operands;
use rpt_advanced_core::{
    command_language::{CommandSource, Invocation, InvocationContext, prepare_runtime_action},
    config::resources::manifest::ContentHash,
    management::patch::{self, PatchPolicy},
};
use rptadv_peer_support::{
    https::ServiceId,
    working_files::{EditGuard, immutable::InstallError},
};
use std::time::{SystemTime, UNIX_EPOCH};

// The leading @ is outside the authenticated TLS ServiceId alphabet.
const HOST_RELOAD: &str = "@host-lifecycle";
struct OperationInput<'a> {
    connection: Option<&'a rptadv_peer_support::https::ConnectionProof>,
    actor: &'a str,
    method: Method,
    target: &'a str,
    headers: &'a HeaderMap,
    body: &'a [u8],
}

const MAX_CURRENT_REQUESTS: usize = 256;
const MAX_HISTORICAL_REQUESTS: usize = 256;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const RETAIN_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Draft,
    Frozen,
    Completed,
    Rejected,
    Indeterminate,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Frozen => "submitted",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
            Self::Indeterminate => "indeterminate",
        }
    }
    fn read(s: &str) -> Option<Self> {
        [
            Self::Draft,
            Self::Frozen,
            Self::Completed,
            Self::Rejected,
            Self::Indeterminate,
        ]
        .into_iter()
        .find(|p| p.name() == s)
    }
}

#[derive(Clone)]
pub(super) struct Record {
    body: Value,
    version: Id,
    protected: u8,
    operands: Operands,
    retry_base: Option<Arc<(Value, Operands)>>,
    // Only an admission with uncertain durability may fence its preceding draft/create.
    uncertain_predecessor: Option<Option<Id>>,
    actor: String,
    phase: Phase,
    submitted_from: Option<String>,
    timestamp: u64,
    outcomes: BTreeMap<Id, &'static str>,
    results: BTreeMap<Id, maintenance::QueryResult>,
}

type Key = (Id, Id);
#[derive(Clone, Default)]
pub(super) struct Ledger {
    rows: BTreeMap<Key, Record>,
    digest: Option<[u8; 32]>,
}

fn id(v: &Value) -> Result<Id, Fault> {
    let text = v.as_str().ok_or(Fault::Invalid)?;
    let id: Id = text.parse().map_err(|_| Fault::Invalid)?;
    (id.to_string() == text).then_some(id).ok_or(Fault::Invalid)
}

fn now() -> Result<u64, Fault> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| Fault::Storage)
}

impl Ledger {
    pub fn read(guard: &EditGuard<'_>, boot: Id) -> Result<Self, Fault> {
        let Some(bytes) = guard.read_operations(MAX_BYTES).map_err(lock_error)? else {
            return Ok(Self::default());
        };
        let data = rptadv_json::decode_value(&bytes).map_err(|_| Fault::Storage)?;
        let rows = data["requests"]
            .as_array()
            .filter(|v| v.len() <= MAX_CURRENT_REQUESTS + MAX_HISTORICAL_REQUESTS)
            .ok_or(Fault::Storage)?;
        let format = data["format_version"]
            .as_u64()
            .filter(|v| [1, 2].contains(v))
            .ok_or(Fault::Storage)?;
        let mut ledger = Self {
            digest: Some(artifact_sha256(&bytes)),
            ..Default::default()
        };
        let timestamp = now()?;
        for row in rows {
            let key = (
                id(&row["boot"]).map_err(|_| Fault::Storage)?,
                id(&row["request"]).map_err(|_| Fault::Storage)?,
            );
            let time = row["time"].as_u64().ok_or(Fault::Storage)?;
            if key.0 != boot && timestamp.saturating_sub(time) > RETAIN_SECONDS {
                continue;
            }
            let actor = row["actor"].as_str().ok_or(Fault::Storage)?;
            if actor != HOST_RELOAD && ServiceId::new(actor).is_err() {
                return Err(Fault::Storage);
            }
            let actor = actor.to_owned();
            let mut phase =
                Phase::read(row["phase"].as_str().ok_or(Fault::Storage)?).ok_or(Fault::Storage)?;
            let body = row["configuration"].clone();
            let submit = body["submitted"].as_bool().ok_or(Fault::Storage)?;
            if (phase == Phase::Draft) == submit {
                return Err(Fault::Storage);
            }
            let (version, protected) = if format == 1 {
                (key.1, 0)
            } else {
                (
                    id(&row["version"]).map_err(|_| Fault::Storage)?,
                    row["protected"]
                        .as_u64()
                        .and_then(|v| u8::try_from(v).ok())
                        .ok_or(Fault::Storage)?,
                )
            };
            protected::validate_summary(&body, protected, &actor, submit)
                .map_err(|_| Fault::Storage)?;
            if phase == Phase::Frozen && key.0 != boot {
                phase = Phase::Indeterminate;
            }
            let mut outcomes = BTreeMap::new();
            for (target, result) in row["outcomes"].as_object().ok_or(Fault::Storage)? {
                let k = id(&json!(target)).map_err(|_| Fault::Storage)?;
                if !body
                    .get("targets")
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.iter().any(|v| id(v) == Ok(k)))
                {
                    return Err(Fault::Storage);
                }
                let v = match result.as_str() {
                    Some("admitted") => "admitted",
                    Some("rejected") => "rejected",
                    Some("indeterminate") => "indeterminate",
                    _ => return Err(Fault::Storage),
                };
                outcomes.insert(k, v);
            }
            let mut results = BTreeMap::new();
            if let Some(values) = row.get("results") {
                for (key, value) in values.as_object().ok_or(Fault::Storage)? {
                    let id = id(&json!(key)).map_err(|_| Fault::Storage)?;
                    if outcomes.get(&id) != Some(&"admitted") {
                        return Err(Fault::Storage);
                    }
                    let result: maintenance::QueryResult =
                        serde_json::from_value(value.clone()).map_err(|_| Fault::Storage)?;
                    if !result.valid(id) {
                        return Err(Fault::Storage);
                    }
                    results.insert(id, result);
                }
            }
            let submitted_from = match row.get("submitted_from") {
                None | Some(Value::Null) => None,
                Some(Value::String(tag))
                    if tag.len() == 66
                        && tag.starts_with('"')
                        && tag.ends_with('"')
                        && tag[1..65].bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    Some(tag.clone())
                }
                _ => return Err(Fault::Storage),
            };
            let record = Record {
                body,
                version,
                protected,
                operands: Operands::default(),
                retry_base: None,
                uncertain_predecessor: None,
                actor,
                phase,
                submitted_from,
                timestamp: time,
                outcomes,
                results,
            };
            if ledger.rows.insert(key, record).is_some() {
                return Err(Fault::Storage);
            }
        }
        // Current-boot execution identities are never evicted. Historical receipts expire
        // after seven days or when displaced by a newer historical receipt at the history bound.
        let mut historical = ledger
            .rows
            .iter()
            .filter(|((b, _), _)| *b != boot)
            .map(|(key, row)| (row.timestamp, *key))
            .collect::<Vec<_>>();
        historical.sort_unstable();
        for (_, key) in historical
            .iter()
            .take(historical.len().saturating_sub(MAX_HISTORICAL_REQUESTS))
        {
            ledger.rows.remove(key);
        }
        Ok(ledger)
    }

    fn write(
        &mut self,
        guard: &EditGuard<'_>,
        after_rename: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<(), Fault> {
        let rows=self.rows.iter().map(|((boot,request),r)|{
            let outcomes=r.outcomes.iter().map(|(id,s)|(id.to_string(),json!(s))).collect::<serde_json::Map<_,_>>();
            json!({"boot":boot.to_string(),"request":request.to_string(),"time":r.timestamp,"actor":r.actor.as_str(),"phase":r.phase.name(),"version":r.version.to_string(),"protected":r.protected,"submitted_from":r.submitted_from,"configuration":r.body,"outcomes":outcomes,"results":r.results.iter().map(|(id,v)|(id.to_string(),json!(v))).collect::<serde_json::Map<_,_>>()})
        }).collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&json!({"format_version":2,"requests":rows}))
            .map_err(|_| Fault::Storage)?;
        let result = guard
            .replace_operations_checked(self.digest.as_ref(), &bytes, MAX_BYTES, after_rename)
            .map_err(|e| match e {
                InstallError::Conflict => Fault::Conflict,
                InstallError::DurabilityUnknown => Fault::DurabilityUnknown,
                InstallError::Limits => Fault::TooLarge,
                _ => Fault::Storage,
            });
        if result.is_ok() || result == Err(Fault::DurabilityUnknown) {
            self.digest = Some(artifact_sha256(&bytes));
        }
        result
    }

    pub fn document(&self, api: &Api, boot: Id, request: Id) -> Result<Value, Fault> {
        let record = self.rows.get(&(boot, request)).ok_or(Fault::Missing)?;
        let path = format!("/operations/{boot}/{request}/");
        let mut links = api.links(&path, "operation");
        links["up"] = json!({"href":format!("{}/operations/",api.mount)});
        let outcomes = record
            .outcomes
            .iter()
            .map(|(id, v)| {
                let mut row = json!({"target":id.to_string(),"outcome":v});
                if let Some(query) = record.results.get(id) {
                    row["result"] = json!(query);
                }
                row
            })
            .collect::<Vec<_>>();
        Ok(
            json!({"_links":links,"configuration":record.body,"meta":{"request_version":record.version.to_string()},"receipt":{"state":record.phase.name(),"outcomes":outcomes}}),
        )
    }

    pub fn index(&self, api: &Api, boot: Id) -> Value {
        let mut links = api.links("/operations/", "operations");
        links["up"] = json!({"href":format!("{}/",api.mount)});
        links["item"] = json!(
            self.rows
                .keys()
                .map(|(b, r)| json!({"href":format!("{}/operations/{b}/{r}/",api.mount)}))
                .collect::<Vec<_>>()
        );
        links["rpt:request"] =
            json!({"href":format!("{}/operations/{boot}/{{request}}/",api.mount),"templated":true});
        json!({"_links":links,"boot_id":boot.to_string()})
    }
}

/// Complete action or a recognized, bounded draft. Drafts can omit action/targets until submission.
fn validate(body: &Value, actor: &str, submitted: bool) -> Result<(), Fault> {
    let fields = body.as_object().ok_or(Fault::Invalid)?;
    if actor == HOST_RELOAD && (!submitted || body["action"]["action"] != "reload_configuration") {
        return Err(Fault::Forbidden);
    }
    if fields.keys().any(|k| {
        ![
            "action",
            "targets",
            "expected_active_revision",
            "maintenance_lease",
            "desired_etag",
            "working_fingerprint",
            "restore",
            "submitted",
        ]
        .contains(&k.as_str())
    }) {
        return Err(Fault::Invalid);
    }
    if fields.get("submitted").is_some_and(|v| !v.is_boolean()) {
        return Err(Fault::Invalid);
    }
    if let Some(v) = fields.get("targets") {
        let items = v
            .as_array()
            .filter(|v| v.len() <= 64)
            .ok_or(Fault::Invalid)?;
        let ids = items.iter().map(id).collect::<Result<BTreeSet<_>, _>>()?;
        if ids.len() != items.len() {
            return Err(Fault::Invalid);
        }
    }
    for k in [
        "expected_active_revision",
        "desired_etag",
        "working_fingerprint",
    ] {
        if let Some(v) = fields.get(k)
            && !v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 256)
        {
            return Err(Fault::Invalid);
        }
    }
    if let Some(value) = fields.get("working_fingerprint") {
        value
            .as_str()
            .ok_or(Fault::Invalid)?
            .parse::<ContentHash>()
            .map_err(|_| Fault::Invalid)?;
    }
    if let Some(s) = fields.get("restore")
        && !matches!(s.as_str(), Some("desired" | "last_known_good"))
    {
        return Err(Fault::Invalid);
    }
    if let Some(href) = fields.get("maintenance_lease") {
        if body["action"]["action"] != "radio_maintenance"
            || !href
                .as_str()
                .is_some_and(|s| s.starts_with('/') && s.len() <= 1024)
        {
            return Err(Fault::Invalid);
        }
    }
    let Some(action) = fields.get("action") else {
        return if submitted {
            Err(Fault::Invalid)
        } else {
            Ok(())
        };
    };
    let name = action["action"].as_str().ok_or(Fault::Invalid)?;
    if matches!(
        name,
        "apply_configuration" | "restore_configuration" | "reload_configuration"
    ) {
        if action.as_object().map(|o| o.len()) != Some(1)
            || fields.contains_key("targets")
            || fields.contains_key("expected_active_revision")
            || (name != "restore_configuration" && fields.contains_key("restore"))
            || (name == "apply_configuration" && fields.contains_key("working_fingerprint"))
        {
            return Err(Fault::Invalid);
        }
        if submitted
            && (!fields.contains_key("desired_etag")
                || name != "apply_configuration" && !fields.contains_key("working_fingerprint")
                || name == "restore_configuration" && !fields.contains_key("restore"))
        {
            return Err(Fault::Invalid);
        }
    } else {
        let prepared = prepare_runtime_action(action).map_err(|_| Fault::Invalid)?;
        prepared
            .check_source(&CommandSource::Management(actor.to_owned()))
            .map_err(|_| Fault::Forbidden)?;
        if ["desired_etag", "working_fingerprint", "restore"]
            .iter()
            .any(|s| fields.contains_key(*s))
        {
            return Err(Fault::Invalid);
        }
        if submitted
            && (!fields.contains_key("expected_active_revision")
                || fields
                    .get("targets")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty))
        {
            return Err(Fault::Invalid);
        }
    }
    Ok(())
}

pub(super) enum Work<P> {
    Domain(
        String,
        Vec<(Id, Invocation)>,
        Option<Arc<maintenance::Grant>>,
    ),
    Configuration(Box<super::Recovery<P>>),
}

impl<R: Runtime> Backend<R> {
    pub(super) fn operation_snapshot(&self) -> Result<Arc<Ledger>, Fault> {
        if let Some(ledger) = self.ledger.read().expect("operation snapshot").as_ref() {
            return Ok(Arc::clone(ledger));
        }
        // Startup may have found the edit lock busy. Recover the real receipts
        // on demand without accepting a new boot identity or writing the ledger.
        let guard = self.store.files.try_edit().map_err(lock_error)?;
        let mut slot = self.ledger.write().expect("operation snapshot");
        if let Some(current) = slot.as_ref() {
            return Ok(Arc::clone(current));
        }
        let ledger = Arc::new(Ledger::read(&guard, self.boot)?);
        *slot = Some(Arc::clone(&ledger));
        Ok(ledger)
    }
    fn read_ledger(&self, guard: &EditGuard<'_>) -> Result<Ledger, Fault> {
        let mut ledger = Ledger::read(guard, self.boot)?;
        let prior = self.ledger.read().expect("operation snapshot");
        // A post-rename admission whose durability could not be established is never
        // dispatchable in this boot, even if a later observation sees the preceding draft.
        if let Some(prior) = prior.as_ref() {
            for (key, row) in &prior.rows {
                if let Some(predecessor) = row.uncertain_predecessor
                    && key.0 == self.boot
                    && row.phase == Phase::Indeterminate
                    && ledger.rows.get(key).map(|r| r.version) == predecessor
                {
                    ledger.rows.insert(*key, row.clone());
                }
            }
        }
        for (key, row) in ledger.rows.iter_mut().filter(|((b, _), _)| *b == self.boot) {
            if let Some(cached) = prior
                .as_ref()
                .and_then(|l| l.rows.get(key))
                .filter(|r| r.version == row.version)
            {
                row.operands = cached.operands.clone();
                row.retry_base = cached.retry_base.clone();
                row.uncertain_predecessor = cached.uncertain_predecessor;
                if row.phase == Phase::Frozen && cached.phase == Phase::Indeterminate {
                    row.phase = Phase::Indeterminate;
                }
            }
            if row.operands.mask() != row.protected {
                return Err(Fault::Storage);
            }
        }
        Ok(ledger)
    }
    fn admit_domain(&self, actor: &str, body: &Value) -> Result<Vec<(Id, Invocation)>, Fault> {
        let expected = body["expected_active_revision"]
            .as_str()
            .ok_or(Fault::Invalid)?;
        if self.runtime.observed().active_revision.as_deref() != Some(expected) {
            return Err(Fault::Conflict);
        }
        let a = prepare_runtime_action(&body["action"]).map_err(|_| Fault::Invalid)?;
        body["targets"]
            .as_array()
            .ok_or(Fault::Invalid)?
            .iter()
            .map(|target| {
                let target = id(target)?;
                let owner = self.runtime.operation_owner(target, expected, &a)?;
                let (Scope::Site(id) | Scope::Port(id)) = owner.scope;
                if id != target {
                    return Err(Fault::Conflict);
                }
                let context = InvocationContext {
                    owner: owner.scope,
                    generation: owner.generation,
                    requires_grant: owner.requires_grant,
                    source: CommandSource::Management(actor.to_owned()),
                };
                Ok((
                    target,
                    a.admit(&context, false).map_err(|_| Fault::Forbidden)?,
                ))
            })
            .collect()
    }

    pub fn operation(
        &self,
        req: &AdmittedRequest,
        p: Parameters<'_>,
    ) -> Result<(StatusCode, Value), Fault> {
        let (boot, request) = (
            p.boot.ok_or(Fault::Missing)?,
            p.request.ok_or(Fault::Missing)?,
        );
        self.edit_operation(
            OperationInput {
                actor: req.actor.as_str(),
                connection: Some(&req.connection),
                method: req.method.clone(),
                headers: &req.headers,
                body: req.body.as_bytes(),
                target: req.target.as_str(),
            },
            boot,
            request,
        )
    }

    /// Capture the validated working state before creating a normal, durable submission.
    /// The queue accepts no paths, actors, actions or validators from the signal caller.
    pub(super) fn service_reload(&self) -> Result<(), Fault> {
        self.refresh(false)?;
        let snapshot = self
            .cached
            .read()
            .expect("management snapshot")
            .clone()
            .ok_or(Fault::Unavailable)?;
        let tag = self.desired_validator(&snapshot)?;
        let fingerprint = snapshot.working.as_ref().ok_or(Fault::Storage)?;
        let body = serde_json::to_vec(&json!({"configuration":{
            "submitted":true, "action":{"action":"reload_configuration"},
            "desired_etag":tag, "working_fingerprint":fingerprint
        }}))
        .map_err(|_| Fault::Storage)?;
        let request = fresh()?;
        let target = format!("{}/operations/{}/{request}/", self.api.mount, self.boot);
        let mut headers = HeaderMap::new();
        headers.insert(
            "if-none-match",
            "*".parse().expect("static creation precondition"),
        );
        let (_, receipt) = self.edit_operation(
            OperationInput {
                actor: HOST_RELOAD,
                connection: None,
                method: Method::PUT,
                target: &target,
                headers: &headers,
                body: &body,
            },
            self.boot,
            request,
        )?;
        match receipt["receipt"]["state"].as_str() {
            Some("completed") => Ok(()),
            Some("rejected") => Err(Fault::Conflict),
            _ => Err(Fault::DurabilityUnknown),
        }
    }

    fn edit_operation(
        &self,
        req: OperationInput<'_>,
        boot: Id,
        request: Id,
    ) -> Result<(StatusCode, Value), Fault> {
        if self.boot != boot {
            return Err(Fault::Gone);
        }
        let key = (boot, request);
        let work = {
            let guard = self.store.files.try_edit().map_err(lock_error)?;
            let tx = Transaction::new(&guard, self.store.limits).with_write_key(&self.key);
            let mut ledger = self.read_ledger(&guard)?;
            let prior = ledger.rows.get(&key);
            if let Some(prior) = prior {
                if prior.actor.as_str() != req.actor {
                    return Err(Fault::Forbidden);
                }
            } else {
                if req.method != Method::PUT {
                    return Err(Fault::Missing);
                }
                if req.headers.get("if-none-match").is_none_or(|v| v != "*") {
                    return Err(if req.headers.contains_key("if-none-match") {
                        Fault::PreconditionFailed
                    } else {
                        Fault::PreconditionRequired
                    });
                }
                if ledger.rows.keys().filter(|(b, _)| *b == boot).count() >= MAX_CURRENT_REQUESTS {
                    return Err(Fault::Conflict);
                }
            }
            let original = prior.and_then(|r| {
                (req.method == Method::PATCH
                    && r.submitted_from
                        .as_ref()
                        .is_some_and(|tag| require(req.headers, tag).is_ok()))
                .then_some(r.retry_base.as_deref())
                .flatten()
            });
            let base = original
                .map(|(body, operands)| (body, operands))
                .or_else(|| prior.map(|r| (&r.body, &r.operands)));
            let current = base.map_or_else(|| json!({}), |(body, _)| body.clone());
            let mut operands = if req.method == Method::PATCH {
                base.map(|(_, operands)| operands.clone())
                    .unwrap_or_default()
            } else {
                Operands::default()
            };
            let policy = PatchPolicy {
                writable: &["/configuration"],
                server_owned: &["/_links", "/meta", "/receipt"],
                secret_slots: &protected::PATHS,
            };
            let staged = if req.method == Method::PUT {
                patch::stage_replacement(req.body, &policy, super::authoring::LIMITS)
            } else {
                patch::stage_draft_patch(
                    &json!({"configuration":current}),
                    req.body,
                    &policy,
                    super::authoring::LIMITS,
                )
            }
            .map_err(|e| match e.kind {
                patch::PatchErrorKind::Syntax => Fault::Malformed,
                patch::PatchErrorKind::TestFailed => Fault::Conflict,
                _ => Fault::Invalid,
            })?;
            let mut body = staged
                .authoring
                .get("configuration")
                .cloned()
                .ok_or(Fault::Invalid)?;
            let fields = body.as_object_mut().ok_or(Fault::Invalid)?;
            fields.entry("submitted").or_insert(json!(false));
            let submitted = body["submitted"].as_bool().ok_or(Fault::Invalid)?;
            operands.apply(staged.private_edits)?;
            operands.with_full(&body, |full| validate(full, req.actor, submitted))?;
            // The edit guard anchors both the verified receipt and every staged request read.
            // A refused apply drops this provisional lease before returning to read-only service.
            let fresh_lease = if submitted
                && body["action"]["action"] == "apply_configuration"
                && self
                    .store
                    .writer
                    .read()
                    .expect("store writer lifetime")
                    .is_none()
            {
                Some(self.store.files.try_writer_lease().map_err(lock_error)?)
            } else {
                None
            };
            self.admit_writer(&guard, fresh_lease.as_ref())?;
            let mut submitted_from = None;
            if let Some(prior) = prior {
                let doc = ledger.document(&self.api, boot, request)?;
                if prior.phase != Phase::Draft
                    && prior.body == body
                    && prior.operands.same(&operands)
                {
                    // A replay of an already durably frozen request cannot dispatch again.
                    let tag = validator(req.target, "", &doc);
                    let same_create = req.method == Method::PUT
                        && prior.submitted_from.is_none()
                        && req.headers.get("if-none-match").is_some_and(|v| v == "*");
                    if !same_create
                        && prior
                            .submitted_from
                            .as_deref()
                            .is_none_or(|t| require(req.headers, t).is_err())
                    {
                        require(req.headers, &tag)?;
                    }
                    return Ok((StatusCode::OK, doc));
                }
                if original.is_some() {
                    return Err(Fault::Conflict);
                }
                let tag = validator(req.target, "", &doc);
                require(req.headers, &tag)?;
                if submitted {
                    submitted_from = Some(tag);
                }
                if prior.phase != Phase::Draft {
                    return Err(Fault::Conflict);
                }
            }
            let selected = tx.inspect().map_err(|error| {
                let fault = Fault::from(error);
                self.runtime.inhibit_uncertain(None, fault);
                fault
            })?;
            if selected.private.credentials().is_err()
                || selected.status == StartupStatus::SecretsUnavailable
            {
                self.runtime.inhibit_uncertain(
                    Some(selected.state.desired.revision.as_str()),
                    Fault::Unavailable,
                );
                return Err(Fault::Unavailable);
            }
            if selected.status == StartupStatus::StorageFault
                || selected.bookkeeping_fault && !submitted
            {
                self.runtime.inhibit_uncertain(
                    Some(selected.state.desired.revision.as_str()),
                    Fault::Storage,
                );
                return Err(Fault::Storage);
            }
            // Enforce the external write-key policy even for empty public-only installations.
            rptadv_peer_support::sealed_records::load_systemd_key(&self.key)
                .map_err(|_| Fault::Unavailable)?;
            let work = if submitted {
                match body["action"]["action"].as_str() {
                    Some(
                        "apply_configuration" | "restore_configuration" | "reload_configuration",
                    ) => Some(Work::Configuration(Box::new(
                        self.prepare_recovery(&tx, &selected, &body)?,
                    ))),
                    _ => {
                        if selected.bookkeeping_fault
                            || matches!(
                                selected.status,
                                StartupStatus::StorageFault | StartupStatus::SecretsUnavailable
                            )
                        {
                            return Err(Fault::Unavailable);
                        }
                        let grant = if body["action"]["action"] == "radio_maintenance" {
                            let [target] =
                                body["targets"].as_array().ok_or(Fault::Invalid)?.as_slice()
                            else {
                                return Err(Fault::Invalid);
                            };
                            Some(
                                self.maintenance_grant(
                                    body["maintenance_lease"].as_str().ok_or(Fault::Forbidden)?,
                                    id(target)?,
                                    body["expected_active_revision"]
                                        .as_str()
                                        .ok_or(Fault::Invalid)?,
                                    req.actor,
                                    req.connection.ok_or(Fault::Forbidden)?,
                                )?,
                            )
                        } else {
                            None
                        };
                        Some(Work::Domain(
                            body["expected_active_revision"]
                                .as_str()
                                .ok_or(Fault::Invalid)?
                                .to_owned(),
                            operands.with_full(&body, |full| self.admit_domain(req.actor, full))?,
                            grant,
                        ))
                    }
                }
            } else {
                None
            };
            let retry_base = if submitted && req.method == Method::PATCH {
                prior.map(|r| Arc::new((r.body.clone(), r.operands.clone())))
            } else {
                None
            };
            let predecessor = prior.map(|r| r.version);
            ledger.rows.insert(
                key,
                Record {
                    body,
                    version: fresh()?,
                    protected: operands.mask(),
                    operands,
                    retry_base,
                    uncertain_predecessor: None,
                    actor: req.actor.to_owned(),
                    submitted_from,
                    timestamp: now()?,
                    phase: if submitted {
                        Phase::Frozen
                    } else {
                        Phase::Draft
                    },
                    outcomes: BTreeMap::new(),
                    results: BTreeMap::new(),
                },
            );
            if let Some(lease) = fresh_lease {
                // Keep the authorized lease from the first durable intent through native drain.
                let mut writer = self.store.writer.write().expect("store writer lifetime");
                if writer.is_some() {
                    return Err(Fault::Conflict);
                }
                *writer = Some(HeldWriter {
                    lease,
                    provisional: false,
                });
            }
            let admission = ledger.write(&guard, || {
                #[cfg(test)]
                if !self.runtime.admission_rename_observed() {
                    return Err(InstallError::DurabilityUnknown);
                }
                Ok(())
            });
            if admission == Err(Fault::DurabilityUnknown) {
                if submitted {
                    let row = ledger.rows.get_mut(&key).expect("staged request");
                    row.phase = Phase::Indeterminate;
                    row.uncertain_predecessor = Some(predecessor);
                }
                // The CAS is against the exact just-renamed bytes. Never dispatch after
                // uncertainty, even if the terminal receipt's sync must also be retried.
                let _ = ledger.write(&guard, || Ok(()));
                *self.ledger.write().expect("operation snapshot") = Some(Arc::new(ledger));
                self.runtime
                    .inhibit_uncertain(None, Fault::DurabilityUnknown);
                return Err(Fault::DurabilityUnknown);
            }
            admission?;
            let doc = ledger.document(&self.api, boot, request)?;
            *self.ledger.write().expect("operation snapshot") = Some(Arc::new(ledger));
            if !submitted {
                return Ok((StatusCode::OK, doc));
            }
            work.expect("submitted operation admitted")
        };
        let mut outcomes = BTreeMap::new();
        let mut results = BTreeMap::new();
        let phase = match work {
            Work::Domain(expected, targets, grant) => {
                for (id, intent) in targets {
                    let result = if intent.context.recheck(false).is_err()
                        || self.runtime.observed().active_revision.as_deref() != Some(&expected)
                    {
                        Effect::Rejected
                    } else if let Some(grant) = &grant {
                        if grant.valid() && grant.generation == intent.context.generation.id() {
                            self.runtime.maintenance(Arc::clone(grant), intent)
                        } else {
                            Effect::Rejected
                        }
                    } else {
                        self.runtime.dispatch(&expected, intent)
                    };
                    outcomes.insert(id, result.label());
                    if let Effect::Query(query) = result {
                        results.insert(id, query);
                    }
                }
                if outcomes.values().any(|s| *s == "indeterminate") {
                    Phase::Indeterminate
                } else if outcomes.values().any(|s| *s == "rejected") {
                    Phase::Rejected
                } else {
                    Phase::Completed
                }
            }
            Work::Configuration(recovery) => match self.execute_recovery(*recovery) {
                Ok(()) => Phase::Completed,
                Err(Fault::DurabilityUnknown | Fault::Storage) => Phase::Indeterminate,
                Err(_) => Phase::Rejected,
            },
        };
        let recorded: Result<_, Fault> = (|| {
            let guard = self.store.files.try_edit().map_err(lock_error)?;
            self.admit_writer(&guard, None)?;
            let mut ledger = self.read_ledger(&guard)?;
            let r = ledger
                .rows
                .get_mut(&key)
                .filter(|r| r.phase == Phase::Frozen)
                .ok_or(Fault::Conflict)?;
            r.phase = phase;
            r.outcomes = outcomes;
            r.results = results;
            ledger.write(&guard, || Ok(()))?;
            let doc = ledger.document(&self.api, boot, request)?;
            *self.ledger.write().expect("operation snapshot") = Some(Arc::new(ledger));
            Ok((StatusCode::OK, doc))
        })();
        if recorded.is_err() {
            let mut snapshot = self.ledger.write().expect("operation snapshot");
            if let Some(row) = snapshot
                .as_mut()
                .and_then(|l| Arc::make_mut(l).rows.get_mut(&key))
            {
                row.phase = Phase::Indeterminate;
            }
            self.runtime
                .inhibit_uncertain(None, Fault::DurabilityUnknown);
            return Err(Fault::DurabilityUnknown);
        }
        recorded
    }
}
