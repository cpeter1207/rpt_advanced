//! Account-bound private store. Only verified owners can project configured state.

use crate::configuration_transaction::TransactionError;
use rpt_advanced_core::config::{
    resources::{
        candidate::{ResourceBinding, ResourceId, ResourceKind, ResourceScope},
        manifest::{ContentHash, PrivateReference},
        profiles::credentials::{CredentialOwner, CredentialOwners, CredentialSlot},
    },
    valid_admin_hash, valid_mailbox_pin_hash,
};
use rpt_advanced_core::management::patch::PrivateChange;
use rptadv_json::{BorrowedChildren, decode_borrowed};
use rptadv_peer_support::{
    sealed_records::{
        KeyId, KeyLoadError, RecordIdentity, RecordScope, SealedLimits, SecretBytes,
        load_systemd_key,
    },
    working_files::{
        CaptureLimits, EditGuard,
        immutable::{
            ArtifactLimits,
            private_records::{OpenedPrivateRecord, PreparedPrivateRecord, PrivateRecordError},
        },
        transaction::new_identity,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

type SlotKey = (ResourceId, CredentialSlot);

/// One explicit private operation after a public resource patch or create.
pub struct CredentialEdit {
    /// Exact server-selected owner.
    pub resource: ResourceId,
    /// Private field of that owner.
    pub slot: CredentialSlot,
    /// Zeroizing supplied JSON or explicit removal.
    pub change: PrivateChange,
}

/// Value-free classification of private store availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretsState {
    /// All selected private records were authenticated.
    Ready,
    /// Required systemd credential is unavailable.
    Locked,
    /// Integrity, binding, format or storage failure.
    Error,
}

impl From<PrivateRecordError> for SecretsState {
    fn from(e: PrivateRecordError) -> Self {
        match e {
            PrivateRecordError::Key(KeyLoadError::Missing | KeyLoadError::Unavailable) => {
                Self::Locked
            }
            _ => Self::Error,
        }
    }
}
struct Entry {
    reference: PrivateReference,
    encoded_len: u64,
    owner: Value,
    affinity: Value,
    value: Option<Arc<SecretBytes>>,
}

/// An authenticated zeroizing value and its opaque generation identity.
/// Delegates never infer credential changes from plaintext or allocation addresses.
pub(crate) struct RetainedCredential {
    pub(crate) bytes: Arc<SecretBytes>,
    reference: PrivateReference,
}
impl RetainedCredential {
    pub(crate) fn same_version(&self, other: &Self) -> bool {
        self.reference == other.reference
    }
}

/// Authenticated index and credential bytes, held off the audio path.
#[derive(Default)]
pub struct Credentials {
    root: Option<PrivateReference>,
    warnings: usize,
    key: Option<KeyId>,
    entries: BTreeMap<SlotKey, Entry>,
    owners: CredentialOwners,
}

impl Credentials {
    pub(crate) fn reference(&self) -> Option<PrivateReference> {
        self.root
    }
    /// Physical encrypted references authenticated by this selected account graph.
    pub(crate) fn references(&self) -> impl Iterator<Item = PrivateReference> + '_ {
        self.root
            .into_iter()
            .chain(self.entries.values().map(|entry| entry.reference))
    }

    /// Lookup a checked credential. A mask or an incomplete/mismatched account is unusable.
    pub fn get(&self, resource: ResourceId, slot: CredentialSlot) -> Option<&SecretBytes> {
        self.selected_entry(resource, slot)?.value.as_deref()
    }

    /// Retain only the selected zeroizing credential across a durable owner handoff.
    /// Repeated/inherited bindings share its allocation until their last owner retires.
    pub(crate) fn retain(
        &self,
        resource: ResourceId,
        slot: CredentialSlot,
    ) -> Option<RetainedCredential> {
        let entry = self.selected_entry(resource, slot)?;
        Some(RetainedCredential {
            bytes: Arc::clone(entry.value.as_ref()?),
            reference: entry.reference,
        })
    }

    fn selected_entry(&self, resource: ResourceId, slot: CredentialSlot) -> Option<&Entry> {
        let owner = self.owners.get(&(resource, slot))?;
        let affinity = owner.affinity.as_ref()?;
        let entry = self.entries.get(&(resource, slot)).or_else(|| {
            if !matches!(owner.binding.scope, ResourceScope::Port(_)) || slot == CredentialSlot::Pin
            {
                return None;
            }
            let site = self.owners.iter().find(|((_, k), o)| {
                *k == slot && matches!(o.binding.scope, ResourceScope::Site(_))
            })?;
            self.entries.get(site.0)
        })?;
        (&entry.affinity == affinity).then_some(entry)
    }

    /// Distinguish absent owners from verified configured/unconfigured state.
    pub fn configured(&self, resource: ResourceId, slot: CredentialSlot) -> Option<bool> {
        self.owners
            .contains_key(&(resource, slot))
            .then(|| self.get(resource, slot).is_some())
    }

    fn complete_mailboxes(&self) -> bool {
        self.owners
            .keys()
            .filter(|(_, slot)| *slot == CredentialSlot::Pin)
            .all(|key| {
                self.entries
                    .get(key)
                    .is_some_and(|entry| entry.value.is_some())
            })
    }
}

/// Selected public graph remains available when private material cannot be read.
pub struct PrivateSelection(Result<Credentials, SecretsState>);
impl PrivateSelection {
    /// Public, value-free status for management reads.
    pub fn state(&self) -> SecretsState {
        self.0.as_ref().map_or_else(|e| *e, |_| SecretsState::Ready)
    }
    pub(crate) fn warning_count(&self) -> usize {
        self.0.as_ref().map_or(0, |c| c.warnings)
    }
    /// Unknown configured flags are omitted when unavailable.
    pub fn configured(&self, resource: ResourceId, slot: CredentialSlot) -> Option<bool> {
        self.0.as_ref().ok()?.configured(resource, slot)
    }
    /// Verified graph for internal credential consumers only.
    pub fn credentials(&self) -> Result<&Credentials, SecretsState> {
        self.0.as_ref().map_err(|e| *e)
    }
    pub(crate) fn into_credentials(self) -> Result<Credentials, SecretsState> {
        self.0
    }
}

fn key_error(e: KeyLoadError) -> SecretsState {
    PrivateRecordError::Key(e).into()
}

/// Check the declared identity and exclude reserved credential subtrees before public storage.
pub(crate) fn public_envelope(binding: ResourceBinding, bytes: &[u8]) -> bool {
    check_envelope(binding, bytes, false)
}

/// The import normalizer may discard wrong-typed COMMAND.admin. Keep reserved names out
/// of staging even when someone placed them inside a malformed array.
pub(crate) fn public_input_envelope(binding: ResourceBinding, bytes: &[u8]) -> bool {
    check_envelope(binding, bytes, true)
}

pub(crate) fn has_admin_hash(value: &rptadv_json::BorrowedValue<'_>) -> bool {
    match &value.children {
        BorrowedChildren::Object(fields) => {
            fields.contains_key("lock_hash")
                || fields.contains_key("unlock_hash")
                || fields.values().any(has_admin_hash)
        }
        BorrowedChildren::Array(items) => items.iter().any(has_admin_hash),
        BorrowedChildren::Scalar => false,
    }
}

fn check_envelope(binding: ResourceBinding, bytes: &[u8], staging: bool) -> bool {
    let Ok(root) = decode_borrowed(bytes) else {
        return false;
    };
    let BorrowedChildren::Object(fields) = root.children else {
        return false;
    };
    let text = |key: &str| serde_json::from_str::<String>(fields.get(key)?.raw()).ok();
    if text("id").and_then(|s| s.parse::<ResourceId>().ok()) != Some(binding.id)
        || text("kind").as_deref() != Some(binding.kind.as_str())
    {
        return false;
    }
    use ResourceKind::*;
    if !matches!(
        binding.kind,
        Allstarlink | Echolink | EcholinkProxy | PhonePatch | Command | Mailbox
    ) {
        return true;
    }
    let Some(config) = fields.get("configuration") else {
        return false;
    };
    let BorrowedChildren::Object(body) = &config.children else {
        return false;
    };
    match binding.kind {
        Allstarlink | Echolink | EcholinkProxy | PhonePatch => {
            !body.contains_key("credentials") && !body.contains_key("proxy")
        }
        Command => body.get("admin").is_none_or(|admin| match &admin.children {
            BorrowedChildren::Object(fields) => {
                !fields.contains_key("lock_hash") && !fields.contains_key("unlock_hash")
            }
            _ => staging && !has_admin_hash(admin),
        }),
        Mailbox => !body.contains_key("pin_hash") && !body.contains_key("pin_configured"),
        _ => true,
    }
}

/// Bounded guarded reader/writer. The public graph is charged by its caller.
pub(crate) struct PrivateStore<'a, 's> {
    pub guard: &'a EditGuard<'s>,
    pub store: ResourceId,
    pub limits: CaptureLimits,
    pub write_key: Option<&'a KeyId>,
}
impl PrivateStore<'_, '_> {
    fn limits(&self, remaining: u64) -> (ArtifactLimits, SealedLimits) {
        let cap = self.limits.max_file_bytes.min(remaining);
        let bytes = usize::try_from(cap).unwrap_or(usize::MAX);
        (
            ArtifactLimits {
                max_file_bytes: cap,
                max_path_bytes: self.limits.max_path_bytes,
            },
            SealedLimits {
                max_plaintext_bytes: bytes,
                max_document_bytes: bytes,
            },
        )
    }
    fn scope(&self, record: ResourceId) -> Result<RecordScope, SecretsState> {
        RecordScope::new(&self.store.to_string(), &record.to_string())
            .map_err(|_| SecretsState::Error)
    }
    pub(crate) fn open(
        &self,
        r: PrivateReference,
        remaining: &mut u64,
    ) -> Result<OpenedPrivateRecord, SecretsState> {
        self.open_with_witness(r, remaining, None)
    }
    pub(crate) fn open_with_witness(
        &self,
        r: PrivateReference,
        remaining: &mut u64,
        missing_bytes: Option<&[u8]>,
    ) -> Result<OpenedPrivateRecord, SecretsState> {
        let identity = RecordIdentity::new(self.scope(r.record)?, &r.version.to_string())
            .map_err(|_| SecretsState::Error)?;
        let (artifact, sealed) = self.limits(*remaining);
        let opened =
            match self
                .guard
                .open_private_record(identity, r.sha256.as_bytes(), artifact, sealed)
            {
                Err(PrivateRecordError::Read(
                    rptadv_peer_support::working_files::immutable::ReadError::Store(
                        rptadv_peer_support::working_files::CaptureError::Missing,
                    ),
                )) if missing_bytes.is_some() => OpenedPrivateRecord::from_witness(
                    identity,
                    r.sha256.as_bytes(),
                    missing_bytes.unwrap(),
                    sealed,
                )?,
                value => value?,
            };
        *remaining -= opened.encoded_len() as u64;
        Ok(opened)
    }
    fn install(
        &self,
        record: Option<ResourceId>,
        plaintext: SecretBytes,
        key: &rptadv_peer_support::sealed_records::SecretKey,
        remaining: &mut u64,
    ) -> Result<PrivateReference, TransactionError> {
        let record = match record {
            Some(id) => id,
            None => new_identity()
                .map_err(|_| TransactionError::StorageFault)?
                .to_string()
                .parse()
                .map_err(|_| TransactionError::StorageFault)?,
        };
        let (artifact, sealed) = self.limits(*remaining);
        let scope = self
            .scope(record)
            .map_err(|_| TransactionError::StorageFault)?;
        let prepared =
            PreparedPrivateRecord::seal(key, &scope, &plaintext, sealed).map_err(|error| {
                match error {
                    PrivateRecordError::Sealed(
                        rptadv_peer_support::sealed_records::SealedError::SizeLimit,
                    ) => TransactionError::InvalidCandidate,
                    _ => TransactionError::StorageFault,
                }
            })?;
        *remaining -= prepared.encoded_len() as u64;
        self.guard
            .prepare_private_parent(self.store.uuid(), &prepared)
            .map_err(|_| TransactionError::StorageFault)?;
        let receipt = self
            .guard
            .install_private_record(&prepared, artifact)
            .map_err(|_| TransactionError::StorageFault)?;
        Ok(PrivateReference {
            record,
            version: receipt
                .identity()
                .object_version()
                .to_string()
                .parse()
                .map_err(|_| TransactionError::StorageFault)?,
            sha256: ContentHash::from_bytes(*receipt.sha256()),
        })
    }
    /// Read every referenced object before exposing any private state.
    pub fn select(
        &self,
        root: Option<PrivateReference>,
        owners: Result<CredentialOwners, SecretsState>,
    ) -> PrivateSelection {
        self.select_with(root, owners, |reference, remaining| {
            self.open(reference, remaining)
        })
    }
    pub(crate) fn select_with(
        &self,
        root: Option<PrivateReference>,
        owners: Result<CredentialOwners, SecretsState>,
        open: impl FnMut(PrivateReference, &mut u64) -> Result<OpenedPrivateRecord, SecretsState>,
    ) -> PrivateSelection {
        PrivateSelection(owners.and_then(|owners| self.read(root, owners, open)))
    }
    fn read(
        &self,
        root: Option<PrivateReference>,
        owners: CredentialOwners,
        mut open: impl FnMut(PrivateReference, &mut u64) -> Result<OpenedPrivateRecord, SecretsState>,
    ) -> Result<Credentials, SecretsState> {
        let invalid = SecretsState::Error;
        let mut result = Credentials {
            root,
            owners,
            ..Credentials::default()
        };
        if let Some(root) = root {
            if self.limits.max_paths < 2 {
                return Err(invalid);
            }
            let mut remaining = self.limits.max_total_bytes;
            let doc = open(root, &mut remaining)?;
            if self.write_key.is_some_and(|k| k != doc.key_id()) {
                return Err(SecretsState::Locked);
            }
            result.key = Some(doc.key_id().clone());
            let index =
                rptadv_json::decode_value(doc.plaintext().expose_bytes()).map_err(|_| invalid)?;
            let rows = index
                .get("records")
                .and_then(Value::as_array)
                .ok_or(invalid)?;
            if index["format_version"].as_u64() != Some(1)
                || rows.is_empty()
                || rows.len() >= self.limits.max_paths
            {
                return Err(invalid);
            }
            result.warnings += doc.warnings().len() + index.as_object().ok_or(invalid)?.len() - 2;
            let mut records = BTreeSet::from([root.record]);
            for row in rows {
                let [identity, reference] = row.as_array().ok_or(invalid)?.as_slice() else {
                    return Err(invalid);
                };
                let resource = identity
                    .get(2)
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<ResourceId>().ok())
                    .ok_or(invalid)?;
                let slot = identity.get(3).and_then(Value::as_str).ok_or(invalid)?;
                let (&key, owner) = result
                    .owners
                    .range(
                        (resource, CredentialSlot::AllStarLink)..=(resource, CredentialSlot::Pin),
                    )
                    .find(|((_, s), _)| s.tag() == slot)
                    .ok_or(invalid)?;
                if *identity != owner.identity(key.1) {
                    return Err(invalid);
                }
                let r = PrivateReference::decode(reference).map_err(|_| invalid)?;
                result.warnings += reference.as_object().ok_or(invalid)?.len() - 3;
                if !records.insert(r.record) || result.entries.contains_key(&key) {
                    return Err(invalid);
                }
                let doc = open(r, &mut remaining)?;
                if Some(doc.key_id()) != result.key.as_ref() {
                    return Err(invalid);
                }
                let (entry, warnings) = decode_entry(&doc, r, identity, owner, key.1)?;
                result.warnings += warnings;
                result.entries.insert(key, entry);
            }
        }
        if root.is_none()
            && let Some(key) = self.write_key
        {
            load_systemd_key(key).map_err(key_error)?;
        }
        result.complete_mailboxes().then_some(result).ok_or(invalid)
    }

    /// Carry forward only authenticated same-owner records. Install edits and then their index.
    pub fn prepare(
        &self,
        old: &Credentials,
        owners: &CredentialOwners,
        edits: &[CredentialEdit],
    ) -> Result<(Option<PrivateReference>, usize), TransactionError> {
        let invalid = TransactionError::InvalidCandidate;
        if edits.len() > self.limits.max_paths {
            return Err(invalid);
        }
        let mut requested = BTreeMap::new();
        let mut warnings = 0;
        let mut input_budget = self.limits.max_total_bytes;
        for edit in edits {
            if let PrivateChange::Set(token) = &edit.change {
                let len = token.as_json().len() as u64;
                if len > self.limits.max_file_bytes {
                    return Err(invalid);
                }
                input_budget = input_budget.checked_sub(len).ok_or(invalid)?;
            }
            let key = (edit.resource, edit.slot);
            if !owners.contains_key(&key) || requested.insert(key, &edit.change).is_some() {
                return Err(invalid);
            }
        }
        let mut rows = Vec::new();
        let mut changed = false;
        let mut key_material = None;
        let mut remaining = self.limits.max_total_bytes;
        let mut install = |record, plaintext, remaining: &mut u64| {
            if key_material.is_none() {
                let id = self
                    .write_key
                    .or(old.key.as_ref())
                    .ok_or(TransactionError::SecretsUnavailable)?;
                key_material =
                    Some(load_systemd_key(id).map_err(|_| TransactionError::SecretsUnavailable)?);
            }
            self.install(
                record,
                plaintext,
                key_material.as_ref().expect("loaded key"),
                remaining,
            )
        };
        for (&key, owner) in owners {
            let slot = key.1;
            let identity = owner.identity(slot);
            let prior = old.entries.get(&key).filter(|p| p.owner == identity);
            let mut edit = requested.get(&key);
            let mut supplied = None;
            if let Some(PrivateChange::Set(value)) = edit {
                match decode_value(value.as_json(), slot) {
                    Ok(value) => supplied = value,
                    Err(_) => {
                        warnings += 1;
                        edit = None;
                    }
                }
            }
            let reference = match (edit, prior) {
                (None, Some(entry))
                    if entry.value.is_none()
                        || owner.affinity.as_ref() == Some(&entry.affinity) =>
                {
                    remaining = remaining.checked_sub(entry.encoded_len).ok_or(invalid)?;
                    entry.reference
                }
                (None, None) if slot != CredentialSlot::Pin => continue,
                _ => {
                    let raw = match edit {
                        Some(PrivateChange::Set(value)) => Some(value.as_json()),
                        _ => None,
                    };
                    let value = supplied;
                    if slot == CredentialSlot::Pin && value.is_none() {
                        return Err(invalid);
                    }
                    let affinity = if value.is_some() {
                        owner.affinity.as_ref().ok_or(invalid)?
                    } else {
                        &Value::Null
                    };
                    let raw = if value.is_some() {
                        raw.expect("string operation")
                    } else {
                        b"null"
                    };
                    let plaintext = encode_entry(&identity, affinity, raw).map_err(|_| invalid)?;
                    changed = true;
                    if rows.len() + 1 >= self.limits.max_paths {
                        return Err(invalid);
                    }
                    install(prior.map(|p| p.reference.record), plaintext, &mut remaining)?
                }
            };
            rows.push(json!([identity, reference.encode()]));
        }
        if rows.is_empty() {
            return Ok((None, warnings));
        }
        if rows.len() >= self.limits.max_paths {
            return Err(invalid);
        }
        if !changed && rows.len() == old.entries.len() {
            return Ok((old.root, warnings));
        }
        let index =
            serde_json::to_vec(&json!({"format_version":1,"records":rows})).map_err(|_| invalid)?;
        install(
            old.root.map(|r| r.record),
            SecretBytes::new(index),
            &mut remaining,
        )
        .map(|r| (Some(r), warnings))
    }
}

fn encode_entry(
    identity: &Value,
    affinity: &Value,
    raw: &[u8],
) -> Result<SecretBytes, SecretsState> {
    let mut encoded =
        serde_json::to_vec(&json!({"format_version":1,"owner":identity,"account":affinity}))
            .map_err(|_| SecretsState::Error)?;
    encoded.pop();
    encoded.reserve_exact(raw.len() + 11);
    encoded.extend_from_slice(b",\"value\":");
    encoded.extend_from_slice(raw);
    encoded.push(b'}');
    Ok(SecretBytes::new(encoded))
}

fn decode_value(bytes: &[u8], slot: CredentialSlot) -> Result<Option<SecretBytes>, SecretsState> {
    use serde_json::de::{Read, SliceRead};
    let Some(tail) = bytes.strip_prefix(b"\"") else {
        return Err(SecretsState::Error);
    };
    // SliceRead borrows an unescaped string, or unescapes into our preallocated
    // zeroizing scratch. The decoded UTF-8 never exceeds the original token.
    let value = SecretBytes::try_fill(tail.len(), |scratch| {
        let mut reader = SliceRead::new(tail);
        let len = reader
            .parse_str(scratch)
            .map_err(|_| SecretsState::Error)?
            .len();
        if reader.byte_offset() != tail.len() {
            return Err(SecretsState::Error);
        }
        if scratch.is_empty() {
            scratch.extend_from_slice(&tail[..len]);
        }
        Ok(())
    })?;
    let text = std::str::from_utf8(value.expose_bytes()).expect("decoded JSON string");
    if text.is_empty() {
        return Ok(None);
    }
    let valid = match slot {
        CredentialSlot::Pin => valid_mailbox_pin_hash(text),
        CredentialSlot::Lock | CredentialSlot::Unlock => valid_admin_hash(text),
        _ => true,
    };
    if !valid {
        return Err(SecretsState::Error);
    }
    Ok(Some(value))
}

fn decode_entry(
    doc: &OpenedPrivateRecord,
    reference: PrivateReference,
    identity: &Value,
    owner: &CredentialOwner,
    slot: CredentialSlot,
) -> Result<(Entry, usize), SecretsState> {
    let invalid = SecretsState::Error;
    let root = decode_borrowed(doc.plaintext().expose_bytes()).map_err(|_| invalid)?;
    let BorrowedChildren::Object(fields) = root.children else {
        return Err(invalid);
    };
    let token = |s: &str| fields.get(s).map(|v| v.raw().as_bytes()).ok_or(invalid);
    let public = |key| rptadv_json::decode_value(token(key)?).map_err(|_| invalid);
    if public("format_version")?.as_u64() != Some(1) || public("owner")? != *identity {
        return Err(invalid);
    }
    let affinity = public("account")?;
    let raw = token("value")?;
    let value = if raw == b"null" {
        if slot == CredentialSlot::Pin || !affinity.is_null() {
            return Err(invalid);
        }
        None
    } else {
        if owner.affinity.as_ref() != Some(&affinity) {
            return Err(invalid);
        }
        Some(Arc::new(decode_value(raw, slot)?.ok_or(invalid)?))
    };
    let warnings = doc.warnings().len() + fields.len() - 4;
    Ok((
        Entry {
            reference,
            encoded_len: doc.encoded_len() as u64,
            owner: identity.clone(),
            affinity,
            value,
        },
        warnings,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_private_json_decoding_uses_one_plaintext_allocation() {
        for (raw, expected) in [
            (br#""simple""#.as_slice(), "simple"),
            (
                br#""escaped\"quote\n\t\uD83D\uDE00""#.as_slice(),
                "escaped\"quote\n\t\u{1F600}",
            ),
        ] {
            crate::fixture::ALLOCATION_COUNT.with(|count| {
                count.set(Some(0));
                let result = decode_value(raw, CredentialSlot::Phone);
                let allocations = count.replace(None);
                assert_eq!(result.unwrap().unwrap().expose_bytes(), expected.as_bytes());
                assert_eq!(allocations, Some(1));
            });
        }
    }
}
