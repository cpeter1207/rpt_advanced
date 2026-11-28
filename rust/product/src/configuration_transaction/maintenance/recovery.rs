//! Reconfirm exact durable intent, derive registered candidates, and attest completion.
use super::*;
use serde_json::{Value, json};

impl Maintenance<'_, '_> {
    pub(super) fn reserve(
        &self,
        lease: &WriterLease,
        record: &Record,
    ) -> Result<Option<ContentHash>, MaintenanceError> {
        self.proof(lease)?;
        let cap = record::limit(self.tx.limits);
        let previous = self.reservation()?;
        if let Some((existing, digest)) = &previous {
            if existing.pending.is_some() {
                if existing != record {
                    return Err(MaintenanceError::Conflict);
                }
                self.confirm_intent(*digest)?;
                return Ok(Some(*digest));
            }
            if existing.request == record.request {
                self.verify_terminal(existing)?;
                self.confirm_intent(*digest)?;
                return Ok(None);
            }
        }
        let bytes = record::encode(record, cap)?;
        self.tx
            .guard
            .replace_maintenance(
                lease,
                previous.as_ref().map(|(_, h)| h.as_bytes()),
                &bytes,
                cap,
                || {
                    #[cfg(test)]
                    self.tx
                        .at(Boundary::IntentRename)
                        .map_err(|_| InstallError::DurabilityUnknown)?;
                    Ok(())
                },
            )
            .map_err(install_error)?;
        let digest = hash(&bytes);
        self.confirm_intent(digest)?;
        Ok(Some(digest))
    }
    pub(super) fn confirm_intent(&self, digest: ContentHash) -> Result<(), MaintenanceError> {
        self.tx
            .guard
            .confirm_maintenance(digest.as_bytes(), record::limit(self.tx.limits), || {
                #[cfg(test)]
                self.tx
                    .at(Boundary::IntentBarrier)
                    .map_err(|_| InstallError::DurabilityUnknown)?;
                Ok(())
            })
            .map_err(install_error)
    }
    fn check_keys(&self, request: &Request) -> Result<(), MaintenanceError> {
        let check = |name: &str| {
            let id = KeyId::new(name).map_err(|_| MaintenanceError::InvalidGraph)?;
            load_systemd_key(&id)
                .map(|_| ())
                .map_err(|_| MaintenanceError::SecretsUnavailable)
        };
        match request {
            Request::Gc { .. } => Ok(()),
            Request::Rotate {
                old_key, new_key, ..
            } => {
                check(old_key)?;
                check(new_key)
            }
            Request::Restore { key, .. } | Request::Retain { key, .. } => check(key),
        }
    }
    pub(super) fn verify_terminal(&self, r: &Record) -> Result<Value, MaintenanceError> {
        self.check_keys(&r.request)?;
        let terminal = hash(r.final_state.as_bytes());
        self.check_state(terminal)?;
        self.tx
            .confirm_selector(terminal, self.store)
            .map_err(MaintenanceError::from)?;
        let selected = self.capture()?;
        let working = self.baseline()?;
        match &r.request {
            Request::Restore { .. } => {
                if selected.state.clean_working != Some(digest(&working))
                    || selected.state.journal.is_some()
                {
                    return Err(MaintenanceError::Conflict);
                }
                match &r.pending {
                    Some(Work::Restore { target_working, .. }) => target_working.check(&working)?,
                    None => r.working.check(&working)?,
                    _ => return Err(MaintenanceError::InvalidGraph),
                }
                Ok(
                    json!({"status":"complete","revision":selected.state.desired.revision.as_str(),"activation":"pending"}),
                )
            }
            Request::Gc { .. } => {
                r.working.check(&working)?;
                let gc = r.gc.as_ref().ok_or(MaintenanceError::InvalidGraph)?;
                let paths = self
                    .tx
                    .guard
                    .discover_artifacts(self.tx.limits)
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                if !paths
                    .iter()
                    .map(String::as_str)
                    .eq(gc.survivors.iter().map(|f| f.path.as_str()))
                {
                    return Err(MaintenanceError::Conflict);
                }
                for f in &gc.survivors {
                    if let Some(expected) = &f.sha256 {
                        let target =
                            ArtifactPath::parse(&f.path).ok_or(MaintenanceError::InvalidGraph)?;
                        let file = self
                            .tx
                            .guard
                            .capture_artifact(target, self.tx.artifact_limits())
                            .map_err(|_| MaintenanceError::Conflict)?;
                        if *expected != record::hex(hash(file.bytes())) {
                            return Err(MaintenanceError::Conflict);
                        }
                    }
                }
                Ok(json!({"status":"complete","removed":gc.removed}))
            }
            _ => {
                r.working.check(&working)?;
                Ok(ffi::summary(&selected))
            }
        }
    }
    pub(super) fn finish(
        &self,
        lease: &WriterLease,
        mut r: Record,
        intent: ContentHash,
    ) -> Result<Value, MaintenanceError> {
        let answer = self.verify_terminal(&r)?;
        if let Some(Work::Restore { target_working, .. }) = &r.pending {
            r.working = target_working.clone();
        }
        r.pending = None;
        r.validate(self.store, self.tx.limits)?;
        let cap = record::limit(self.tx.limits);
        let bytes = record::encode(&r, cap)?;
        self.tx
            .guard
            .replace_maintenance(lease, Some(intent.as_bytes()), &bytes, cap, || {
                #[cfg(test)]
                self.tx
                    .at(Boundary::ReceiptRename)
                    .map_err(|_| InstallError::DurabilityUnknown)?;
                Ok(())
            })
            .map_err(install_error)?;
        self.confirm_intent(hash(&bytes))?;
        Ok(answer)
    }
    pub(super) fn restore_record(&self, p: &RestorePlan) -> Result<Record, MaintenanceError> {
        let journal = PublicationJournal::decode(
            &p.journal.1,
            self.tx.limits.max_paths,
            self.tx.limits.max_path_bytes,
        )
        .map_err(|_| MaintenanceError::InvalidGraph)?;
        let manifest =
            PublicManifest::decode(&p.manifest.1).map_err(|_| MaintenanceError::InvalidGraph)?;
        let target_working = Working::capture(&p.working)?.target(&manifest.value, &journal.value);
        let work = Work::Restore {
            target_working,
            source_digest: record::hex(p.backup.digest),
            source_bounds: p.backup.limits.into(),
            manifest: Blob {
                path: p.manifest.0.clone(),
                bytes: record::document(&p.manifest.1)?,
            },
            journal: Blob {
                path: p.journal.0.clone(),
                bytes: record::document(&p.journal.1)?,
            },
            pending_state: record::document(
                &p.state
                    .encode()
                    .map_err(|_| MaintenanceError::InvalidGraph)?,
            )?,
        };
        let request = Request::Restore {
            before: p.before.map(record::hex),
            observed: record::hex(digest(&p.working)),
            key: p.key.as_str().into(),
            source: p.source_locator.clone(),
        };
        let clean = p.clean_state(journal.value.target_working);
        Record::new(
            self.store,
            self.tx.limits,
            request,
            Working::capture(&p.working)?,
            &clean.encode().map_err(|_| MaintenanceError::InvalidGraph)?,
            None,
            work,
        )
    }

    fn applied(&self) -> Result<Option<ContentHash>, MaintenanceError> {
        self.tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map(|v| v.as_deref().map(hash))
            .map_err(|_| MaintenanceError::InvalidGraph)
    }
    pub(super) fn matching(
        &self,
        request: &Request,
    ) -> Result<Option<(Record, ContentHash)>, MaintenanceError> {
        let Some((r, h)) = self.reservation()? else {
            return Ok(None);
        };
        if r.request != *request {
            if r.pending.is_some() {
                return Err(MaintenanceError::Conflict);
            }
            return Ok(None);
        }
        Ok(Some((r, h)))
    }

    pub(super) fn restore_needs_source(&self, request: &Request) -> Result<bool, MaintenanceError> {
        let Some((r, _)) = self.matching(request)? else {
            return Ok(true);
        };
        let current = self.applied()?;
        if r.pending.is_some() && current == request.before()? {
            return Ok(true);
        }
        if current == Some(hash(r.final_state.as_bytes()))
            || matches!(&r.pending, Some(Work::Restore { pending_state, .. })
                if current == Some(hash(pending_state.as_bytes())))
        {
            return Ok(false);
        }
        Err(MaintenanceError::Conflict)
    }

    /// Admit the observed directory layout before replay and require the exact target at clean publication.
    pub(super) fn reconcile_restore(
        &self,
        record: &Record,
        key: &KeyId,
    ) -> Result<(), MaintenanceError> {
        let Some(Work::Restore { target_working, .. }) = &record.pending else {
            return Err(MaintenanceError::InvalidGraph);
        };
        let tx = Transaction::new(self.tx.guard, self.tx.limits).with_write_key(key);
        #[cfg(test)]
        let tx = if let Some(checkpoint) = self.tx.boundary {
            tx.with_checkpoint(checkpoint)
        } else {
            tx
        };
        let selected = tx
            .inspect_with_working_check(|snapshot| {
                record.working.permits_transition(target_working, snapshot)
            })
            .map_err(MaintenanceError::from)?;
        tx.reconcile_selected(selected, Some(&target_working.directories))
            .map_err(MaintenanceError::from)
    }

    fn finish_published_restore(
        &self,
        lease: &WriterLease,
        r: Record,
        h: ContentHash,
    ) -> Result<Value, MaintenanceError> {
        let Request::Restore { key, .. } = &r.request else {
            return Err(MaintenanceError::InvalidGraph);
        };
        let key = KeyId::new(key).map_err(|_| MaintenanceError::InvalidGraph)?;
        let Some(Work::Restore {
            pending_state,
            journal,
            manifest,
            ..
        }) = &r.pending
        else {
            return Err(MaintenanceError::InvalidGraph);
        };
        let actual = self.applied()?;
        let graph = self.capture()?;
        if actual == Some(hash(pending_state.as_bytes())) {
            if graph
                .artifacts
                .get(&journal.path)
                .is_none_or(|a| a.bytes() != journal.bytes.as_bytes())
                || graph
                    .artifacts
                    .get(&manifest.path)
                    .is_none_or(|a| a.bytes() != manifest.bytes.as_bytes())
            {
                return Err(MaintenanceError::Conflict);
            }
            graph.check_role_key(&graph.state.desired, &key)?;
            if let Some(lkg) = &graph.state.last_known_good {
                graph.check_role_key(lkg, &key)?;
            }
            self.reconcile_restore(&r, &key)?;
        } else if actual != Some(hash(r.final_state.as_bytes())) {
            return Err(MaintenanceError::Conflict);
        }
        self.finish(lease, r, h)
    }
    pub(super) fn resume_published_restore(
        &self,
        lease: &WriterLease,
        record: &Record,
    ) -> Result<Option<StatePointer>, MaintenanceError> {
        let Some((r, h)) = self.matching(&record.request)? else {
            return Ok(None);
        };
        if r.pending.is_none() {
            self.verify_terminal(&r)?;
            self.confirm_intent(h)?;
            return Ok(Some(r.state()?));
        }
        if r != *record {
            return Err(MaintenanceError::Conflict);
        }
        if self.applied()? == record.request.before()? {
            return Ok(None);
        }
        self.confirm_intent(h)?;
        let state = r.state()?;
        self.finish_published_restore(lease, r, h)?;
        Ok(Some(state))
    }

    pub(super) fn resume(
        &self,
        lease: &WriterLease,
        request: &Request,
        source: &mut Option<Backup>,
    ) -> Result<Option<Value>, MaintenanceError> {
        self.proof(lease)?;
        let Some((r, h)) = self.matching(request)? else {
            return Ok(None);
        };
        self.confirm_intent(h)?;
        self.check_keys(request)?;
        let Some(work) = &r.pending else {
            return self.verify_terminal(&r).map(Some);
        };
        require_compatible_limits(r.bounds.limits(), self.tx.limits)?;
        let current = self.applied()?;
        match (request, work) {
            (Request::Gc { .. }, Work::Gc { .. }) => {
                let plan = self.gc_from_record(&r)?;
                self.collect(lease, &plan)?;
            }
            (
                Request::Restore {
                    before: _,
                    observed,
                    key,
                    source: locator,
                },
                Work::Restore {
                    source_digest,
                    source_bounds,
                    manifest,
                    journal,
                    ..
                },
            ) => {
                if current != request.before()? {
                    return self.finish_published_restore(lease, r, h).map(Some);
                }
                let captured = source.take().ok_or(MaintenanceError::Conflict)?;
                if record::hex(captured.digest) != *source_digest
                    || record::Bounds::from(captured.limits) != *source_bounds
                {
                    return Err(MaintenanceError::Conflict);
                }
                let key = KeyId::new(key).map_err(|_| MaintenanceError::InvalidGraph)?;
                let plan = self.restore_with(
                    lease,
                    captured,
                    request.before()?,
                    observed
                        .parse()
                        .map_err(|_| MaintenanceError::InvalidGraph)?,
                    &key,
                    locator.clone(),
                    Some((manifest, journal)),
                )?;
                if self.restore_record(&plan)? != r {
                    return Err(MaintenanceError::Conflict);
                }
                self.restore(lease, &plan)?;
            }
            (
                Request::Rotate {
                    old_key, new_key, ..
                },
                Work::Rotate {
                    ciphertext,
                    manifests,
                },
            ) => {
                if current == Some(hash(r.final_state.as_bytes())) {
                    return self.finish(lease, r, h).map(Some);
                }
                if current != request.before()? {
                    return Err(MaintenanceError::Conflict);
                }
                let old = KeyId::new(old_key).map_err(|_| MaintenanceError::InvalidGraph)?;
                let new = KeyId::new(new_key).map_err(|_| MaintenanceError::InvalidGraph)?;
                let plan = self.rotation_with(lease, &old, &new, Some((ciphertext, manifests)))?;
                if self.rotation_record(&plan)? != r {
                    return Err(MaintenanceError::Conflict);
                }
                self.rotate(lease, &plan)?;
            }
            (Request::Retain { key, pins, .. }, Work::Retain) => {
                if current == Some(hash(r.final_state.as_bytes())) {
                    return self.finish(lease, r, h).map(Some);
                }
                let expected = request.before()?.ok_or(MaintenanceError::InvalidGraph)?;
                if current != Some(expected) {
                    return Err(MaintenanceError::Conflict);
                }
                let refs = pins
                    .iter()
                    .map(Pin::reference)
                    .collect::<Result<Vec<_>, _>>()?;
                self.retain_inner(
                    lease,
                    expected,
                    &refs,
                    &KeyId::new(key).map_err(|_| MaintenanceError::InvalidGraph)?,
                )?;
            }
            _ => return Err(MaintenanceError::InvalidGraph),
        }
        let (done, _) = self
            .matching(request)?
            .ok_or(MaintenanceError::InvalidGraph)?;
        if done.pending.is_some() {
            return Err(MaintenanceError::Conflict);
        }
        self.verify_terminal(&done).map(Some)
    }

    pub(super) fn rotation_record(&self, p: &RotationPlan) -> Result<Record, MaintenanceError> {
        let ciphertext = p
            .private
            .iter()
            .map(|sealed| {
                let id = sealed.identity();
                Ok(Blob {
                    path: format!(
                        "private/{}/{}.sealed.json",
                        id.scope().record_uuid(),
                        id.object_version()
                    ),
                    bytes: record::document(sealed.sealed_bytes())?,
                })
            })
            .collect::<Result<_, MaintenanceError>>()?;
        let manifests = p
            .manifests
            .iter()
            .map(|(path, raw)| {
                Ok(Blob {
                    path: path.clone(),
                    bytes: record::document(raw)?,
                })
            })
            .collect::<Result<_, MaintenanceError>>()?;
        Record::new(
            self.store,
            self.tx.limits,
            Request::Rotate {
                before: record::hex(p.before),
                old_key: p.old_key.as_str().into(),
                new_key: p.new_key.as_str().into(),
            },
            Working::capture(&p.working)?,
            &p.state
                .encode()
                .map_err(|_| MaintenanceError::InvalidGraph)?,
            None,
            Work::Rotate {
                ciphertext,
                manifests,
            },
        )
    }
    pub(super) fn gc_record(&self, p: &GarbagePlan) -> Result<Record, MaintenanceError> {
        let candidates: BTreeSet<_> = p.paths.iter().collect();
        let mut survivors = Vec::new();
        for path in &p.inventory {
            if !candidates.contains(path) {
                survivors.push(Survivor {
                    path: path.clone(),
                    sha256: p
                        .registered
                        .artifacts
                        .get(path)
                        .map(|a| record::hex(hash(a.bytes()))),
                });
            }
        }
        let registered = p
            .registered
            .artifacts()
            .map(|(path, bytes)| {
                Ok(Blob {
                    path: path.into(),
                    bytes: record::document(bytes)?,
                })
            })
            .collect::<Result<Vec<_>, MaintenanceError>>()?;
        Record::new(
            self.store,
            self.tx.limits,
            Request::Gc {
                before: record::hex(p.state),
            },
            Working::capture(&self.baseline()?)?,
            &p.registered.raw_state,
            Some(GcResult {
                removed: p.paths.len(),
                survivors,
            }),
            Work::Gc {
                inventory: p.inventory.clone(),
                registered,
                candidates: p.paths.clone(),
            },
        )
    }

    pub(super) fn gc_from_record(&self, r: &Record) -> Result<GarbagePlan, MaintenanceError> {
        let Some(Work::Gc {
            inventory,
            registered: proof,
            candidates,
        }) = &r.pending
        else {
            return Err(MaintenanceError::InvalidGraph);
        };
        let mut registered = self.capture()?;
        if hash(r.final_state.as_bytes()) != registered.digest
            || r.request.before()? != Some(registered.digest)
        {
            return Err(MaintenanceError::Conflict);
        }
        r.working.check(&self.baseline()?)?;
        let reachable = registered
            .artifacts
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = self
            .tx
            .guard
            .discover_artifacts(self.tx.limits)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        let original = inventory.iter().collect::<BTreeSet<_>>();
        let eligible = candidates.iter().collect::<BTreeSet<_>>();
        if original.len() != inventory.len()
            || eligible.len() != candidates.len()
            || actual.iter().any(|p| !original.contains(p))
        {
            return Err(MaintenanceError::Conflict);
        }
        let current = actual.iter().collect::<BTreeSet<_>>();
        let blobs = proof
            .iter()
            .map(|b| (&b.path, b.bytes.as_str()))
            .collect::<BTreeMap<_, _>>();
        if blobs.len() != proof.len() {
            return Err(MaintenanceError::InvalidGraph);
        }
        let mut missing = BTreeMap::new();
        for path in original.difference(&current) {
            if !eligible.contains(path) || reachable.contains(*path) {
                return Err(MaintenanceError::Conflict);
            }
            missing.insert(
                path.as_str(),
                *blobs.get(*path).ok_or(MaintenanceError::InvalidGraph)?,
            );
        }
        self.registered(&mut registered, inventory, &missing)?;
        if !registered
            .artifacts()
            .eq(proof.iter().map(|b| (b.path.as_str(), b.bytes.as_bytes())))
        {
            return Err(MaintenanceError::Conflict);
        }
        let paths = if registered.state.journal.is_some() {
            Vec::new()
        } else {
            registered
                .artifacts
                .keys()
                .filter(|p| !reachable.contains(*p))
                .cloned()
                .collect::<Vec<_>>()
        };
        if paths != *candidates {
            return Err(MaintenanceError::Conflict);
        }
        let plan = GarbagePlan {
            state: registered.digest,
            paths,
            inventory: inventory.clone(),
            registered,
        };
        if self.gc_record(&plan)? != *r {
            return Err(MaintenanceError::Conflict);
        }
        Ok(plan)
    }
}
