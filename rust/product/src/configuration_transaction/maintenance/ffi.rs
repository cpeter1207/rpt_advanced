//! Optional synchronous dynamic boundary; the executable carries no store algorithms.
use super::*;
use crate::abi;
use rptadv_peer_support::working_files::WorkingFiles;
use serde_json::{Value, json};
use std::{
    ffi::{c_char, c_void},
    path::Path,
};

const LIMITS: CaptureLimits = CaptureLimits {
    max_paths: 8192,
    max_path_bytes: 512,
    max_file_bytes: 8 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
};
static DESCRIPTOR: abi::rptadv_store_maintenance_v1 = abi::rptadv_store_maintenance_v1 {
    struct_size: std::mem::size_of::<abi::rptadv_store_maintenance_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.store1\0\0\0",
    run: Some(run),
};
/// Return the independent optional local protected-store maintenance boundary.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_product_store_maintenance_descriptor_v1()
-> *const abi::rptadv_store_maintenance_v1 {
    &DESCRIPTOR
}
fn error_name(error: MaintenanceError) -> &'static str {
    match error {
        MaintenanceError::InvalidGraph => "invalid_graph",
        MaintenanceError::Conflict => "conflict",
        MaintenanceError::SecretsUnavailable => "secrets_unavailable",
        MaintenanceError::ExternalEdit => "external_edit",
        MaintenanceError::DurabilityUnknown => "durability_unknown",
        MaintenanceError::Limits => "limits",
    }
}
unsafe extern "C" fn run(
    operation: u32,
    request: *const c_char,
    length: usize,
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        if request.is_null() || length == 0 || length > 64 * 1024 {
            return Err(MaintenanceError::Limits);
        }
        // The ABI requires the caller's readable span to remain alive for this call.
        let bytes = unsafe { std::slice::from_raw_parts(request.cast::<u8>(), length) };
        let value = rptadv_json::decode_value(bytes).map_err(|_| MaintenanceError::InvalidGraph)?;
        execute(
            operation,
            &value,
            #[cfg(test)]
            None,
        )
    })
    .unwrap_or(Err(MaintenanceError::InvalidGraph));
    let (status, value) = match result {
        Ok(value) => (0, value),
        Err(error) => (-1, json!({"status":"failed","error":error_name(error)})),
    };
    if let Some(sink) = sink
        && let Ok(bytes) = serde_json::to_vec(&value)
    {
        // The sink obeys the same borrowed synchronous/no-unwind contract.
        unsafe { sink(context, bytes.as_ptr().cast::<c_char>(), bytes.len()) };
    }
    status
}
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, MaintenanceError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or(MaintenanceError::InvalidGraph)
}
fn expected(value: &Value) -> Result<Option<ContentHash>, MaintenanceError> {
    let field = value
        .get("expected_state_sha256")
        .ok_or(MaintenanceError::Conflict)?;
    if field.is_null() {
        Ok(None)
    } else {
        field
            .as_str()
            .and_then(|s| s.parse().ok())
            .map(Some)
            .ok_or(MaintenanceError::InvalidGraph)
    }
}
pub(super) fn summary(backup: &Backup) -> Value {
    let digest: String = backup
        .digest()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    json!({"status":"complete","state_sha256":digest,"artifacts":backup.artifacts().count(),
        "required_key_ids":backup.required_keys().iter().map(KeyId::as_str).collect::<Vec<_>>()})
}
fn snapshot(path: &Path, store: ResourceId) -> Result<Backup, MaintenanceError> {
    let owner = WorkingFiles::open(path).map_err(|_| MaintenanceError::InvalidGraph)?;
    let guard = owner.try_edit().map_err(|_| MaintenanceError::Conflict)?;
    Maintenance::new(&guard, store, LIMITS).capture()
}
/// Probe under the writer lease before initializing an unselected destination.
fn destination(
    path: &Path,
    store: ResourceId,
    preflight: impl FnOnce(Option<EditGuard<'_>>) -> Result<(), MaintenanceError>,
) -> Result<(WorkingFiles, Option<WriterLease>), MaintenanceError> {
    let owner = WorkingFiles::open(path).map_err(|_| MaintenanceError::InvalidGraph)?;
    let lease = owner
        .try_writer_lease()
        .map_err(|_| MaintenanceError::Conflict)?;
    let guard = match owner.try_edit() {
        Ok(guard) => Some(guard),
        Err(CaptureError::Missing) => None,
        Err(_) => return Err(MaintenanceError::Conflict),
    };
    let established = match &guard {
        Some(guard) => guard
            .read_state(LIMITS.max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .is_some(),
        None => false,
    };
    preflight(guard)?;
    if established {
        Ok((owner, Some(lease)))
    } else {
        drop(lease);
        drop(owner);
        Ok((
            WorkingFiles::provision(path, store.uuid()).map_err(|_| MaintenanceError::Conflict)?,
            None,
        ))
    }
}
fn seed_manifest(
    site: ResourceId,
    revision: &str,
) -> Result<(PublicManifest, Vec<u8>), MaintenanceError> {
    use rpt_advanced_core::config::resources::{
        candidate::{ResourceBinding, ResourceKind, ResourceScope},
        manifest::ObjectReference,
    };

    let envelope = json!({"id": site.to_string(), "kind": "metadata",
        "configuration": {"name": "Repeater", "inherit_from": null}})
    .to_string()
    .into_bytes();
    let binding = ResourceBinding {
        id: site,
        scope: ResourceScope::Site(site),
        kind: ResourceKind::Metadata,
    };
    let object = ObjectReference::new(
        revision
            .parse()
            .map_err(|_| MaintenanceError::InvalidGraph)?,
        hash(&envelope),
    );
    let manifest = PublicManifest::new(
        revision
            .parse()
            .map_err(|_| MaintenanceError::InvalidGraph)?,
        revision
            .parse()
            .map_err(|_| MaintenanceError::InvalidGraph)?,
        site,
        &[],
        [(binding, Some(object))],
    )
    .map_err(|_| MaintenanceError::InvalidGraph)?;
    Ok((manifest, envelope))
}

/// Recognize an interrupted first publication before replaying its working changes.
fn initial_publication(selected: &Selected) -> Result<&ManifestRef, MaintenanceError> {
    let state = &selected.state;
    let journal = selected
        .journal
        .as_ref()
        .ok_or(MaintenanceError::Conflict)?;
    let [change] = journal.changes.as_slice() else {
        return Err(MaintenanceError::Conflict);
    };
    if journal.prior.is_some()
        || journal.source_working != fingerprint(&BTreeMap::new())
        || !journal.prune_directories.is_empty()
        || change.source.is_some()
        || change.target.is_none()
        || state.activation != Activation::Pending
        || state.last_known_good.is_some()
        || !state.retained_recovery.is_empty()
        || state.clean_working.is_some()
    {
        return Err(MaintenanceError::Conflict);
    }
    let graph = &selected.desired;
    let revision: ResourceId = state
        .desired
        .revision
        .as_str()
        .parse()
        .map_err(|_| MaintenanceError::Conflict)?;
    let (seed, _) = seed_manifest(graph.manifest().registry().site_id(), &revision.to_string())?;
    if seed.encode().as_slice() != graph.manifest_bytes() {
        return Err(MaintenanceError::Conflict);
    }
    Ok(&state.desired)
}

fn initialize(
    root: &Path,
    store: ResourceId,
    key: &KeyId,
    #[cfg(test)] checkpoint: Option<&dyn Fn(Boundary) -> Result<(), TransactionError>>,
) -> Result<Value, MaintenanceError> {
    // A missing delivered key must leave even the directory layout untouched.
    load_systemd_key(key).map_err(|_| MaintenanceError::SecretsUnavailable)?;
    let (owner, lease) = destination(root, store, |_| Ok(()))?;
    let owner = owner.with_identity(store.uuid());
    let proof = lease
        .as_ref()
        .or_else(|| owner.provisioning_writer_lease())
        .ok_or(MaintenanceError::Conflict)?;
    let guard = owner.try_edit().map_err(|_| MaintenanceError::Conflict)?;
    guard
        .check_writer_lease(proof)
        .map_err(|_| MaintenanceError::Conflict)?;
    let tx = Transaction::new(&guard, LIMITS).with_write_key(key);
    #[cfg(test)]
    let tx = if let Some(checkpoint) = checkpoint {
        tx.with_checkpoint(checkpoint)
    } else {
        tx
    };
    let desired = if guard
        .read_state(LIMITS.max_file_bytes)
        .map_err(|_| MaintenanceError::InvalidGraph)?
        .is_some()
    {
        initial_publication(&tx.inspect()?)?.clone()
    } else {
        let id = || -> Result<ResourceId, MaintenanceError> {
            new_identity()
                .map_err(|_| MaintenanceError::InvalidGraph)?
                .to_string()
                .parse()
                .map_err(|_| MaintenanceError::InvalidGraph)
        };
        let site = id()?;
        let (manifest, object) = seed_manifest(site, &id()?.to_string())?;
        tx.commit(
            Admission::Provision { store },
            &tx.inventory()?,
            &manifest,
            &[(site, &object)],
            id()?,
        )?
        .desired
    };
    tx.reconcile()?;
    let selected = tx.inspect()?;
    if selected.state.desired != desired
        || selected.state.journal.is_some()
        || selected.state.activation != Activation::Pending
        || selected.status != StartupStatus::Interrupted
        || selected.state.clean_working.is_none()
        || selected.state.clean_working != selected.observed_working
    {
        return Err(MaintenanceError::Conflict);
    }
    Ok(json!({"status":"complete","revision":desired.revision.as_str(),"activation":"pending"}))
}

pub(crate) fn execute(
    operation: u32,
    value: &Value,
    #[cfg(test)] checkpoint: Option<&dyn Fn(Boundary) -> Result<(), TransactionError>>,
) -> Result<Value, MaintenanceError> {
    let fields = value.as_object().ok_or(MaintenanceError::InvalidGraph)?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "store_root"
                | "store_uuid"
                | "other_root"
                | "expected_state_sha256"
                | "working_sha256"
                | "write_key_id"
                | "new_key_id"
                | "retained_recovery"
        )
    }) {
        return Err(MaintenanceError::InvalidGraph);
    }
    let root = Path::new(text(value, "store_root")?);
    let store: ResourceId = text(value, "store_uuid")?
        .parse()
        .map_err(|_| MaintenanceError::InvalidGraph)?;
    match operation {
        7 => {
            let key = KeyId::new(text(value, "write_key_id")?)
                .map_err(|_| MaintenanceError::InvalidGraph)?;
            initialize(
                root,
                store,
                &key,
                #[cfg(test)]
                checkpoint,
            )
        }
        1 => {
            let owner = WorkingFiles::open(root).map_err(|_| MaintenanceError::InvalidGraph)?;
            let guard = owner.try_edit().map_err(|_| MaintenanceError::Conflict)?;
            let tx = Transaction::new(&guard, LIMITS);
            let m = Maintenance::new(&guard, store, LIMITS);
            let reservation = match m.reservation() {
                Ok(Some((r, _))) => {
                    json!({"status":if r.pending.is_some() {"pending"} else {"completed"},
                    "request_sha256":record::hex(hash(&record::encode(&r.request,record::limit(LIMITS))?))})
                }
                Ok(None) => json!({"status":"none"}),
                Err(_) => json!({"status":"invalid"}),
            };
            let mut result = if guard
                .read_state(LIMITS.max_file_bytes)
                .map_err(|_| MaintenanceError::InvalidGraph)?
                .is_some()
            {
                match m.capture() {
                    Ok(backup) => summary(&backup),
                    Err(_) if reservation["status"] != "none" => json!({"status":"unavailable"}),
                    Err(error) => return Err(error),
                }
            } else if guard
                .has_bootstrap_marker(store.uuid())
                .map_err(|_| MaintenanceError::InvalidGraph)?
            {
                json!({"status":"unselected"})
            } else {
                return Err(MaintenanceError::InvalidGraph);
            };
            result["maintenance"] = reservation;
            match tx.inventory() {
                Ok(working) => result["working_sha256"] = json!(record::hex(digest(&working))),
                Err(error) if result["maintenance"]["status"] == "none" => {
                    return Err(MaintenanceError::from(error));
                }
                Err(_) => (),
            }
            Ok(result)
        }
        3 => {
            let backup = snapshot(root, store)?;
            let (owner, lease) =
                destination(Path::new(text(value, "other_root")?), store, |_| Ok(()))?;
            let proof = lease
                .as_ref()
                .or_else(|| owner.provisioning_writer_lease())
                .ok_or(MaintenanceError::Conflict)?;
            let guard = owner.try_edit().map_err(|_| MaintenanceError::Conflict)?;
            backup.write_to(&guard, proof)?;
            Ok(summary(&backup))
        }
        4 => {
            let locator = text(value, "other_root")?.to_owned();
            let key = KeyId::new(text(value, "write_key_id")?)
                .map_err(|_| MaintenanceError::InvalidGraph)?;
            let expected = expected(value)?;
            let working = match text(value, "working_sha256")? {
                "empty" => fingerprint(&BTreeMap::new()),
                text => text.parse().map_err(|_| MaintenanceError::InvalidGraph)?,
            };
            let request = Request::Restore {
                before: expected.map(record::hex),
                observed: record::hex(working),
                key: key.as_str().to_owned(),
                source: Some(locator.clone()),
            };
            let mut source = None;
            let (owner, lease) = destination(root, store, |guard| {
                let needs_source = match guard {
                    Some(guard) => {
                        Maintenance::new(&guard, store, LIMITS).restore_needs_source(&request)?
                    }
                    None => true,
                };
                if needs_source {
                    source = Some(snapshot(Path::new(&locator), store)?);
                }
                Ok(())
            })?;
            let proof = lease
                .as_ref()
                .or_else(|| owner.provisioning_writer_lease())
                .ok_or(MaintenanceError::Conflict)?;
            let guard = owner.try_edit().map_err(|_| MaintenanceError::Conflict)?;
            let maintenance = Maintenance::new(&guard, store, LIMITS);
            #[cfg(test)]
            let maintenance = if let Some(checkpoint) = checkpoint {
                maintenance.with_checkpoint(checkpoint)
            } else {
                maintenance
            };
            if let Some(answer) = maintenance.resume(proof, &request, &mut source)? {
                return Ok(answer);
            }
            let plan = maintenance.restore_with(
                proof,
                source.ok_or(MaintenanceError::Conflict)?,
                expected,
                working,
                &key,
                Some(locator),
                None,
            )?;
            let restored = maintenance.restore(proof, &plan)?;
            Ok(
                json!({"status":"complete","revision":restored.desired.revision.as_str(),"activation":"pending"}),
            )
        }
        2 | 5 | 6 => {
            let owner = WorkingFiles::open(root).map_err(|_| MaintenanceError::InvalidGraph)?;
            let lease = owner
                .try_writer_lease()
                .map_err(|_| MaintenanceError::Conflict)?;
            let guard = owner.try_edit().map_err(|_| MaintenanceError::Conflict)?;
            let maintenance = Maintenance::new(&guard, store, LIMITS);
            #[cfg(test)]
            let maintenance = if let Some(checkpoint) = checkpoint {
                maintenance.with_checkpoint(checkpoint)
            } else {
                maintenance
            };
            let before = expected(value)?.ok_or(MaintenanceError::Conflict)?;
            let key =
                |name| KeyId::new(text(value, name)?).map_err(|_| MaintenanceError::InvalidGraph);
            let request = match operation {
                2 => Request::Gc {
                    before: record::hex(before),
                },
                5 => Request::Rotate {
                    before: record::hex(before),
                    old_key: key("write_key_id")?.as_str().into(),
                    new_key: key("new_key_id")?.as_str().into(),
                },
                _ => {
                    let rows = value
                        .get("retained_recovery")
                        .and_then(Value::as_array)
                        .ok_or(MaintenanceError::InvalidGraph)?;
                    if rows.len() > MAX_RETAINED_RECOVERY {
                        return Err(MaintenanceError::Limits);
                    }
                    let refs = rows
                        .iter()
                        .map(|row| {
                            let pin: Pin = serde_json::from_value(row.clone())
                                .map_err(|_| MaintenanceError::InvalidGraph)?;
                            pin.reference()
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Maintenance::pin_request(before, &refs, &key("write_key_id")?)
                }
            };
            if let Some(answer) = maintenance.resume(&lease, &request, &mut None)? {
                return Ok(answer);
            }
            if maintenance.capture()?.digest() != before {
                return Err(MaintenanceError::Conflict);
            }
            match &request {
                Request::Gc { .. } => {
                    let plan = maintenance.plan_gc(&lease)?;
                    let removed = plan.paths().count();
                    maintenance.collect(&lease, &plan)?;
                    Ok(json!({"status":"complete","removed":removed}))
                }
                Request::Rotate { .. } => {
                    let plan = maintenance.prepare_rotation(
                        &lease,
                        &key("write_key_id")?,
                        &key("new_key_id")?,
                    )?;
                    maintenance.rotate(&lease, &plan)?;
                    Ok(summary(&maintenance.capture()?))
                }
                Request::Retain { pins, .. } => {
                    let refs = pins
                        .iter()
                        .map(Pin::reference)
                        .collect::<Result<Vec<_>, _>>()?;
                    maintenance.retain_inner(&lease, before, &refs, &key("write_key_id")?)?;
                    Ok(summary(&maintenance.capture()?))
                }
                Request::Restore { .. } => Err(MaintenanceError::InvalidGraph),
            }
        }
        _ => Err(MaintenanceError::InvalidGraph),
    }
}
