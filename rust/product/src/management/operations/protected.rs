//! Write-only operation operands live only in this service boot's bounded request table.
use super::{Fault, Value, json};
use rpt_advanced_core::management::patch::{PrivateChange, PrivateEdit};
use rptadv_peer_support::sealed_records::SecretBytes;
use std::sync::Arc;
use zeroize::Zeroize;

// PHONE authentication and administration actions are source-restricted by the typed decoder.
// A hardware write_pin object is an ordinary public command operand.
const FIELDS: [&str; 3] = ["parameter", "digits", "digit"];
pub(super) const PATHS: [&str; 3] = [
    "/configuration/action/parameter",
    "/configuration/action/digits",
    "/configuration/action/digit",
];

#[derive(Clone, Default)]
pub(super) struct Operands([Option<Arc<SecretBytes>>; FIELDS.len()]);
impl Operands {
    pub fn apply(&mut self, edits: Vec<PrivateEdit>) -> Result<(), Fault> {
        for edit in edits {
            self.0[edit.slot] = match edit.change {
                PrivateChange::Remove => None,
                PrivateChange::Set(value) => {
                    let text = serde_json::from_slice::<String>(value.as_json())
                        .map_err(|_| Fault::Invalid)?;
                    let protected = SecretBytes::new(text.into_bytes());
                    if protected.expose_bytes().len() > 16384 {
                        return Err(Fault::Invalid);
                    }
                    Some(Arc::new(protected))
                }
            };
        }
        Ok(())
    }
    pub fn mask(&self) -> u8 {
        self.0
            .iter()
            .enumerate()
            .fold(0, |bits, (i, value)| bits | ((value.is_some() as u8) << i))
    }
    pub fn same(&self, other: &Self) -> bool {
        self.0.iter().zip(&other.0).all(|(left, right)| {
            left.as_ref().map(|v| v.expose_bytes()) == right.as_ref().map(|v| v.expose_bytes())
        })
    }
    pub fn with_full<T>(
        &self,
        body: &Value,
        work: impl FnOnce(&Value) -> Result<T, Fault>,
    ) -> Result<T, Fault> {
        if self.mask() == 0 {
            return work(body);
        }
        let mut transient = Transient(body.clone());
        let fields = transient
            .0
            .get_mut("action")
            .and_then(Value::as_object_mut)
            .ok_or(Fault::Invalid)?;
        for (field, value) in FIELDS.iter().zip(&self.0) {
            if let Some(value) = value {
                fields.insert(
                    (*field).into(),
                    Value::String(
                        std::str::from_utf8(value.expose_bytes())
                            .map_err(|_| Fault::Invalid)?
                            .to_owned(),
                    ),
                );
            }
        }
        work(&transient.0)
    }
}

/// Recheck a stored public summary with harmless syntactically valid stand-ins. The result
/// is for decoder/source validation only. Historical operations cannot be dispatched.
pub(super) fn validate_summary(
    body: &Value,
    mask: u8,
    actor: &super::ServiceId,
    submitted: bool,
) -> Result<(), Fault> {
    let action = &body["action"];
    if mask >= 1 << FIELDS.len() || FIELDS.iter().any(|field| action.get(*field).is_some()) {
        return Err(Fault::Storage);
    }
    if mask == 0 {
        return super::validate(body, actor, submitted);
    }
    let mut summary = body.clone();
    let name = action["action"].as_str().ok_or(Fault::Storage)?;
    let fields = summary
        .get_mut("action")
        .and_then(Value::as_object_mut)
        .ok_or(Fault::Storage)?;
    for (i, field) in FIELDS
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
    {
        let sample = match i {
            0 if name == "call_macro" => "",
            0 => "1000",
            _ => "1",
        };
        fields.insert((*field).to_string(), json!(sample));
    }
    super::validate(&summary, actor, submitted)
}

struct Transient(Value);
impl Drop for Transient {
    fn drop(&mut self) {
        if let Some(action) = self.0.get_mut("action").and_then(Value::as_object_mut) {
            for field in FIELDS {
                if let Some(Value::String(value)) = action.get_mut(field) {
                    value.zeroize();
                }
            }
        }
    }
}
