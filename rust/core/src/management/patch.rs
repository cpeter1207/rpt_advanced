//! Atomic, resource-local RFC 6901 and RFC 6902 authoring mutations.

use std::{fmt, io};

use json_patch::{
    AddOperation, CopyOperation, MoveOperation, PatchOperation, RemoveOperation, ReplaceOperation,
};
use jsonptr::{Pointer, PointerBuf};
use num_bigint::BigInt;
use rptadv_json::{BorrowedChildren, BorrowedValue};
use serde_json::{Map, Number, Value};
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

/// Configuration for one resource's sparse authoring projection.
pub struct PatchPolicy<'a> {
    /// RFC 6901 pointers granting access to authoring subtrees.
    pub writable: &'a [&'a str],
    /// RFC 6901 pointers for excluded server-owned object members.
    ///
    /// Protected paths use stable object members, without array ancestors.
    pub server_owned: &'a [&'a str],
    /// RFC 6901 object-member pointers for existing credential owners' slots.
    ///
    /// Slots must be distinct and their parent objects must exist in the
    /// initial authoring body. Derive them from public ownership alone.
    pub secret_slots: &'a [&'a str],
}

/// Caller-selected staging limits.
#[derive(Clone, Copy)]
pub struct PatchLimits {
    /// Maximum request bytes, including whitespace and ignored members.
    pub request_bytes: usize,
    /// Maximum number of operations.
    pub operations: usize,
    /// Maximum UTF-8 bytes in each decoded path string.
    pub pointer_bytes: usize,
    /// Maximum number of tokens in each path.
    pub pointer_tokens: usize,
    /// Maximum number of bytes in a public numeric token.
    pub number_bytes: usize,
    /// Maximum serialized size of the authoring stage after each operation.
    pub stage_bytes: usize,
    /// Maximum number of levels in an authoring stage, counting the root.
    pub stage_depth: usize,
}

/// Value-free error classification for the requesting coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchErrorKind {
    /// Malformed JSON or operation envelope.
    Syntax,
    /// A caller-supplied limit was exceeded.
    Limit,
    /// The trusted authoring body or path policy is invalid.
    Authoring,
    /// An operation is outside the permitted authoring paths.
    Forbidden,
    /// A path cannot be applied to the current authoring body.
    Path,
    /// A test's value did not match.
    TestFailed,
}

/// Value-free failure with an optional zero-based operation index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchError {
    /// Failure classification.
    pub kind: PatchErrorKind,
    /// Operation index when applicable.
    pub operation: Option<usize>,
}

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "JSON Patch {:?}", self.kind)?;
        if let Some(index) = self.operation {
            write!(formatter, " at operation {index}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PatchError {}

/// Disposable public authoring stage and its companion private edits.
pub struct StagedPatch {
    /// Final sparse authoring body with write-only slots omitted.
    pub authoring: Value,
    /// Last explicit change to each touched slot, in policy slot order.
    pub private_edits: Vec<PrivateEdit>,
}

impl fmt::Debug for StagedPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedPatch")
            .field("private_edit_count", &self.private_edits.len())
            .finish_non_exhaustive()
    }
}

/// A companion credential edit for a known owner.
#[derive(Debug)]
pub struct PrivateEdit {
    /// Index in the resource policy's secret-slot list.
    pub slot: usize,
    /// Final explicit edit for the selected credential handler.
    pub change: PrivateChange,
}

/// Explicit intent for the selected credential handler.
#[derive(Debug)]
pub enum PrivateChange {
    /// Supplied JSON, pending handler validation and interpretation.
    Set(PrivateValue),
    /// Explicit slot removal.
    Remove,
}

/// Opaque JSON token with zeroizing ownership and redacted diagnostics.
pub struct PrivateValue(Zeroizing<Vec<u8>>);

impl PrivateValue {
    /// Retain a private token from a bounded, duplicate-checked create or replacement request.
    pub fn from_token(token: &BorrowedValue<'_>) -> Self {
        Self::from_raw(token.raw())
    }

    fn from_raw(raw: &str) -> Self {
        Self(Zeroizing::new(raw.as_bytes().to_vec()))
    }

    /// Borrow the supplied JSON for credential validation after profile resolution.
    pub fn as_json(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PrivateValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrivateValue(..)")
    }
}

/// Stage a complete patch against a single public resource authoring body.
///
/// Pass the sparse local authoring body with all write-only and server-owned
/// members omitted. Existing credential owners receive neutral slots only in
/// the disposable working stage. Each supplied private token borrows the
/// caller's request until success, when its final explicit edit gets a
/// zeroizing owner. The caller retains responsibility for protecting and
/// wiping the original request buffer.
///
/// The result still needs policy-specific normalization, inheritance and
/// validation. Root add and replace supply a complete authoring value. Deleting
/// the resource itself is outside this operation.
pub fn stage_patch(
    authoring: &Value,
    input: &[u8],
    policy: &PatchPolicy<'_>,
    limits: PatchLimits,
) -> Result<StagedPatch, PatchError> {
    let request_error = |kind| PatchError {
        kind,
        operation: None,
    };
    if input.len() > limits.request_bytes {
        return Err(request_error(PatchErrorKind::Limit));
    }
    let request =
        rptadv_json::decode_borrowed(input).map_err(|_| request_error(PatchErrorKind::Syntax))?;
    let BorrowedChildren::Array(raw_operations) = request.children else {
        return Err(request_error(PatchErrorKind::Syntax));
    };
    if raw_operations.len() > limits.operations {
        return Err(request_error(PatchErrorKind::Limit));
    }
    let operations = raw_operations
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            Operation::parse(raw, limits).map_err(|kind| PatchError {
                kind,
                operation: Some(index),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let policy = Paths::parse(policy).map_err(request_error)?;
    policy
        .validate_authoring(authoring)
        .map_err(request_error)?;
    check_stage(authoring, limits).map_err(request_error)?;
    let mut stage = authoring.clone();
    policy.seed_slots(&mut stage);
    check_stage(&stage, limits).map_err(request_error)?;
    let mut edits = vec![None; policy.secret_slots.len()];
    for (index, op) in operations.into_iter().enumerate() {
        op.apply(&mut stage, &policy, limits, &mut edits)
            .and_then(|()| check_stage(&stage, limits))
            .map_err(|kind| PatchError {
                kind,
                operation: Some(index),
            })?;
    }
    for slot in &policy.secret_slots {
        if let Some((parent, name)) = slot.split_back()
            && let Some(Value::Object(fields)) = stage.pointer_mut(parent.as_str())
        {
            fields.remove(name.decoded().as_ref());
        }
    }
    let private_edits = edits
        .into_iter()
        .enumerate()
        .filter_map(|(slot, edit)| {
            edit.map(|edit| PrivateEdit {
                slot,
                change: match edit {
                    PendingEdit::Set(raw) => PrivateChange::Set(PrivateValue::from_raw(raw)),
                    PendingEdit::Remove => PrivateChange::Remove,
                },
            })
        })
        .collect();
    Ok(StagedPatch {
        authoring: stage,
        private_edits,
    })
}

#[derive(Clone, Copy)]
enum PendingEdit<'a> {
    Set(&'a str),
    Remove,
}

struct Paths<'a> {
    writable: Vec<&'a Pointer>,
    server_owned: Vec<&'a Pointer>,
    secret_slots: Vec<&'a Pointer>,
}

impl<'a> Paths<'a> {
    fn parse(policy: &PatchPolicy<'a>) -> Result<Self, PatchErrorKind> {
        let parse = |paths: &'a [&'a str]| {
            paths
                .iter()
                .map(|path| Pointer::parse(path).map_err(|_| PatchErrorKind::Authoring))
                .collect::<Result<Vec<_>, _>>()
        };
        let paths = Self {
            writable: parse(policy.writable)?,
            server_owned: parse(policy.server_owned)?,
            secret_slots: parse(policy.secret_slots)?,
        };
        for (index, slot) in paths.secret_slots.iter().enumerate() {
            if slot.is_root()
                || !paths.writable_at(slot)
                || intersects(slot, &paths.server_owned)
                || intersects(slot, &paths.secret_slots[..index])
            {
                return Err(PatchErrorKind::Authoring);
            }
        }
        Ok(paths)
    }

    fn validate_authoring(&self, stage: &Value) -> Result<(), PatchErrorKind> {
        if !self.authoring_fields(stage, Pointer::root()) {
            return Err(PatchErrorKind::Authoring);
        }
        for path in &self.server_owned {
            if stage.pointer(path.as_str()).is_some() || has_array_ancestor(stage, path) {
                return Err(PatchErrorKind::Authoring);
            }
        }
        for path in &self.secret_slots {
            let parent = path.parent().expect("validated secret member");
            if stage.pointer(path.as_str()).is_some()
                || !stage.pointer(parent.as_str()).is_some_and(Value::is_object)
                || has_array_ancestor(stage, path)
            {
                return Err(PatchErrorKind::Authoring);
            }
        }
        Ok(())
    }

    fn authoring_fields(&self, stage: &Value, path: &Pointer) -> bool {
        if self.writable_at(path) {
            return true;
        }
        stage.as_object().is_some_and(|fields| {
            self.writable.iter().any(|root| root.starts_with(path))
                && fields.iter().all(|(name, child)| {
                    let mut child_path = path.to_owned();
                    child_path.push_back(name.as_str());
                    self.authoring_fields(child, &child_path)
                })
        })
    }

    fn seed_slots(&self, stage: &mut Value) {
        for path in &self.secret_slots {
            let (parent, name) = path.split_back().expect("validated secret member");
            stage
                .pointer_mut(parent.as_str())
                .and_then(Value::as_object_mut)
                .expect("validated secret parent")
                .insert(name.decoded().into_owned(), Value::Null);
        }
    }

    fn writable_at(&self, path: &Pointer) -> bool {
        self.writable.iter().any(|root| path.starts_with(root))
    }

    fn check_explicit(&self, path: &Pointer) -> Result<(), PatchErrorKind> {
        if self
            .server_owned
            .iter()
            .any(|owned| path.starts_with(owned))
            || self
                .secret_slots
                .iter()
                .any(|slot| path != *slot && path.starts_with(slot))
        {
            Err(PatchErrorKind::Forbidden)
        } else {
            Ok(())
        }
    }

    fn check_indirect(&self, path: &Pointer) -> Result<(), PatchErrorKind> {
        if self.writable_at(path)
            && !intersects(path, &self.server_owned)
            && !intersects(path, &self.secret_slots)
        {
            Ok(())
        } else {
            Err(PatchErrorKind::Forbidden)
        }
    }

    fn project<'input>(
        &self,
        value: &'input BorrowedValue<'input>,
        path: &Pointer,
        limits: PatchLimits,
        edits: &mut [Option<PendingEdit<'input>>],
    ) -> Result<Value, PatchErrorKind> {
        self.check_explicit(path)?;
        if let Some(slot) = self.secret_slots.iter().position(|slot| path == *slot) {
            edits[slot] = Some(PendingEdit::Set(value.raw()));
            return Ok(Value::Null);
        }

        // A root (or other enclosing object) can replace its writable members.
        // Each supplied child must independently fit the profile.
        let writable = self.writable_at(path)
            || (matches!(value.children, BorrowedChildren::Object(_))
                && self.writable.iter().any(|root| root.starts_with(path)));
        if !writable {
            return Err(PatchErrorKind::Forbidden);
        }
        match &value.children {
            BorrowedChildren::Object(fields) => fields
                .iter()
                .map(|(name, child)| {
                    let mut child_path = path.to_owned();
                    child_path.push_back(name.as_str());
                    Ok((
                        name.as_str().to_owned(),
                        self.project(child, &child_path, limits, edits)?,
                    ))
                })
                .collect::<Result<Map<_, _>, _>>()
                .map(Value::Object),
            // Protected members have stable object paths. Retargeting one
            // through array insertion or movement would change the owner.
            BorrowedChildren::Array(_)
                if intersects(path, &self.secret_slots) || intersects(path, &self.server_owned) =>
            {
                Err(PatchErrorKind::Forbidden)
            }
            _ => public_value(value, limits),
        }
    }
}

fn intersects(path: &Pointer, others: &[&Pointer]) -> bool {
    others
        .iter()
        .any(|other| path.starts_with(other) || other.starts_with(path))
}

fn has_array_ancestor(mut doc: &Value, path: &Pointer) -> bool {
    for token in path.tokens() {
        if doc.is_array() {
            return true;
        }
        let Some(child) = doc
            .as_object()
            .and_then(|fields| fields.get(token.decoded().as_ref()))
        else {
            return false;
        };
        doc = child;
    }
    false
}

struct Operation<'a> {
    path: PointerBuf,
    action: Action<'a>,
}

enum Action<'a> {
    Add(&'a BorrowedValue<'a>),
    Remove,
    Replace(&'a BorrowedValue<'a>),
    Move(PointerBuf),
    Copy(PointerBuf),
    Test(&'a BorrowedValue<'a>),
}

impl<'a> Operation<'a> {
    fn parse(raw: &'a BorrowedValue<'a>, limits: PatchLimits) -> Result<Self, PatchErrorKind> {
        let BorrowedChildren::Object(fields) = &raw.children else {
            return Err(PatchErrorKind::Syntax);
        };
        let field = |name| fields.get(name).ok_or(PatchErrorKind::Syntax);
        let path = parse_pointer(field("path")?, limits)?;
        let action = match string_token(field("op")?)?.as_str() {
            "add" => Action::Add(field("value")?),
            "remove" => Action::Remove,
            "replace" => Action::Replace(field("value")?),
            "move" => Action::Move(parse_pointer(field("from")?, limits)?),
            "copy" => Action::Copy(parse_pointer(field("from")?, limits)?),
            "test" => Action::Test(field("value")?),
            _ => return Err(PatchErrorKind::Syntax),
        };
        Ok(Self { path, action })
    }

    fn apply(
        self,
        stage: &mut Value,
        policy: &Paths<'_>,
        limits: PatchLimits,
        edits: &mut [Option<PendingEdit<'a>>],
    ) -> Result<(), PatchErrorKind> {
        let Self { path, action } = self;
        let operation = match action {
            Action::Add(value) => {
                let value = policy.project(value, &path, limits, edits)?;
                PatchOperation::Add(AddOperation { path, value })
            }
            Action::Replace(value) => {
                let value = policy.project(value, &path, limits, edits)?;
                PatchOperation::Replace(ReplaceOperation { path, value })
            }
            Action::Remove => {
                policy.check_explicit(&path)?;
                if !policy.writable_at(&path) || intersects(&path, &policy.server_owned) {
                    return Err(PatchErrorKind::Forbidden);
                }
                if let Some(slot) = policy.secret_slots.iter().position(|slot| &*path == *slot) {
                    edits[slot] = Some(PendingEdit::Remove);
                }
                PatchOperation::Remove(RemoveOperation { path })
            }
            Action::Move(from) => {
                policy.check_indirect(&from)?;
                policy.check_indirect(&path)?;
                if from == path && path.is_root() {
                    // A move from the root back to itself is a no-op.
                    return Ok(());
                }
                PatchOperation::Move(MoveOperation { from, path })
            }
            Action::Copy(from) => {
                policy.check_indirect(&from)?;
                policy.check_indirect(&path)?;
                PatchOperation::Copy(CopyOperation { from, path })
            }
            Action::Test(expected) => {
                policy.check_indirect(&path)?;
                let actual = stage.pointer(path.as_str()).ok_or(PatchErrorKind::Path)?;
                let expected = public_value(expected, limits)?;
                return if equal(actual, &expected) {
                    Ok(())
                } else {
                    Err(PatchErrorKind::TestFailed)
                };
            }
        };
        // Only the disposable stage is touched, so no undo stack is needed.
        // Library errors own request paths; translate them before returning.
        json_patch::patch_unsafe(stage, &[operation]).map_err(|_| PatchErrorKind::Path)
    }
}

fn string_token(value: &BorrowedValue<'_>) -> Result<String, PatchErrorKind> {
    if value.raw().starts_with('"')
        && let Value::String(text) = decode_scalar(value)?
    {
        Ok(text)
    } else {
        Err(PatchErrorKind::Syntax)
    }
}

fn parse_pointer(
    value: &BorrowedValue<'_>,
    limits: PatchLimits,
) -> Result<PointerBuf, PatchErrorKind> {
    let text = string_token(value)?;
    if text.len() > limits.pointer_bytes {
        return Err(PatchErrorKind::Limit);
    }
    let pointer = PointerBuf::parse(text).map_err(|_| PatchErrorKind::Syntax)?;
    if pointer.count() > limits.pointer_tokens {
        return Err(PatchErrorKind::Limit);
    }
    Ok(pointer)
}

fn decode_scalar(value: &BorrowedValue<'_>) -> Result<Value, PatchErrorKind> {
    rptadv_json::decode_value(value.raw().as_bytes()).map_err(|_| PatchErrorKind::Syntax)
}

fn public_value(value: &BorrowedValue<'_>, limits: PatchLimits) -> Result<Value, PatchErrorKind> {
    match &value.children {
        BorrowedChildren::Object(fields) => fields
            .iter()
            .map(|(name, child)| Ok((name.as_str().to_owned(), public_value(child, limits)?)))
            .collect::<Result<Map<_, _>, _>>()
            .map(Value::Object),
        BorrowedChildren::Array(items) => items
            .iter()
            .map(|item| public_value(item, limits))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        BorrowedChildren::Scalar => {
            let raw = value.raw();
            if matches!(raw.as_bytes().first(), Some(b'-' | b'0'..=b'9'))
                && raw.len() > limits.number_bytes
            {
                return Err(PatchErrorKind::Limit);
            }
            decode_scalar(value)
        }
    }
}

fn equal(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => a == b || decimal(a) == decimal(b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, x)| b.get(key).is_some_and(|y| equal(x, y)))
        }
        _ => actual == expected,
    }
}

// A nonzero number is its sign, significant digits, and adjusted exponent.
// Zero always has the same sign and exponent. Never scale the coefficient.
fn decimal(number: &Number) -> Option<(bool, String, BigInt)> {
    let mantissa = number.as_str();
    let (coefficient, exponent) = mantissa.split_once(['e', 'E']).unwrap_or((mantissa, "0"));
    let negative = coefficient.starts_with('-');
    let coefficient = coefficient.trim_start_matches('-');
    let (integer, fraction) = coefficient.split_once('.').unwrap_or((coefficient, ""));
    let digits = format!("{integer}{fraction}");
    let significant = digits.trim_start_matches('0').trim_end_matches('0');
    if significant.is_empty() {
        return None;
    }
    let exponent: BigInt = exponent.parse().expect("validated numeric exponent");
    let trailing_zeroes = digits.len() - digits.trim_end_matches('0').len();
    Some((
        negative,
        significant.to_owned(),
        exponent + trailing_zeroes - fraction.len(),
    ))
}

fn check_stage(stage: &Value, limits: PatchLimits) -> Result<(), PatchErrorKind> {
    let mut pending = vec![(stage, 1)];
    while let Some((value, depth)) = pending.pop() {
        if depth > limits.stage_depth {
            return Err(PatchErrorKind::Limit);
        }
        match value {
            Value::Object(fields) => pending.extend(fields.values().map(|next| (next, depth + 1))),
            Value::Array(items) => pending.extend(items.iter().map(|next| (next, depth + 1))),
            Value::Number(number) if number.as_str().len() > limits.number_bytes => {
                return Err(PatchErrorKind::Limit);
            }
            _ => {}
        }
    }
    serde_json::to_writer(ByteBudget(limits.stage_bytes), stage).map_err(|_| PatchErrorKind::Limit)
}

struct ByteBudget(usize);

impl io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or(io::ErrorKind::WriteZero)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
