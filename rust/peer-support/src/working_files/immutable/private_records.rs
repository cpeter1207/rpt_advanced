//! Guarded persistence of authenticated encrypted private records.

use std::fmt;

use super::{
    ArtifactLimits, EditGuard, ImmutableTarget, InstallBoundary, InstallError, InstallOutcome,
    InstalledArtifact, ReadError,
};
use crate::sealed_records::{
    KeyId, KeyLoadError, RecordIdentity, RecordScope, SealedError, SealedLimits, SealedRecord,
    SealedWarning, SecretBytes, SecretKey, load_systemd_key, seal,
};

#[cfg(test)]
mod tests;

/// Sealed envelope prepared once for immutable installation and exact retries.
pub struct PreparedPrivateRecord {
    identity: RecordIdentity,
    encoded: Vec<u8>,
}

impl PreparedPrivateRecord {
    /// Encrypt opaque bytes under a fresh version and encode the resulting envelope.
    pub fn seal(
        key: &SecretKey,
        scope: &RecordScope,
        plaintext: &SecretBytes,
        limits: SealedLimits,
    ) -> Result<Self, PrivateRecordError> {
        let record = seal(key, scope, plaintext, limits).map_err(PrivateRecordError::Sealed)?;
        let encoded = record.encode(limits).map_err(PrivateRecordError::Sealed)?;
        Ok(Self {
            identity: *record.identity(),
            encoded,
        })
    }

    /// Exact ciphertext envelope bytes, safe for manifest/object backup.
    pub fn sealed_bytes(&self) -> &[u8] {
        &self.encoded
    }

    /// Number of sealed bytes charged before installation.
    pub fn encoded_len(&self) -> usize {
        self.encoded.len()
    }

    /// The identity authenticated by the envelope.
    pub fn identity(&self) -> RecordIdentity {
        self.identity
    }
}

impl fmt::Debug for PreparedPrivateRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedPrivateRecord(..)")
    }
}

/// Receipt for a synchronized sealed private record.
pub struct InstalledPrivateRecord {
    identity: RecordIdentity,
    artifact: InstalledArtifact,
}

impl InstalledPrivateRecord {
    /// The identity of the installed envelope.
    pub fn identity(&self) -> RecordIdentity {
        self.identity
    }

    /// How the synchronized envelope was obtained.
    pub fn outcome(&self) -> InstallOutcome {
        self.artifact.outcome()
    }

    /// SHA-256 of the exact stored envelope bytes.
    pub fn sha256(&self) -> &[u8; 32] {
        self.artifact.sha256()
    }
}

impl fmt::Debug for InstalledPrivateRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InstalledPrivateRecord(..)")
    }
}

/// Authenticated opaque plaintext for the private payload owner.
pub struct OpenedPrivateRecord {
    identity: RecordIdentity,
    plaintext: SecretBytes,
    key_id: KeyId,
    encoded: Vec<u8>,
    warnings: Vec<SealedWarning>,
}

impl OpenedPrivateRecord {
    /// Authenticate previously captured ciphertext against a graph-derived identity and digest.
    pub fn from_witness(
        identity: RecordIdentity,
        expected_sha256: &[u8; 32],
        bytes: &[u8],
        sealed_limits: SealedLimits,
    ) -> Result<Self, PrivateRecordError> {
        if bytes.len() > sealed_limits.max_document_bytes {
            return Err(PrivateRecordError::Sealed(SealedError::SizeLimit));
        }
        if super::artifact_sha256(bytes) != *expected_sha256 {
            return Err(PrivateRecordError::Read(ReadError::DigestMismatch));
        }
        Self::authenticate(identity, bytes, sealed_limits, load_systemd_key)
    }

    fn authenticate(
        identity: RecordIdentity,
        bytes: &[u8],
        sealed_limits: SealedLimits,
        load_key: impl FnOnce(&KeyId) -> Result<SecretKey, KeyLoadError>,
    ) -> Result<Self, PrivateRecordError> {
        let decoded = SealedRecord::decode(identity, bytes, sealed_limits)
            .map_err(PrivateRecordError::Sealed)?;
        let key = load_key(decoded.record.key_id()).map_err(PrivateRecordError::Key)?;
        let plaintext = decoded
            .record
            .open(&key, sealed_limits)
            .map_err(PrivateRecordError::Sealed)?;
        Ok(Self {
            identity,
            plaintext,
            key_id: decoded.record.key_id().clone(),
            encoded: bytes.to_vec(),
            warnings: decoded.warnings,
        })
    }

    /// Retain only the authenticated ciphertext for an exact immutable installation.
    pub fn into_prepared(self) -> PreparedPrivateRecord {
        PreparedPrivateRecord {
            identity: self.identity,
            encoded: self.encoded,
        }
    }

    /// The identity against which the envelope was authenticated.
    pub fn identity(&self) -> RecordIdentity {
        self.identity
    }

    /// Borrow the zeroizing owner of the authenticated plaintext.
    pub fn plaintext(&self) -> &SecretBytes {
        &self.plaintext
    }

    /// Identifier of the key used to authenticate the record.
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    /// Exact encrypted size charged to the aggregate read budget.
    pub fn encoded_len(&self) -> usize {
        self.encoded.len()
    }

    /// Exact authenticated ciphertext envelope; plaintext stays in its opaque owner.
    pub fn sealed_bytes(&self) -> &[u8] {
        &self.encoded
    }

    /// Value-free warnings from decoding the sealed envelope.
    pub fn warnings(&self) -> &[SealedWarning] {
        &self.warnings
    }
}

impl fmt::Debug for OpenedPrivateRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenedPrivateRecord(..)")
    }
}

/// Content-free failure from private record persistence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivateRecordError {
    /// The immutable envelope could not be installed.
    Install(InstallError),
    /// The protected envelope could not be read or verified.
    Read(ReadError),
    /// The selected systemd credential could not be loaded.
    Key(KeyLoadError),
    /// The sealed record could not be encoded, decoded or authenticated.
    Sealed(SealedError),
}

impl fmt::Display for PrivateRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install(error) => error.fmt(f),
            Self::Read(error) => error.fmt(f),
            Self::Key(error) => error.fmt(f),
            Self::Sealed(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PrivateRecordError {}

impl EditGuard<'_> {
    /// Install the exact prepared envelope under its canonical private path.
    ///
    /// The caller associates this guarded root with the prepared store UUID.
    /// Keep the prepared record for an exact retry after uncertain durability.
    pub fn install_private_record(
        &self,
        prepared: &PreparedPrivateRecord,
        limits: ArtifactLimits,
    ) -> Result<InstalledPrivateRecord, PrivateRecordError> {
        self.install_private_record_with(prepared, limits, |_| Ok(()))
    }

    /// Install a prepared envelope with the shared per-call commit hook.
    fn install_private_record_with(
        &self,
        prepared: &PreparedPrivateRecord,
        limits: ArtifactLimits,
        boundary: impl FnMut(InstallBoundary) -> Result<(), InstallError>,
    ) -> Result<InstalledPrivateRecord, PrivateRecordError> {
        let identity = prepared.identity();
        let artifact = self
            .install(
                &private_target(identity),
                &prepared.encoded,
                limits,
                boundary,
            )
            .map_err(PrivateRecordError::Install)?;
        Ok(InstalledPrivateRecord { identity, artifact })
    }

    /// Prepare a shard only for an already authenticated source record from this store.
    pub fn prepare_authenticated_private_parent(
        &self,
        store: uuid::Uuid,
        opened: &OpenedPrivateRecord,
    ) -> Result<(), super::super::CaptureError> {
        if opened.identity.scope().store_uuid() != store {
            return Err(super::super::CaptureError::UnsafePath);
        }
        self.prepare_shard("private", opened.identity.scope().record_uuid())
    }

    /// Restore exact ciphertext from an authenticated opaque record, never raw/decoded bytes.
    pub fn install_authenticated_private_record(
        &self,
        opened: &OpenedPrivateRecord,
        limits: ArtifactLimits,
    ) -> Result<InstalledPrivateRecord, PrivateRecordError> {
        let identity = opened.identity;
        let artifact = self
            .install_immutable(&private_target(identity), &opened.encoded, limits)
            .map_err(PrivateRecordError::Install)?;
        Ok(InstalledPrivateRecord { identity, artifact })
    }

    /// Verify and authenticate the envelope selected by its trusted identity and digest.
    ///
    /// The caller associates the supplied store UUID with this guarded root.
    pub fn open_private_record(
        &self,
        identity: RecordIdentity,
        expected_sha256: &[u8; 32],
        artifact_limits: ArtifactLimits,
        sealed_limits: SealedLimits,
    ) -> Result<OpenedPrivateRecord, PrivateRecordError> {
        self.open_private_record_with(
            identity,
            expected_sha256,
            artifact_limits,
            sealed_limits,
            load_systemd_key,
        )
    }

    /// Verify the exact stored bytes before loading the key named by the envelope.
    fn open_private_record_with(
        &self,
        identity: RecordIdentity,
        expected_sha256: &[u8; 32],
        mut artifact_limits: ArtifactLimits,
        sealed_limits: SealedLimits,
        load_key: impl FnOnce(&KeyId) -> Result<SecretKey, KeyLoadError>,
    ) -> Result<OpenedPrivateRecord, PrivateRecordError> {
        let target = private_target(identity);
        let document_cap = u64::try_from(sealed_limits.max_document_bytes).unwrap_or(u64::MAX);
        artifact_limits.max_file_bytes = artifact_limits.max_file_bytes.min(document_cap);
        let artifact = self
            .read(&target, expected_sha256, artifact_limits, || {})
            .map_err(PrivateRecordError::Read)?;
        OpenedPrivateRecord::authenticate(identity, artifact.bytes(), sealed_limits, load_key)
    }
}

/// Derive the protected private filename from canonical UUIDs.
fn private_target(identity: RecordIdentity) -> ImmutableTarget {
    ImmutableTarget {
        namespace: "private",
        resource: Some(identity.scope().record_uuid().to_string()),
        leaf: format!("{}.sealed.json", identity.object_version()),
    }
}
