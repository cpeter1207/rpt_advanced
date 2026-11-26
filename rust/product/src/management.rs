//! Authenticated HAL management of the durable desired state, outside the media plane.
mod authoring;
mod graph;
pub(crate) mod live;
pub mod maintenance;
pub(crate) mod native;
mod operations;
mod recovery;
use recovery::Recovery;
mod openapi;
mod projections;
#[cfg(test)]
pub(crate) static FULL_PREPARATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
mod representation;
mod schema;

use crate::{
    configuration_private::CredentialEdit,
    configuration_transaction::{
        Admission, Publication, Selected, StartupStatus, Transaction, TransactionError,
    },
};
use graph::{Graph, fresh};
pub use projections::PreparedConfiguration;
use representation::{Api, binding_id};
use rpt_advanced_core::{
    config::resources::{
        candidate::{
            ResourceId as Id, ResourceKind as Kind, ResourceRegistry, ResourceScope as Scope,
        },
        profiles::credentials::CredentialSlot,
        state::{Activation, ManifestRef},
    },
    management::routes::{Parameters, Route, Target},
};
use rptadv_peer_support::{
    https::{AdmittedRequest, CoordinatorInbox, HeaderMap, Method, Response, StatusCode},
    sealed_records::KeyId,
    working_files::{
        CaptureError, CaptureLimits, EditGuard, WorkingFiles, WriterLease,
        immutable::artifact_sha256,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::{
        Arc, RwLock,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

/// Value-free coordinator failure; caller-supplied JSON never enters errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// No current resource at this path.
    Missing,
    /// Malformed request or routing parameters.
    Malformed,
    /// Required strong resource precondition absent.
    PreconditionRequired,
    /// Validator does not match the selected representation.
    PreconditionFailed,
    /// Unsupported method for this profile.
    Method,
    /// Representation media type is unsuitable.
    Media,
    /// Client does not accept the response representation.
    Accept,
    /// An operation belongs to an earlier service boot.
    Gone,
    /// The authenticated caller lacks the source required for this effect.
    Forbidden,
    /// Body exceeds staging limits.
    TooLarge,
    /// Invalid authoring projection.
    Invalid,
    /// Existing member, ownership, or pending transition conflicts.
    Conflict,
    /// The selected private key or runtime/domain owner is unavailable.
    Unavailable,
    /// The durable store could not be verified.
    Storage,
    /// The commit point's durability could not be verified.
    DurabilityUnknown,
}
impl Fault {
    fn wire(self) -> (StatusCode, &'static str) {
        use Fault::*;
        match self {
            Gone => (StatusCode::GONE, "expired_boot"),
            Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Missing => (StatusCode::NOT_FOUND, "not_found"),
            Malformed => (StatusCode::BAD_REQUEST, "malformed"),
            PreconditionRequired => (StatusCode::PRECONDITION_REQUIRED, "precondition_required"),
            PreconditionFailed => (StatusCode::PRECONDITION_FAILED, "precondition_failed"),
            Method => (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            Media => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"),
            Accept => (StatusCode::NOT_ACCEPTABLE, "not_acceptable"),
            TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            Invalid => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_resource"),
            Conflict => (StatusCode::CONFLICT, "conflict"),
            Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Storage => (StatusCode::SERVICE_UNAVAILABLE, "storage_fault"),
            DurabilityUnknown => (StatusCode::SERVICE_UNAVAILABLE, "durability_unknown"),
        }
    }
}
impl From<TransactionError> for Fault {
    fn from(e: TransactionError) -> Self {
        match e {
            TransactionError::Conflict | TransactionError::ExternalEdit => Self::Conflict,
            TransactionError::InvalidCandidate => Self::Invalid,
            TransactionError::SecretsUnavailable => Self::Unavailable,
            TransactionError::StorageFault => Self::Storage,
            TransactionError::DurabilityUnknown => Self::DurabilityUnknown,
        }
    }
}

/// Active state supplied exclusively by the running lifecycle owner.
#[derive(Clone, Default)]
pub struct Observed {
    pub(crate) domain: Arc<live::Domain>,
    /// Actually adopted revision, absent in management-only recovery.
    pub active_revision: Option<String>,
    /// Generation observed for each actual resource owner.
    pub owners: BTreeMap<Id, u64>,
}

/// Actual owner token and action policy from the existing lifecycle/dispatcher.
pub struct OperationOwner {
    /// Real active target, including its site or port authority.
    pub scope: Scope,
    /// Live generation work, retained by the normal command invocation.
    pub generation: Arc<rpt_advanced_core::runtime::GenerationWork>,
    /// Whether this owner requires an actual receiver/connection grant.
    pub requires_grant: bool,
}
/// Result at the existing domain dispatcher's admission boundary.
pub enum Effect {
    /// The domain accepted the operation.
    Admitted,
    /// A successful bounded typed radio query.
    Query(maintenance::QueryResult),
    /// No effect was admitted.
    Rejected,
    /// Effect status could not be established.
    Indeterminate,
}
impl Effect {
    fn label(&self) -> &'static str {
        match self {
            Self::Admitted | Self::Query(_) => "admitted",
            Self::Rejected => "rejected",
            Self::Indeterminate => "indeterminate",
        }
    }
}

/// Value-free events delivered to the existing control host for logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    /// A public resource was normalized; values and untrusted field names are excluded.
    Resource(rpt_advanced_core::config::resources::candidate::CandidateWarning),
    /// A resolved command layer ignored an invalid or unsupported setting in this scope.
    Command(Scope, rpt_advanced_core::command_language::LanguageWarning),
    /// One coalesced transport availability event.
    Transport(rptadv_peer_support::https::TransportEvent),
    /// An envelope or private codec returned a recoverable warning.
    PrivateSettings(usize),
}

/// A terminal observation from the existing serialized hardware/runtime owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdoptionOutcome {
    /// All affected owners confirmed the requested revision.
    Confirmed,
    /// The attempted transition failed and rollback was confirmed.
    RolledBack,
    /// An affected owner could not verify its state or rollback.
    RecoveryRequired,
}
impl AdoptionOutcome {
    fn state(self) -> Activation {
        match self {
            Self::Confirmed => Activation::Confirmed,
            Self::RolledBack => Activation::Failed,
            Self::RecoveryRequired => Activation::RecoveryRequired,
        }
    }
}

/// Runtime adoption may complete immediately or return one bounded outstanding confirmation.
pub struct Adoption(Confirmation);
enum Confirmation {
    Observed(AdoptionOutcome),
    Pending(mpsc::Receiver<AdoptionOutcome>),
}
impl Adoption {
    /// Record an actual terminal observation at the host boundary.
    pub fn observed(result: AdoptionOutcome) -> Self {
        Self(Confirmation::Observed(result))
    }
    /// Give the existing owner a single bounded channel for its later terminal confirmation.
    pub fn pending() -> (Self, SyncSender<AdoptionOutcome>) {
        let (send, recv) = mpsc::sync_channel(1);
        (Self(Confirmation::Pending(recv)), send)
    }
}

/// Runtime handoff. Preparation is effect-free, adoption returns the actual owner result.
/// Implementations must recheck the retained owner/generation before applying effects.
pub trait Runtime: Send + Sync + 'static {
    /// An owned prepared transition, independent of borrowed store/private buffers.
    type Prepared: Send;
    /// Whether this process can authorize an ordinary save against this selected revision.
    /// Recovery operations retain their own explicit authorization and preconditions.
    fn permits_save(&self, revision: &str) -> bool;
    /// Dispatch one radio operation through the exact, still-live physical grant.
    fn maintenance(
        &self,
        _grant: Arc<maintenance::Grant>,
        _intent: rpt_advanced_core::command_language::Invocation,
    ) -> Effect {
        Effect::Rejected
    }
    /// Return after any outstanding manual overrides on this exact owner are cleared.
    fn clear_maintenance(&self, _grant: Arc<maintenance::Grant>) -> Result<(), Fault> {
        Ok(())
    }
    /// Validate all remaining domain relationships and prepare runtime adoption.
    fn prepare(
        &self,
        configuration: &PreparedConfiguration,
        revision: &str,
    ) -> Result<Self::Prepared, Fault>;
    /// Adopt a durable and published desired generation. A dropped or uncertain handoff requires recovery.
    fn adopt(&self, prepared: Self::Prepared) -> Adoption;
    /// Complete account preparation with the store's authenticated final ownership bindings.
    /// No provider or runtime effects are permitted before durable selection and publication.
    fn prepare_credentials(
        &self,
        prepared: &mut Self::Prepared,
        credentials: &crate::configuration_private::Credentials,
    ) -> Result<(), Fault>;
    /// Inhibit affected transmit owners when storage, private state, or activation durability is uncertain.
    /// This notification runs outside the media callback and carries no private material.
    fn inhibit_uncertain(&self, revision: Option<&str>, cause: Fault);
    /// Test observation after releasing the publishing guard and before runtime adoption.
    #[cfg(test)]
    fn after_publication(&self) {}
    /// Test fault at the operation ledger rename, before the root directory sync.
    #[cfg(test)]
    fn admission_rename_observed(&self) -> bool {
        true
    }
    /// Test observation after private preparation and immediately before final inspection.
    #[cfg(test)]
    fn before_final_inspection(&self) {}
    /// Test observation of the existing recovery desired-selector rename.
    #[cfg(test)]
    fn recovery_desired_rename_observed(&self) -> bool {
        true
    }
    /// Report a value-free diagnostic through the existing control-host logger.
    fn diagnostic(&self, event: Diagnostic);
    /// Read a bounded immutable owner snapshot without waiting for disk or adoption.
    fn observed(&self) -> Observed;
    /// Acquire real owner/generation against the expected active revision.
    fn operation_owner(
        &self,
        target: Id,
        expected_active: &str,
        action: &rpt_advanced_core::command_language::PreparedAction,
    ) -> Result<OperationOwner, Fault>;
    /// Dispatch through the normal domain handler with its actual source and ownership.
    fn dispatch(
        &self,
        expected_active: &str,
        invocation: rpt_advanced_core::command_language::Invocation,
    ) -> Effect;
}

/// Pristine catalog sources and capabilities supplied by the hardware inventory owner.
pub struct ProfileInputs {
    /// Installed source layers shared with this station's generation preparation.
    pub messages: rpt_advanced_core::messages::InstalledMessageSources,
    /// Cached public interface capabilities indexed by equipment kind, real port ID and property pointer.
    pub interfaces: BTreeMap<
        (Kind, Id, String),
        rpt_advanced_core::config::resources::profiles::equipment::InterfaceCapability,
    >,
}
impl ProfileInputs {
    pub(crate) fn installed() -> Arc<Self> {
        Arc::new(Self {
            messages: rpt_advanced_core::messages::InstalledMessageSources::capture(),
            interfaces: BTreeMap::new(),
        })
    }
}

// Immutable inputs belong to one Backend. A fresh coordinator starts with an empty cache.
struct PreparedViews {
    selection: ManifestRef,
    result: Result<PreparedConfiguration, Fault>,
    credential_slots: Vec<(Id, CredentialSlot)>,
}
struct Snapshot {
    registry: ResourceRegistry,
    revision: String,
    last_good: Option<String>,
    activation: &'static str,
    storage: &'static str,
    working: Option<String>,
    versions: BTreeMap<Id, String>,
    views: Arc<PreparedViews>,
    secrets_state: &'static str,
    configured: BTreeMap<Id, BTreeMap<&'static str, bool>>,
}
impl Snapshot {
    fn from_selected(s: &Selected, views: Arc<PreparedViews>) -> Self {
        let manifest = s.desired.manifest();
        let registry = manifest.registry();
        use crate::configuration_private::SecretsState;
        let secrets_state = match s.private.state() {
            SecretsState::Ready => "ready",
            SecretsState::Locked => "locked",
            SecretsState::Error => "error",
        };
        let mut configured = BTreeMap::<Id, BTreeMap<&'static str, bool>>::new();
        for &(id, slot) in &views.credential_slots {
            let flags = configured.entry(id).or_default();
            if let Some(value) = s.private.configured(id, slot) {
                let field = match slot {
                    CredentialSlot::AllStarLink
                    | CredentialSlot::Directory
                    | CredentialSlot::Proxy
                    | CredentialSlot::Phone => "password_configured",
                    CredentialSlot::Pin => "pin_configured",
                    CredentialSlot::Unlock => "unlock_configured",
                    CredentialSlot::Lock => "lock_configured",
                };
                flags.insert(field, value);
            }
        }
        Self {
            registry: registry.clone(),
            revision: manifest.revision().as_str().to_owned(),
            last_good: s
                .state
                .last_known_good
                .as_ref()
                .map(|r| r.revision.as_str().to_owned()),
            activation: match s.state.activation {
                Activation::Pending => "pending",
                Activation::Confirmed => "confirmed",
                Activation::Failed => "failed",
                Activation::RecoveryRequired => "recovery_required",
            },
            storage: if s.bookkeeping_fault {
                "storage_fault"
            } else {
                match s.status {
                    StartupStatus::Ready => "ready",
                    StartupStatus::Interrupted => "interrupted",
                    StartupStatus::ExternalEdit => "external_edit",
                    StartupStatus::StorageFault => "storage_fault",
                    StartupStatus::SecretsUnavailable => "secrets_unavailable",
                }
            },
            working: s.observed_working.map(|hash| hex(hash.as_bytes())),
            versions: registry
                .bindings()
                .filter_map(|b| {
                    Some((
                        b.id,
                        manifest.reference(b.id)?.version().as_str().to_owned(),
                    ))
                })
                .collect(),
            views,
            secrets_state,
            configured,
        }
    }
}

/// Outcome of a successful public inspection, including restrictions on a later handoff.
struct Inspection {
    projection_error: Option<Fault>,
    status: StartupStatus,
    bookkeeping_fault: bool,
}
/// A lease held during opening stays provisional until its first guarded admission check.
struct HeldWriter {
    lease: WriterLease,
    provisional: bool,
}

/// The protected store shared by the management worker and the native owner through drain.
pub struct ManagedStore {
    files: WorkingFiles,
    trusted: Id,
    limits: CaptureLimits,
    writer: RwLock<Option<HeldWriter>>,
}

impl ManagedStore {
    /// Open the externally identified store. A blocked, ownerless startup releases its
    /// attempted lease so the offline maintenance owner can finish its operation.
    pub fn open(
        path: &std::path::Path,
        trusted: Id,
        limits: CaptureLimits,
    ) -> Result<Self, rpt_advanced_core::runtime::RuntimeError> {
        use rpt_advanced_core::runtime::RuntimeError;
        let files = WorkingFiles::open(path)
            .map_err(|_| RuntimeError::Preparation)?
            .with_identity(trusted.uuid());
        let lease = match files.try_writer_lease() {
            Ok(lease) => Some(lease),
            Err(CaptureError::Busy) => None,
            Err(_) => return Err(RuntimeError::Preparation),
        };
        let writer = match files.try_edit() {
            Ok(guard)
                if Transaction::new(&guard, limits)
                    .ordinary_writer_admission(trusted)
                    .is_ok() =>
            {
                let lease = lease.ok_or(RuntimeError::Busy)?;
                guard
                    .check_writer_lease(&lease)
                    .is_ok()
                    .then_some(HeldWriter {
                        lease,
                        provisional: false,
                    })
            }
            Err(CaptureError::Busy) => lease.map(|lease| HeldWriter {
                lease,
                provisional: true,
            }),
            _ => None,
        };
        Ok(Self {
            files,
            trusted,
            limits,
            writer: RwLock::new(writer),
        })
    }

    /// Verify the lifetime and maintenance record under the guard used for the effect.
    fn check_writer(
        &self,
        guard: &EditGuard<'_>,
        fresh: Option<&WriterLease>,
    ) -> Result<(), Fault> {
        let (proof, provisional) = {
            let writer = self.writer.read().expect("store writer lifetime");
            let held = writer.as_ref();
            let lease = held
                .map(|owner| &owner.lease)
                .or(fresh)
                .ok_or(Fault::Conflict)?;
            (
                guard.check_writer_lease(lease).map_err(lock_error),
                held.is_some_and(|owner| owner.provisional),
            )
        };
        let admission = proof.and_then(|()| {
            Transaction::new(guard, self.limits)
                .ordinary_writer_admission(self.trusted)
                .map_err(Fault::from)
        });
        if provisional {
            // The edit guard serializes admissions. A failed cold-start may have released
            // the lease while the receipt was being checked.
            let mut writer = self.writer.write().expect("store writer lifetime");
            let held = writer.as_mut().ok_or(Fault::Conflict)?;
            if admission.is_ok() {
                held.provisional = false;
            } else {
                *writer = None;
            }
        }
        admission
    }
}

struct Backend<R> {
    stopping: std::sync::atomic::AtomicBool,
    boot: Id,
    ledger: RwLock<Option<Arc<operations::Ledger>>>,
    maintenance: std::sync::Mutex<maintenance::Book>,
    store: Arc<ManagedStore>,
    key: KeyId,
    inputs: Arc<ProfileInputs>,
    runtime: Arc<R>,
    api: Arc<Api>,
    cached: Arc<RwLock<Option<Arc<Snapshot>>>>,
}

impl<R: Runtime> Backend<R> {
    fn admit_writer(
        &self,
        guard: &EditGuard<'_>,
        fresh: Option<&WriterLease>,
    ) -> Result<(), Fault> {
        self.store.check_writer(guard, fresh).inspect_err(|fault| {
            self.runtime.inhibit_uncertain(None, *fault);
        })
    }

    fn snapshot(
        &self,
        selected: &Selected,
        prepared: Option<PreparedConfiguration>,
    ) -> Result<Snapshot, Fault> {
        let prior = self
            .cached
            .read()
            .expect("management snapshot")
            .as_ref()
            .filter(|s| prepared.is_none() && s.views.selection == selected.state.desired)
            .map(|s| Arc::clone(&s.views));
        let views = match prior {
            Some(views) => views,
            None => {
                let candidate = selected
                    .desired
                    .assemble_candidate()
                    .map_err(|_| Fault::Storage)?;
                let credential_slots = candidate
                    .credential_owners()
                    .map_err(|_| Fault::Invalid)?
                    .into_keys()
                    .collect();
                let result = prepared.map_or_else(
                    || {
                        let zone = crate::schedule_zone::capture();
                        PreparedConfiguration::prepare(&candidate, zone.as_deref(), &self.inputs)
                    },
                    Ok,
                );
                Arc::new(PreparedViews {
                    selection: selected.state.desired.clone(),
                    result,
                    credential_slots,
                })
            }
        };
        Ok(Snapshot::from_selected(selected, views))
    }
    fn refresh(&self, startup: bool) -> Result<(), Fault> {
        let result = self.inspect_snapshot(startup);
        if let Err(error) = result
            && (startup || error != Fault::Conflict)
        {
            let previous = self.cached.write().expect("management snapshot").take();
            self.runtime
                .inhibit_uncertain(previous.as_ref().map(|s| s.revision.as_str()), error);
        }
        result
    }

    /// A clean historical selection is eligible for a real cold-start handoff.
    /// Unconfirmed, externally edited or interrupted stores stay management-only.
    fn cold_start(&self) -> Result<(), Fault> {
        let (revision, prepared) = {
            let guard = self
                .store
                .files
                .try_edit()
                .map_err(lock_error)
                .and_then(|guard| {
                    self.store.check_writer(&guard, None)?;
                    Ok(guard)
                })
                .inspect_err(|_| {
                    // No native handoff has started. Leave the directory free for maintenance.
                    self.store
                        .writer
                        .write()
                        .expect("store writer lifetime")
                        .take();
                })?;
            let tx = Transaction::new(&guard, self.store.limits).with_write_key(&self.key);
            let selected = tx.inspect()?;
            if selected.status != StartupStatus::Ready
                || selected.bookkeeping_fault
                || selected.state.activation != Activation::Confirmed
                || selected.state.journal.is_some()
            {
                return Err(Fault::Unavailable);
            }
            let snapshot = self.snapshot(&selected, None)?;
            let configuration = snapshot.views.result.as_ref().map_err(|error| *error)?;
            for warning in &configuration.public.warnings {
                self.runtime.diagnostic(*warning);
            }
            let mut prepared = self.runtime.prepare(configuration, &snapshot.revision)?;
            self.runtime.prepare_credentials(
                &mut prepared,
                selected
                    .private
                    .credentials()
                    .map_err(|_| Fault::Unavailable)?,
            )?;
            self.admit_selected(
                &tx,
                &selected.state.desired,
                false,
                None,
                Some(selected.digest),
            )?;
            (snapshot.revision, prepared)
        };
        let outcome = self.adopt(&revision, prepared);
        self.finish_adoption(&revision, outcome)
    }

    fn inspect_snapshot(&self, startup: bool) -> Result<(), Fault> {
        let guard = self.store.files.try_edit().map_err(lock_error)?;
        let tx = Transaction::new(&guard, self.store.limits).with_write_key(&self.key);
        let mut selected = tx.inspect()?;
        if self.store.check_writer(&guard, None).is_err() {
            selected.status = StartupStatus::StorageFault;
        }
        self.publish_verified(selected, startup, None).map(|_| ())
    }

    fn publish_snapshot(
        &self,
        tx: &Transaction<'_, '_>,
        startup: bool,
    ) -> Result<Inspection, Fault> {
        self.publish_verified(tx.inspect()?, startup, None)
    }

    fn publish_verified(
        &self,
        selected: Selected,
        startup: bool,
        prepared: Option<PreparedConfiguration>,
    ) -> Result<Inspection, Fault> {
        let view = self.snapshot(&selected, prepared)?;
        let projection_error = view.views.result.as_ref().err().copied();
        // The failing bookkeeping write issued its inhibit at the point of failure.
        // Polling that same latch must not invalidate a later explicit repair.
        let blocked = if selected.status == StartupStatus::SecretsUnavailable {
            Some(Fault::Unavailable)
        } else if selected.status == StartupStatus::StorageFault
            || startup
                && (selected.status == StartupStatus::Interrupted
                    || selected.state.activation != Activation::Confirmed
                    || selected.state.journal.is_some())
        {
            Some(Fault::Storage)
        } else {
            projection_error
        };
        if let Some(fault) = blocked {
            self.runtime.inhibit_uncertain(Some(&view.revision), fault);
        }
        *self.cached.write().expect("management snapshot") = Some(Arc::new(view));
        Ok(Inspection {
            projection_error,
            status: selected.status,
            bookkeeping_fault: selected.bookkeeping_fault,
        })
    }

    /// Final private/store inspection at the adoption boundary. Readable public state
    /// remains available for locked or damaged selections, even when the handoff is denied.
    fn admit_selected(
        &self,
        tx: &Transaction<'_, '_>,
        expected: &ManifestRef,
        repairing_bookkeeping: bool,
        prepared: Option<PreparedConfiguration>,
        cold_selection: Option<rpt_advanced_core::config::resources::manifest::ContentHash>,
    ) -> Result<(), Fault> {
        #[cfg(test)]
        self.runtime.before_final_inspection();
        // The receipt binds the runtime plan to the full committed manifest, including its
        // private reference. Recheck current storage, key and working state under this guard.
        let inspection = tx
            .inspect()
            .map_err(Fault::from)
            .and_then(|selected| {
                if selected.state.desired != *expected
                    || cold_selection.is_some_and(|digest| digest != selected.digest)
                {
                    self.publish_verified(selected, false, None)?;
                    return Err(Fault::DurabilityUnknown);
                }
                self.publish_verified(selected, false, prepared)
            })
            .map_err(|fault| {
                self.runtime
                    .inhibit_uncertain(Some(expected.revision.as_str()), fault);
                Fault::Unavailable
            })?;
        match inspection.status {
            StartupStatus::SecretsUnavailable | StartupStatus::StorageFault => {
                Err(Fault::Unavailable)
            }
            StartupStatus::ExternalEdit => Err(Fault::Conflict),
            _ if inspection.bookkeeping_fault && !repairing_bookkeeping => Err(Fault::Unavailable),
            _ => inspection.projection_error.map_or(Ok(()), Err),
        }
    }

    fn mutate(
        &self,
        req: &AdmittedRequest,
        r: &Route,
        p: Parameters<'_>,
        relative: &str,
    ) -> Result<(StatusCode, Value, R::Prepared, String), Fault> {
        let guard = self.store.files.try_edit().map_err(lock_error)?;
        self.admit_writer(&guard, None)?;
        let transaction = Transaction::new(&guard, self.store.limits).with_write_key(&self.key);
        let selected = transaction.inspect()?;
        let snapshot = self.snapshot(&selected, None)?;
        let prior = self.api.read(
            r,
            p,
            relative,
            Some(&snapshot),
            &self.runtime.observed(),
            None,
        )?;
        let tag = validator(req.target.as_str(), &snapshot.revision, &prior);
        require(&req.headers, &tag)?;
        if selected.status == StartupStatus::SecretsUnavailable {
            return Err(Fault::Unavailable);
        }
        if selected.bookkeeping_fault || selected.status == StartupStatus::StorageFault {
            return Err(Fault::Storage);
        }
        if !self.runtime.permits_save(&snapshot.revision) {
            return Err(Fault::Conflict);
        }
        let scope = p
            .port
            .map_or(Scope::Site(snapshot.registry.site_id()), Scope::Port);
        let kind = match r.target {
            Target::Collection(k)
            | Target::Member(k)
            | Target::Credentials(k)
            | Target::Section(k) => k,
            Target::Metadata => Kind::Metadata,
            _ => return Err(Fault::Method),
        };
        let mut graph = Graph::selected(&selected.desired);
        let mut private = Vec::<CredentialEdit>::new();
        let mut written = None;
        let mut created = None;
        if req.method == Method::DELETE {
            let members = if matches!(r.target, Target::Collection(_)) {
                Vec::new()
            } else {
                let id = binding_id(&snapshot, r, p).ok_or(Fault::Missing)?;
                graph
                    .members(scope, kind)
                    .into_iter()
                    .filter(|m| *m != id)
                    .collect()
            };
            graph.set_members(scope, kind, &members)?;
        } else {
            let base = if kind == Kind::Event && !matches!(r.target, Target::Collection(_)) {
                let mut body = prior;
                let fields = body.as_object_mut().ok_or(Fault::Invalid)?;
                fields.remove("meta");
                fields.remove("_links");
                fields.remove("key");
                fields.remove("id");
                body
            } else {
                json!({"configuration":prior.get("configuration").cloned().unwrap_or(json!({}))})
            };
            let edit = authoring::stage(&req.method, r.target, kind, base, req.body.as_bytes())?;
            if req.method != Method::POST && matches!(r.target, Target::Collection(_)) {
                let ids = edit
                    .fields
                    .get("members")
                    .and_then(Value::as_array)
                    .ok_or(Fault::Invalid)?
                    .iter()
                    .map(|v| {
                        let text = v.as_str().ok_or(Fault::Invalid)?;
                        let id: Id = text.parse().map_err(|_| Fault::Invalid)?;
                        if id.to_string() != text {
                            return Err(Fault::Invalid);
                        }
                        Ok(id)
                    })
                    .collect::<Result<Vec<_>, Fault>>()?;
                graph.set_members(scope, kind, &ids)?;
            } else {
                let binding = if req.method == Method::POST {
                    let b = graph.create(scope, kind, edit.key)?;
                    created = Some(b.id);
                    b
                } else if let Some(id) = binding_id(&snapshot, r, p) {
                    snapshot.registry.by_id(id).ok_or(Fault::Missing)?
                } else {
                    graph.ensure_section(scope, kind)?
                };
                if !matches!(r.target, Target::Credentials(_)) {
                    graph.replace(binding, edit.fields)?;
                    written = Some(binding);
                }
                private = edit
                    .private
                    .into_iter()
                    .map(|(slot, change)| CredentialEdit {
                        resource: binding.id,
                        slot,
                        change,
                    })
                    .collect();
            }
        }
        let revision = fresh()?;
        let zone = crate::schedule_zone::capture();
        let mut manifest = graph.manifest(revision)?;
        if let Some(binding) = written {
            let candidate = graph.assemble(&manifest)?;
            let projections =
                PreparedConfiguration::prepare(&candidate, zone.as_deref(), &self.inputs)?;
            for warning in &projections.public.warnings {
                self.runtime.diagnostic(*warning);
            }
            graph.replace(binding, projections.public.normalized(binding)?)?;
            manifest = graph.manifest(revision)?;
        }
        let candidate = graph.assemble(&manifest)?;
        let compiled = PreparedConfiguration::prepare(&candidate, zone.as_deref(), &self.inputs)?;
        let mut prepared = self
            .runtime
            .prepare(&compiled, manifest.revision().as_str())?;
        let source = transaction.inventory()?;
        let (receipt, store_warnings) = transaction.commit_prepared(
            Publication {
                admission: Admission::Save {
                    state: selected.digest,
                },
                source: &source,
                manifest: &manifest,
                objects: &graph.changed(),
                journal_id: fresh()?,
                edits: &private,
            },
            |credentials| {
                self.runtime
                    .prepare_credentials(&mut prepared, credentials)
                    .map_err(|_| TransactionError::Conflict)
            },
        )?;
        if store_warnings > 0 {
            self.runtime
                .diagnostic(Diagnostic::PrivateSettings(store_warnings));
        }
        self.complete_publication(&transaction, &receipt.desired, compiled)?;
        drop(guard);
        #[cfg(test)]
        self.runtime.after_publication();
        let result = if let Some(id) = created {
            json!({"created":id.to_string()})
        } else {
            json!({})
        };
        Ok((
            if created.is_some() {
                StatusCode::CREATED
            } else {
                StatusCode::ACCEPTED
            },
            result,
            prepared,
            revision.to_string(),
        ))
    }

    fn complete_publication(
        &self,
        tx: &Transaction<'_, '_>,
        expected: &ManifestRef,
        prepared: PreparedConfiguration,
    ) -> Result<(), Fault> {
        let revision = expected.revision.as_str();
        // The selector is already durable. A failed publication can no longer be
        // reported as an ordinary pre-commit conflict or a rejected recovery.
        tx.reconcile().map_err(|_| {
            self.runtime
                .inhibit_uncertain(Some(revision), Fault::DurabilityUnknown);
            Fault::DurabilityUnknown
        })?;
        self.admit_selected(tx, expected, false, Some(prepared), None)
            .map_err(|fault| {
                if fault == Fault::Conflict {
                    self.runtime
                        .inhibit_uncertain(Some(revision), Fault::DurabilityUnknown);
                }
                Fault::DurabilityUnknown
            })
    }

    fn adopt(&self, revision: &str, prepared: R::Prepared) -> Activation {
        let result = match self.runtime.adopt(prepared).0 {
            Confirmation::Observed(result) => result,
            Confirmation::Pending(completion) => loop {
                match completion.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(result) => break result,
                    Err(mpsc::RecvTimeoutError::Timeout)
                        if !self.stopping.load(std::sync::atomic::Ordering::Acquire) => {}
                    Err(_) => break AdoptionOutcome::RecoveryRequired,
                }
            },
        };
        match result {
            // A confirmed rollback has already stopped the failed candidate and
            // restored every touched incumbent. Preserve unrelated running stations.
            AdoptionOutcome::Confirmed | AdoptionOutcome::RolledBack => {}
            AdoptionOutcome::RecoveryRequired => self
                .runtime
                .inhibit_uncertain(Some(revision), Fault::DurabilityUnknown),
        }
        result.state()
    }
    fn finish_adoption(&self, revision: &str, observed: Activation) -> Result<(), Fault> {
        let result = (|| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let guard = loop {
                match self.store.files.try_edit() {
                    Err(CaptureError::Busy)
                        if std::time::Instant::now() < until
                            && !self.stopping.load(std::sync::atomic::Ordering::Acquire) =>
                    {
                        thread::sleep(std::time::Duration::from_millis(10))
                    }
                    result => break result.map_err(lock_error)?,
                }
            };
            self.admit_writer(&guard, None)?;
            let tx = Transaction::new(&guard, self.store.limits).with_write_key(&self.key);
            let selected = tx.inspect()?;
            if selected.state.desired.revision.as_str() != revision {
                return Err(Fault::DurabilityUnknown);
            }
            tx.record_activation(&selected, observed)
                .map_err(|_| Fault::DurabilityUnknown)?;
            self.publish_snapshot(&tx, false)
                .map(|_| ())
                .map_err(|_| Fault::DurabilityUnknown)
        })();
        if result.is_err() {
            self.store.files.mark_bookkeeping_fault();
            self.runtime
                .inhibit_uncertain(Some(revision), Fault::DurabilityUnknown);
        }
        result
    }

    fn process(&self, req: AdmittedRequest) {
        let path = req.target.path();
        let Ok((r, p)) = self.api.lookup(path) else {
            req.respond(error_response(&self.api, "/", "api", Fault::Missing, None));
            return;
        };
        let relative = &path[self.api.mount.len()..];
        if r.target == Target::Operation {
            let result = self.operation(&req, p);
            let reply = match result {
                Ok((status, body)) => response(status, body, Some(r), "application/hal+json"),
                Err(e) => error_response(&self.api, relative, &r.profile, e, Some(r)),
            };
            req.respond(reply);
            return;
        }
        let result = self.mutate(&req, r, p, relative);
        match result {
            Err(e) => {
                let _ = self.refresh(false);
                if matches!(e, Fault::Storage | Fault::DurabilityUnknown) {
                    self.runtime.inhibit_uncertain(None, e);
                }
                let response = error_response(&self.api, relative, &r.profile, e, Some(r));
                req.respond(response);
            }
            Ok((status, mut body, prepared, revision)) => {
                body["desired_revision"] = json!(revision);
                let created = body.get("created").and_then(Value::as_str);
                let location = created.map(|id| format!("{}{relative}{id}/", self.api.mount));
                let surviving = if let Some(path) = &location {
                    &path[self.api.mount.len()..]
                } else if req.method == Method::DELETE
                    && matches!(r.target, Target::Member(_) | Target::Metadata)
                {
                    &relative[..relative
                        .trim_end_matches('/')
                        .rfind('/')
                        .expect("member parent")
                        + 1]
                } else {
                    relative
                };
                let path = format!("{}{surviving}", self.api.mount);
                let (subject, params) = self.api.lookup(&path).expect("receipt route");
                let mut links = self.api.links(surviving, &subject.profile);
                links["up"] = self.api.parent_link(subject, params);
                links["rpt:changes"] = json!({"href":format!("{}/changes/",self.api.mount)});
                body["_links"] = links;
                let mut reply = response(status, body, Some(r), "application/hal+json");
                if let Some(location) = location {
                    reply
                        .headers_mut()
                        .insert("location", location.parse().expect("canonical location"));
                }
                req.respond(reply);
                let outcome = self.adopt(&revision, prepared);
                let _ = self.finish_adoption(&revision, outcome);
            }
        }
    }
}

fn lock_error(error: CaptureError) -> Fault {
    if error == CaptureError::Busy {
        Fault::Conflict
    } else {
        Fault::Storage
    }
}

enum WriteRequest {
    Http(Box<AdmittedRequest>),
    HostReload(SyncSender<Result<(), Fault>>),
}

/// Weak sender held by the native service frontend. The store worker drains on
/// coordinator drop even if a frontend still owns its last receipt.
pub(crate) struct HostReload {
    writer: std::sync::Weak<SyncSender<WriteRequest>>,
    busy: Arc<std::sync::atomic::AtomicBool>,
    receipt: Option<mpsc::Receiver<Result<(), Fault>>>,
}

impl HostReload {
    pub(crate) fn begin(&mut self) -> Result<(), Fault> {
        if self.receipt.is_some() {
            return Err(Fault::Conflict);
        }
        let writer = self.writer.upgrade().ok_or(Fault::Unavailable)?;
        self.busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| Fault::Conflict)?;
        let (send, receive) = mpsc::sync_channel(1);
        if writer.try_send(WriteRequest::HostReload(send)).is_err() {
            self.busy.store(false, std::sync::atomic::Ordering::Release);
            return Err(Fault::Unavailable);
        }
        self.receipt = Some(receive);
        Ok(())
    }

    pub(crate) fn poll(&mut self) -> Result<bool, Fault> {
        let Some(receipt) = self.receipt.as_ref() else {
            return Ok(true);
        };
        let result = match receipt.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return Ok(false),
            Err(mpsc::TryRecvError::Disconnected) => Err(Fault::DurabilityUnknown),
        };
        self.receipt = None;
        result.map(|()| true)
    }
}

/// Bounded authenticated request consumer. Call from a management thread.
pub struct Coordinator<R> {
    backend: Arc<Backend<R>>,
    boot: Id,
    busy: Arc<std::sync::atomic::AtomicBool>,
    api: Arc<Api>,
    runtime: Arc<R>,
    cached: Arc<RwLock<Option<Arc<Snapshot>>>>,
    send: Option<Arc<SyncSender<WriteRequest>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl<R: Runtime> Coordinator<R> {
    /// Open a public management snapshot on the control plane and start one serialized write worker.
    pub fn new(
        store: Arc<ManagedStore>,
        key: KeyId,
        mount: &str,
        runtime: Arc<R>,
    ) -> Result<Self, Fault> {
        Self::with_profile_inputs(store, key, mount, runtime, ProfileInputs::installed())
    }
    /// Start the coordinator with the same immutable preparation inputs supplied to the runtime host.
    pub fn with_profile_inputs(
        store: Arc<ManagedStore>,
        key: KeyId,
        mount: &str,
        runtime: Arc<R>,
        inputs: Arc<ProfileInputs>,
    ) -> Result<Self, Fault> {
        Self::start(store, key, mount, runtime, inputs, false)
    }

    pub(crate) fn for_service(
        store: Arc<ManagedStore>,
        key: KeyId,
        runtime: Arc<R>,
        inputs: Arc<ProfileInputs>,
    ) -> Result<Self, Fault> {
        Self::start(store, key, "/api/v1", runtime, inputs, true)
    }

    fn start(
        store: Arc<ManagedStore>,
        key: KeyId,
        mount: &str,
        runtime: Arc<R>,
        inputs: Arc<ProfileInputs>,
        cold_start: bool,
    ) -> Result<Self, Fault> {
        let api = Arc::new(Api::new(mount)?);
        let cached = Arc::new(RwLock::new(None));
        let boot = fresh()?;
        let initial = store
            .files
            .try_edit()
            .ok()
            .and_then(|g| operations::Ledger::read(&g, boot).ok())
            .map(Arc::new);
        let backend = Arc::new(Backend {
            stopping: std::sync::atomic::AtomicBool::new(false),
            boot,
            ledger: RwLock::new(initial),
            maintenance: Default::default(),
            store,
            key,
            inputs,
            api: Arc::clone(&api),
            runtime: Arc::clone(&runtime),
            cached: Arc::clone(&cached),
        });
        if backend.refresh(true).is_err() && !cold_start {
            // This constructor has no first cold-start attempt left to resolve the lease.
            backend
                .store
                .writer
                .write()
                .expect("store writer lifetime")
                .take_if(|held| held.provisional);
        }
        let busy = Arc::new(std::sync::atomic::AtomicBool::new(cold_start));
        let pending = busy.clone();
        let (send, recv) = mpsc::sync_channel(1);
        let writer = Arc::clone(&backend);
        let worker = thread::Builder::new()
            .name("rpt-management-store".into())
            .spawn(move || {
                if cold_start {
                    if let Err(fault) = writer.cold_start() {
                        writer.runtime.inhibit_uncertain(None, fault);
                    }
                    pending.store(false, std::sync::atomic::Ordering::Release);
                }
                while let Ok(req) = recv.recv() {
                    match req {
                        WriteRequest::Http(req) => writer.process(*req),
                        WriteRequest::HostReload(reply) => {
                            let _ = reply.send(writer.service_reload());
                        }
                    }
                    pending.store(false, std::sync::atomic::Ordering::Release)
                }
            })
            .map_err(|_| Fault::Unavailable)?;
        Ok(Self {
            backend,
            boot,
            busy,
            api,
            runtime,
            cached,
            send: Some(Arc::new(send)),
            worker: Some(worker),
        })
    }

    pub(crate) fn host_reload(&self) -> HostReload {
        HostReload {
            writer: Arc::downgrade(self.send.as_ref().expect("running coordinator")),
            busy: Arc::clone(&self.busy),
            receipt: None,
        }
    }

    /// Drain coalesced listener health events and then claim at most one request.
    pub fn poll_listener(
        &self,
        listener: &rptadv_peer_support::https::Listener,
        inbox: &mut CoordinatorInbox,
    ) -> bool {
        for event in listener.take_diagnostics().events() {
            self.runtime.diagnostic(Diagnostic::Transport(event));
        }
        self.poll(inbox)
    }

    /// Claim at most one authenticated request. GET uses only the published read snapshot.
    pub fn poll(&self, inbox: &mut CoordinatorInbox) -> bool {
        let Some(req) = inbox.try_recv() else {
            return false;
        };
        self.serve(req);
        true
    }

    fn serve(&self, req: AdmittedRequest) {
        let path = req.target.path();
        let (route, params) = match self.api.lookup(path) {
            Ok(v) => v,
            Err(e) => {
                req.respond(error_response(&self.api, "/", "api", e, None));
                return;
            }
        };
        let rel = &path[self.api.mount.len()..];
        let media = openapi::media(route);
        let send_error = |fault| error_response(&self.api, rel, &route.profile, fault, Some(route));
        let result = (|| {
            if !route.methods.contains(&req.method.as_str()) {
                return Err(Fault::Method);
            }
            let page = pagination(req.target.query(), route, &req.method)?;
            if !accepts(&req.headers, media) {
                return Err(Fault::Accept);
            }
            if matches!(route.target, Target::Maintenance | Target::MaintenanceLease) {
                return self.backend.lease_request(&req, route, params).map(Some);
            }
            if req.method != Method::GET {
                return Ok(None);
            }
            // Storage observations are made on the separate management thread. During a
            // pending write/adoption, serve the last completed inspection immediately.
            if (matches!(
                route.target,
                Target::Health
                    | Target::Changes
                    | Target::Desired
                    | Target::Storage
                    | Target::LastKnownGood
            ) || (route.view == Some(rpt_advanced_core::management::routes::View::Status)))
                && !self.busy.load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = self.backend.refresh(false);
            }
            let cache = self.cached.read().expect("management snapshot").clone();
            let operation = matches!(route.target, Target::Operations | Target::Operation);
            let body = if operation {
                let ledger = self.backend.operation_snapshot()?;
                if route.target == Target::Operations {
                    ledger.index(&self.api, self.boot)
                } else {
                    ledger.document(
                        &self.api,
                        params.boot.ok_or(Fault::Missing)?,
                        params.request.ok_or(Fault::Missing)?,
                    )?
                }
            } else {
                self.api.read(
                    route,
                    params,
                    rel,
                    cache.as_deref(),
                    &self.runtime.observed(),
                    page,
                )?
            };
            let target = req.target.as_str();
            let tag = validator(
                target,
                if operation {
                    ""
                } else {
                    cache.as_ref().map_or("", |s| s.revision.as_str())
                },
                &body,
            );
            let mut reply = response(StatusCode::OK, body, Some(route), media);
            reply
                .headers_mut()
                .insert("etag", tag.parse().expect("digest validator"));
            Ok(Some(reply))
        })();
        match result {
            Err(e) => {
                let reply = send_error(e);
                req.respond(reply);
            }
            Ok(Some(reply)) => req.respond(reply),
            Ok(None) => {
                let busy = send_error(Fault::Conflict);
                if self
                    .busy
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .is_err()
                {
                    req.respond(busy);
                    return;
                }
                if let Err(
                    TrySendError::Full(WriteRequest::Http(req))
                    | TrySendError::Disconnected(WriteRequest::Http(req)),
                ) = self
                    .send
                    .as_ref()
                    .expect("running coordinator")
                    .try_send(WriteRequest::Http(Box::new(req)))
                {
                    self.busy.store(false, std::sync::atomic::Ordering::Release);
                    req.respond(busy);
                }
            }
        }
    }
}
impl<R> Drop for Coordinator<R> {
    fn drop(&mut self) {
        self.backend
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
        self.send.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn response(
    status: StatusCode,
    body: Value,
    r: Option<&Route>,
    media: &'static str,
) -> Response<Vec<u8>> {
    let mut reply = Response::new(serde_json::to_vec(&body).expect("public JSON"));
    *reply.status_mut() = status;
    let headers = reply.headers_mut();
    headers.insert("content-type", media.parse().expect("known media"));
    headers.insert("cache-control", "no-store".parse().expect("cache policy"));
    if let Some(r) = r {
        headers.insert(
            "allow",
            r.methods.join(", ").parse().expect("known methods"),
        );
        if r.methods.contains(&"PATCH") {
            headers.insert(
                "accept-patch",
                "application/json-patch+json".parse().expect("patch media"),
            );
        }
    }
    reply
}
fn error_response(
    api: &Api,
    relative: &str,
    profile: &str,
    error: Fault,
    r: Option<&Route>,
) -> Response<Vec<u8>> {
    let (status, code) = error.wire();
    response(
        status,
        serde_json::to_value(openapi::ManagementError {
            error: code,
            links: api.links(relative, profile),
        })
        .expect("serializable error"),
        r,
        "application/hal+json",
    )
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").expect("string write");
    }
    out
}

fn validator(target: &str, revision: &str, body: &Value) -> String {
    // Length-prefix arbitrary path, revision and representation for unambiguous digest input.
    let bytes = serde_json::to_vec(&(target, revision, body)).expect("public validator input");
    let mut tag = String::with_capacity(66);
    tag.push('"');
    for byte in artifact_sha256(&bytes) {
        write!(tag, "{byte:02x}").expect("string write");
    }
    tag.push('"');
    tag
}
fn require(headers: &HeaderMap, tag: &str) -> Result<(), Fault> {
    let values = headers.get_all("if-match");
    if values.iter().next().is_none() {
        return Err(Fault::PreconditionRequired);
    }
    if values
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim() == tag)
    {
        Ok(())
    } else {
        Err(Fault::PreconditionFailed)
    }
}
fn accepts(headers: &HeaderMap, media: &str) -> bool {
    let values = headers.get_all("accept");
    values.iter().next().is_none()
        || values
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| {
                matches!(v.trim(), "*/*" | "application/*") || v.trim().eq_ignore_ascii_case(media)
            })
}
fn pagination(
    query: Option<&str>,
    r: &Route,
    method: &Method,
) -> Result<Option<(usize, usize)>, Fault> {
    let Some(query) = query else { return Ok(None) };
    if method != Method::GET
        || !matches!(
            r.target,
            Target::Collection(_) | Target::Peers | Target::Calls
        )
    {
        return Err(Fault::Malformed);
    }
    let mut offset = None;
    let mut limit = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        let val = v.parse::<usize>().map_err(|_| Fault::Malformed)?;
        match k.as_ref() {
            "offset" if offset.is_none() => offset = Some(val),
            "limit" if limit.is_none() && (1..=100).contains(&val) => limit = Some(val),
            _ => return Err(Fault::Malformed),
        }
    }
    Ok(Some((offset.unwrap_or(0), limit.unwrap_or(20))))
}
