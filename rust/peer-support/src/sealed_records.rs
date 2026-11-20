//! Bounded authenticated envelopes for opaque private record bytes.

use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Map, Value};
use std::{fmt, mem};
use uuid::{Builder, Uuid};
use zeroize::Zeroizing;

mod systemd;
#[cfg(test)]
mod tests;

pub use systemd::{KeyLoadError, load_systemd_key};

const STORAGE_FORMAT_VERSION: u32 = 1;
const SEALED_FORMAT_VERSION: u32 = 1;
const MAX_KEY_ID_BYTES: usize = 64;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const PURPOSE: &[u8] = b"rpt-advanced.private-record\0";
const ENCODED_NONCE_BYTES: usize = base64::encoded_len(NONCE_BYTES, false).unwrap();

const FORMAT_1_PREFIX: &str = r#"{"format_version":1,"key_id":""#;
const NONCE_PREFIX: &str = r#"","nonce":""#;
const CIPHERTEXT_PREFIX: &str = r#"","ciphertext":""#;
const DOCUMENT_SUFFIX: &str = r#""}"#;
const DOCUMENT_OVERHEAD: usize = FORMAT_1_PREFIX.len()
    + NONCE_PREFIX.len()
    + CIPHERTEXT_PREFIX.len()
    + DOCUMENT_SUFFIX.len()
    + ENCODED_NONCE_BYTES;

/// Exact ASCII identifier for a provisioned encryption key.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyId(String);

impl KeyId {
    /// Accept 1 through 64 ASCII letters, digits, underscores or hyphens.
    pub fn new(value: &str) -> Result<Self, InvalidKeyId> {
        if !(1..=MAX_KEY_ID_BYTES).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(InvalidKeyId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrow the exact symbolic ID for key selection.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyId(..)")
    }
}

/// The selected key identifier does not match the permitted ASCII grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidKeyId;

impl fmt::Display for InvalidKeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid sealed record key identifier")
    }
}

impl std::error::Error for InvalidKeyId {}

/// Selected key with private, zeroizing material.
pub struct SecretKey {
    key_id: KeyId,
    material: Zeroizing<[u8; 32]>,
}

impl SecretKey {
    fn from_material(key_id: KeyId, material: Zeroizing<[u8; 32]>) -> Self {
        Self { key_id, material }
    }

    /// The key identifier associated with this systemd credential.
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&*self.material).into())
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

/// Opaque owned plaintext, zeroized on drop.
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    /// Take ownership of plaintext bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Borrow plaintext for internal use by the payload owner.
    pub fn expose_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(..)")
    }
}

/// Immutable store and credential-record UUIDs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RecordScope {
    store_uuid: Uuid,
    record_uuid: Uuid,
}

impl RecordScope {
    /// Parse the UUIDs supplied by the store owner.
    pub fn new(store_uuid: &str, record_uuid: &str) -> Result<Self, SealedError> {
        Ok(Self {
            store_uuid: Uuid::parse_str(store_uuid).map_err(|_| SealedError::InvalidIdentity)?,
            record_uuid: Uuid::parse_str(record_uuid).map_err(|_| SealedError::InvalidIdentity)?,
        })
    }

    /// UUID of the store that owns the credential record.
    pub fn store_uuid(&self) -> Uuid {
        self.store_uuid
    }

    /// UUID of the credential record within the store.
    pub fn record_uuid(&self) -> Uuid {
        self.record_uuid
    }
}

impl fmt::Debug for RecordScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecordScope(..)")
    }
}

/// Scope and immutable object version selected by the store owner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RecordIdentity {
    scope: RecordScope,
    object_version: Uuid,
}

impl RecordIdentity {
    /// Check the object-version UUID for readback of an existing object.
    pub fn new(scope: RecordScope, object_version: &str) -> Result<Self, SealedError> {
        Ok(Self {
            scope,
            object_version: Uuid::parse_str(object_version)
                .map_err(|_| SealedError::InvalidIdentity)?,
        })
    }

    /// Store and credential record to which this object belongs.
    pub fn scope(&self) -> &RecordScope {
        &self.scope
    }

    /// Version of this immutable object.
    pub fn object_version(&self) -> Uuid {
        self.object_version
    }
}

impl fmt::Debug for RecordIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecordIdentity(..)")
    }
}

/// Caller-supplied byte limits for one sealed record.
#[derive(Clone, Copy, Debug)]
pub struct SealedLimits {
    /// Maximum plaintext length, excluding the authentication tag.
    pub max_plaintext_bytes: usize,
    /// Maximum length of the encoded JSON document.
    pub max_document_bytes: usize,
}

/// Value-free error from the sealed-record boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealedError {
    /// An expected identity could not be parsed as UUIDs.
    InvalidIdentity,
    /// A document or plaintext exceeds its byte limit.
    SizeLimit,
    /// Malformed JSON or ambiguous object membership.
    InvalidJson,
    /// A required envelope field is absent or malformed.
    InvalidEnvelope,
    /// A well-formed format version is unsupported.
    UnsupportedFormat,
    /// Random bytes for a new version or nonce were unavailable.
    EntropyUnavailable,
    /// The cipher could not seal the plaintext.
    EncryptionFailed,
    /// The selected key or authenticated inputs do not match.
    AuthenticationFailed,
}

impl fmt::Display for SealedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidIdentity => "invalid sealed record identity",
            Self::SizeLimit => "sealed record exceeds byte limit",
            Self::InvalidJson => "invalid sealed record JSON or duplicate object member",
            Self::InvalidEnvelope => "invalid sealed record envelope",
            Self::UnsupportedFormat => "unsupported sealed record format",
            Self::EntropyUnavailable => "sealed record entropy unavailable",
            Self::EncryptionFailed => "sealed record encryption failed",
            Self::AuthenticationFailed => "sealed record authentication failed",
        })
    }
}

impl std::error::Error for SealedError {}

/// Value-free warning from decoding a sealed envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealedWarning {
    /// An unrecognized member was ignored.
    UnknownField,
}

impl fmt::Display for SealedWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ignored an unknown sealed record field")
    }
}

/// Structurally decoded envelope and its value-free warnings.
pub struct SealedResolution {
    /// The envelope that can be authenticated with its key.
    pub record: SealedRecord,
    /// An entry for each ignored member.
    pub warnings: Vec<SealedWarning>,
}

impl fmt::Debug for SealedResolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealedResolution(..)")
    }
}

/// XChaCha20-Poly1305 envelope and its externally selected identity.
pub struct SealedRecord {
    identity: RecordIdentity,
    key_id: KeyId,
    nonce: [u8; NONCE_BYTES],
    ciphertext: Vec<u8>,
}

impl fmt::Debug for SealedRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealedRecord(..)")
    }
}

/// Seal opaque bytes with a fresh UUID-v4 object version and independent nonce.
pub fn seal(
    key: &SecretKey,
    scope: &RecordScope,
    plaintext: &SecretBytes,
    limits: SealedLimits,
) -> Result<SealedRecord, SealedError> {
    let random = SystemRandom::new();
    let mut version_bytes = [0; 16];
    let mut nonce = [0; NONCE_BYTES];
    random
        .fill(&mut version_bytes)
        .and_then(|()| random.fill(&mut nonce))
        .map_err(|_| SealedError::EntropyUnavailable)?;
    let identity = RecordIdentity {
        scope: *scope,
        object_version: Builder::from_random_bytes(version_bytes).into_uuid(),
    };
    seal_with_identity(key, identity, nonce, plaintext, limits)
}

fn seal_with_identity(
    key: &SecretKey,
    identity: RecordIdentity,
    nonce: [u8; NONCE_BYTES],
    plaintext: &SecretBytes,
    limits: SealedLimits,
) -> Result<SealedRecord, SealedError> {
    let plaintext = plaintext.expose_bytes();
    let ciphertext_len = plaintext
        .len()
        .checked_add(TAG_BYTES)
        .ok_or(SealedError::SizeLimit)?;
    checked_document_len(ciphertext_len, key.key_id(), limits)?;

    // Reserve the tag space before copying plaintext into the zeroizing buffer.
    let mut ciphertext = Zeroizing::new(Vec::with_capacity(ciphertext_len));
    ciphertext.extend_from_slice(plaintext);
    key.cipher()
        .encrypt_in_place(
            (&nonce).into(),
            &associated_data(&identity, key.key_id()),
            &mut *ciphertext,
        )
        .map_err(|_| SealedError::EncryptionFailed)?;
    Ok(SealedRecord {
        identity,
        key_id: key.key_id().clone(),
        nonce,
        ciphertext: mem::take(&mut *ciphertext),
    })
}

impl SealedRecord {
    /// The identity bound by authentication.
    pub fn identity(&self) -> &RecordIdentity {
        &self.identity
    }

    /// Select the key named by the envelope.
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    /// Encode the four format-1 members in compact JSON.
    pub fn encode(&self, limits: SealedLimits) -> Result<Vec<u8>, SealedError> {
        let len = checked_document_len(self.ciphertext.len(), self.key_id(), limits)?;
        // Validated key IDs and the standard base64 alphabet require no JSON escaping.
        let mut document = String::with_capacity(len);
        document.push_str(FORMAT_1_PREFIX);
        document.push_str(self.key_id.as_str());
        document.push_str(NONCE_PREFIX);
        STANDARD_NO_PAD.encode_string(self.nonce, &mut document);
        document.push_str(CIPHERTEXT_PREFIX);
        STANDARD_NO_PAD.encode_string(&self.ciphertext, &mut document);
        document.push_str(DOCUMENT_SUFFIX);
        Ok(document.into_bytes())
    }

    /// Decode a bounded envelope using the trusted identity supplied by the store owner.
    pub fn decode(
        identity: RecordIdentity,
        document: &[u8],
        limits: SealedLimits,
    ) -> Result<SealedResolution, SealedError> {
        if document.len() > limits.max_document_bytes {
            return Err(SealedError::SizeLimit);
        }
        let value = rptadv_json::decode_value(document).map_err(|_| SealedError::InvalidJson)?;
        let Value::Object(mut fields) = value else {
            return Err(SealedError::InvalidEnvelope);
        };
        let version = fields
            .remove("format_version")
            .and_then(|value| value.as_u64())
            .ok_or(SealedError::InvalidEnvelope)?;
        if version != u64::from(SEALED_FORMAT_VERSION) {
            return Err(SealedError::UnsupportedFormat);
        }

        let key_id = KeyId::new(&required_string(&mut fields, "key_id")?)
            .map_err(|_| SealedError::InvalidEnvelope)?;
        let nonce = decode_nonce(&required_string(&mut fields, "nonce")?)?;
        let ciphertext = decode_ciphertext(
            &required_string(&mut fields, "ciphertext")?,
            &key_id,
            limits,
        )?;

        Ok(SealedResolution {
            record: Self {
                identity,
                key_id,
                nonce,
                ciphertext,
            },
            warnings: vec![SealedWarning::UnknownField; fields.len()],
        })
    }

    /// Authenticate the envelope and return opaque plaintext for payload validation.
    pub fn open(&self, key: &SecretKey, limits: SealedLimits) -> Result<SecretBytes, SealedError> {
        checked_document_len(self.ciphertext.len(), self.key_id(), limits)?;
        if key.key_id() != self.key_id() {
            return Err(SealedError::AuthenticationFailed);
        }

        let mut buffer = Zeroizing::new(self.ciphertext.clone());
        key.cipher()
            .decrypt_in_place(
                (&self.nonce).into(),
                &associated_data(self.identity(), self.key_id()),
                &mut *buffer,
            )
            .map_err(|_| SealedError::AuthenticationFailed)?;
        Ok(SecretBytes(buffer))
    }
}

fn required_string(fields: &mut Map<String, Value>, name: &str) -> Result<String, SealedError> {
    match fields.remove(name) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(SealedError::InvalidEnvelope),
    }
}

fn decode_nonce(encoded: &str) -> Result<[u8; NONCE_BYTES], SealedError> {
    if encoded.len() != ENCODED_NONCE_BYTES {
        return Err(SealedError::InvalidEnvelope);
    }
    let mut nonce = [0; NONCE_BYTES];
    STANDARD_NO_PAD
        .decode_slice(encoded, &mut nonce)
        .map_err(|_| SealedError::InvalidEnvelope)?;
    Ok(nonce)
}

fn decode_ciphertext(
    encoded: &str,
    key_id: &KeyId,
    limits: SealedLimits,
) -> Result<Vec<u8>, SealedError> {
    let tail = encoded.len() % 4;
    if tail == 1 {
        return Err(SealedError::InvalidEnvelope);
    }
    // Each complete quad yields three bytes. A final two or three symbols yield one or two.
    let len = encoded.len() / 4 * 3 + tail.saturating_sub(1);
    checked_document_len(len, key_id, limits)?;
    let mut ciphertext = vec![0; len];
    STANDARD_NO_PAD
        .decode_slice(encoded, &mut ciphertext)
        .map_err(|_| SealedError::InvalidEnvelope)?;
    Ok(ciphertext)
}

fn checked_document_len(
    ciphertext_len: usize,
    key_id: &KeyId,
    limits: SealedLimits,
) -> Result<usize, SealedError> {
    let plaintext_len = ciphertext_len
        .checked_sub(TAG_BYTES)
        .ok_or(SealedError::InvalidEnvelope)?;
    if plaintext_len > limits.max_plaintext_bytes {
        return Err(SealedError::SizeLimit);
    }

    let len = base64::encoded_len(ciphertext_len, false)
        .and_then(|len| len.checked_add(DOCUMENT_OVERHEAD + key_id.as_str().len()))
        .ok_or(SealedError::SizeLimit)?;
    if len > limits.max_document_bytes || len > isize::MAX as usize {
        return Err(SealedError::SizeLimit);
    }
    Ok(len)
}

fn associated_data(identity: &RecordIdentity, key_id: &KeyId) -> Vec<u8> {
    let id = key_id.as_str().as_bytes();
    let mut bytes = Vec::with_capacity(PURPOSE.len() + 2 * 4 + 3 * 16 + 1 + id.len());
    bytes.extend_from_slice(PURPOSE);
    bytes.extend_from_slice(&STORAGE_FORMAT_VERSION.to_be_bytes());
    bytes.extend_from_slice(&SEALED_FORMAT_VERSION.to_be_bytes());
    bytes.extend_from_slice(identity.scope.store_uuid.as_bytes());
    bytes.extend_from_slice(identity.scope.record_uuid.as_bytes());
    bytes.extend_from_slice(identity.object_version.as_bytes());
    bytes.push(id.len() as u8);
    bytes.extend_from_slice(id);
    bytes
}
