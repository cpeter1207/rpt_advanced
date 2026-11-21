//! Explicit recovery prepares the accepted or imported public generation before touching state.
use super::*;
use crate::configuration_transaction::import::import_working;
use rpt_advanced_core::config::resources::{
    manifest::{ContentHash, PublicManifest},
    state::ManifestRef,
};
use rptadv_peer_support::working_files::WorkingSnapshot;

pub(super) struct Recovery<P> {
    expected: ContentHash,
    target: String,
    work: Option<Save>,
    prepared: P,
}
struct Save {
    source: WorkingSnapshot,
    admission: Admission,
    manifest: PublicManifest,
    objects: Vec<(Id, Vec<u8>)>,
}

impl<R: Runtime> Backend<R> {
    pub(super) fn prepare_recovery(
        &self,
        tx: &Transaction<'_, '_>,
        selected: &Selected,
        body: &Value,
    ) -> Result<Recovery<R::Prepared>, Fault> {
        let snapshot = self.snapshot(selected)?;
        let relative = "/status/desired/";
        let full = format!("{}{relative}", self.api.mount);
        let (route, params) = self.api.lookup(&full)?;
        let doc = self.api.read(
            route,
            params,
            relative,
            Some(&snapshot),
            &self.runtime.observed(),
            None,
        )?;
        if body["desired_etag"].as_str() != Some(&validator(&full, &snapshot.revision, &doc)) {
            return Err(Fault::PreconditionFailed);
        }
        if selected.status == StartupStatus::StorageFault
            || selected.bookkeeping_fault && body["action"]["action"] != "apply_configuration"
        {
            return Err(Fault::Storage);
        }
        if selected.status == StartupStatus::SecretsUnavailable {
            return Err(Fault::Unavailable);
        }
        let name = body["action"]["action"].as_str().ok_or(Fault::Invalid)?;
        if name == "apply_configuration" {
            for warning in &snapshot.views.result.as_ref().map_err(|e| *e)?.warnings {
                self.runtime.diagnostic(*warning);
            }
            if selected.status == StartupStatus::ExternalEdit || selected.state.journal.is_some() {
                return Err(Fault::Conflict);
            }
            let candidate = selected
                .desired
                .assemble_candidate()
                .map_err(|_| Fault::Invalid)?;
            let prepared = self.runtime.prepare(
                &candidate,
                selected.desired.manifest().registry(),
                &snapshot.revision,
            )?;
            return Ok(Recovery {
                expected: selected.digest,
                target: snapshot.revision,
                work: None,
                prepared,
            });
        }
        let source = tx.inventory()?;
        let working = ContentHash::from_bytes(*source.fingerprint().as_bytes());
        if body["working_fingerprint"]
            .as_str()
            .and_then(|s| s.parse::<ContentHash>().ok())
            != Some(working)
        {
            return Err(Fault::PreconditionFailed);
        }
        let target = fresh()?;
        let (manifest, objects, admission, prepared) = if name == "restore_configuration" {
            let (loaded, retained) = match body["restore"].as_str() {
                Some("desired") => (&selected.desired, &selected.state.desired),
                Some("last_known_good") => {
                    let r = selected
                        .state
                        .last_known_good
                        .as_ref()
                        .ok_or(Fault::Conflict)?;
                    (
                        if r == &selected.state.desired {
                            &selected.desired
                        } else {
                            selected.last_known_good.as_ref().ok_or(Fault::Storage)?
                        },
                        r,
                    )
                }
                _ => return Err(Fault::Invalid),
            };
            let candidate = loaded.assemble_candidate().map_err(|_| Fault::Invalid)?;
            Projections::prepare(
                &candidate,
                loaded.manifest().registry(),
                self.zone.as_deref(),
                &self.inputs,
            )?;
            let prepared = self.runtime.prepare(
                &candidate,
                loaded.manifest().registry(),
                &target.to_string(),
            )?;
            let m = retained_manifest(loaded.manifest(), target)?;
            (
                m,
                Vec::new(),
                Admission::Restore {
                    state: selected.digest,
                    working,
                    retained: ManifestRef::clone(retained),
                },
                prepared,
            )
        } else {
            let draft = import_working(&source, selected.desired.manifest().registry())?;
            let mut graph = Graph::import(&draft, &selected.desired)?;
            let initial = graph.manifest(target)?;
            let candidate = graph.assemble(&initial)?;
            let validated = Projections::prepare(
                &candidate,
                initial.registry(),
                self.zone.as_deref(),
                &self.inputs,
            )?;
            for warning in &validated.warnings {
                self.runtime.diagnostic(*warning);
            }
            for binding in initial
                .registry()
                .bindings()
                .filter(|b| draft.envelope(b.id).is_some())
            {
                graph.replace(binding, validated.normalized(binding)?)?;
            }
            let manifest = graph.manifest(target)?;
            let candidate = graph.assemble(&manifest)?;
            Projections::prepare(
                &candidate,
                manifest.registry(),
                self.zone.as_deref(),
                &self.inputs,
            )?;
            let prepared = self.runtime.prepare(
                &candidate,
                manifest.registry(),
                manifest.revision().as_str(),
            )?;
            let objects = graph
                .changed()
                .into_iter()
                .map(|(id, bytes)| (id, bytes.to_vec()))
                .collect();
            (
                manifest,
                objects,
                Admission::Reload {
                    state: selected.digest,
                    working,
                },
                prepared,
            )
        };
        Ok(Recovery {
            expected: selected.digest,
            target: target.to_string(),
            work: Some(Save {
                source,
                admission,
                manifest,
                objects,
            }),
            prepared,
        })
    }

    pub(super) fn execute_recovery(&self, mut r: Recovery<R::Prepared>) -> Result<(), Fault> {
        {
            let guard = self.store.try_edit().map_err(lock_error)?;
            let tx = Transaction::new(&guard, self.limits).with_write_key(&self.key);
            #[cfg(test)]
            let checkpoint = |point| {
                if point == crate::configuration_transaction::Boundary::DesiredSync
                    && !self.runtime.recovery_desired_rename_observed()
                {
                    Err(TransactionError::DurabilityUnknown)
                } else {
                    Ok(())
                }
            };
            #[cfg(test)]
            let tx = tx.with_checkpoint(&checkpoint);
            let selected = tx.inspect()?;
            if selected.digest != r.expected {
                return Err(Fault::PreconditionFailed);
            }
            if let Some(save) = r.work {
                let refs = save
                    .objects
                    .iter()
                    .map(|(id, v)| (*id, v.as_slice()))
                    .collect::<Vec<_>>();
                tx.commit_prepared(
                    Publication {
                        admission: save.admission,
                        source: &save.source,
                        manifest: &save.manifest,
                        objects: &refs,
                        journal_id: fresh()?,
                        edits: &[],
                    },
                    |credentials| {
                        self.runtime
                            .prepare_credentials(&mut r.prepared, credentials)
                            .map_err(|_| TransactionError::Conflict)
                    },
                )
                .map_err(|error| {
                    if error == TransactionError::DurabilityUnknown {
                        self.runtime
                            .inhibit_uncertain(Some(&r.target), Fault::DurabilityUnknown);
                    }
                    Fault::from(error)
                })?;
                self.complete_publication(&tx, &r.target)?;
            } else {
                if selected.status == StartupStatus::ExternalEdit
                    || selected.state.journal.is_some()
                {
                    return Err(Fault::Conflict);
                }
                self.runtime.prepare_credentials(
                    &mut r.prepared,
                    selected
                        .private
                        .credentials()
                        .map_err(|_| Fault::Unavailable)?,
                )?;
                // Only an explicit apply can repair an already-recorded bookkeeping fault.
                // Final private, working-tree and storage verification still applies.
                self.admit_selected(&tx, &r.target, selected.bookkeeping_fault)?;
            }
        }
        #[cfg(test)]
        self.runtime.after_publication();
        let outcome = self.adopt(&r.target, r.prepared);
        self.finish_adoption(&r.target, outcome)
            .map_err(|_| Fault::DurabilityUnknown)?;
        match outcome {
            Activation::Confirmed => Ok(()),
            Activation::Failed => Err(Fault::Conflict),
            _ => Err(Fault::DurabilityUnknown),
        }
    }
}

fn retained_manifest(source: &PublicManifest, target: Id) -> Result<PublicManifest, Fault> {
    let mut wire: Value =
        rptadv_json::decode_value(&source.encode()).map_err(|_| Fault::Storage)?;
    wire["id"] = json!(target.to_string());
    wire["revision"] = json!(target.to_string());
    Ok(
        PublicManifest::decode(&serde_json::to_vec(&wire).map_err(|_| Fault::Storage)?)
            .map_err(|_| Fault::Storage)?
            .value,
    )
}
