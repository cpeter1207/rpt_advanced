//! HTTP authoring profiles and credential paths, applied before domain resolution.
use super::Fault;
use rpt_advanced_core::{
    config::resources::{
        PortKey, candidate::ResourceKind as Kind, profiles::credentials::CredentialSlot as Slot,
    },
    management::{
        patch::{self, PatchErrorKind, PatchLimits, PatchPolicy, PrivateChange},
        routes::Target,
    },
};
use rptadv_peer_support::https::Method;
use serde_json::{Map, Value, json};

pub(super) const LIMITS: PatchLimits = PatchLimits {
    request_bytes: 1024 * 1024,
    operations: 512,
    pointer_bytes: 2048,
    pointer_tokens: 64,
    number_bytes: 128,
    stage_bytes: 1024 * 1024,
    stage_depth: 96,
};
const READ_ONLY: &[&str] = &[
    "/_links",
    "/_embedded",
    "/meta",
    "/id",
    "/kind",
    "/status",
    "/receipt",
    "/configuration/pin_configured",
    "/configuration/configured",
];
const EVENT: &[&str] = &[
    "/start",
    "/end",
    "/sequence_number",
    "/restart_recovery",
    "/replace_permanent",
];
const EVENT_CREATE: &[&str] = &[
    "/key",
    "/start",
    "/end",
    "/sequence_number",
    "/restart_recovery",
    "/replace_permanent",
];
pub(super) fn secrets(kind: Kind) -> &'static [(&'static str, Slot)] {
    match kind {
        Kind::Allstarlink => &[("/configuration/credentials/password", Slot::AllStarLink)],
        Kind::Echolink => &[("/configuration/credentials/password", Slot::Directory)],
        Kind::EcholinkProxy => &[("/configuration/credentials/password", Slot::Proxy)],
        Kind::PhonePatch => &[("/configuration/credentials/password", Slot::Phone)],
        Kind::Command => &[
            ("/configuration/admin/unlock_hash", Slot::Unlock),
            ("/configuration/admin/lock_hash", Slot::Lock),
        ],
        Kind::Mailbox => &[("/configuration/pin_hash", Slot::Pin)],
        _ => &[],
    }
}

pub(super) struct Edit {
    pub fields: Map<String, Value>,
    pub key: Option<PortKey>,
    pub private: Vec<(Slot, PrivateChange)>,
}

pub(super) fn stage(
    method: &Method,
    target: Target,
    kind: Kind,
    mut prior: Value,
    input: &[u8],
) -> Result<Edit, Fault> {
    let collection = matches!(target, Target::Collection(_)) && method != Method::POST;
    let creation = method == Method::POST;
    let named_creation = creation && (kind.is_named() || kind == Kind::Metadata);
    let slots = if collection { &[][..] } else { secrets(kind) };
    let paths = slots.iter().map(|(path, _)| *path).collect::<Vec<_>>();
    let writable: &[&str] = match (target, kind, collection, named_creation) {
        (_, _, true, _) => &["/configuration/members"],
        (Target::Credentials(_), _, _, _) => &paths,
        (_, Kind::Event, _, true) => EVENT_CREATE,
        (_, Kind::Event, _, false) => EVENT,
        (_, _, _, true) => &["/key", "/configuration"],
        _ => &["/configuration"],
    };
    let policy = PatchPolicy {
        writable,
        server_owned: READ_ONLY,
        secret_slots: &paths,
    };
    let admin_policy: patch::IgnoreMalformed = |path, value| {
        path.as_str() == "/configuration/admin"
            && !crate::configuration_private::has_admin_hash(value)
    };
    let allow_array = (kind == Kind::Command && !matches!(target, Target::Credentials(_)))
        .then_some(admin_policy);
    let staged = if method == Method::PATCH {
        // A neutral parent exists in the internal view for every public owner.
        // Unspecified private state never determines path success.
        for path in &paths {
            let (parent, _) = path.rsplit_once('/').expect("credential field");
            if prior.pointer(parent).is_none() {
                let (ancestor, key) = parent.rsplit_once('/').expect("credential parent");
                let obj = prior
                    .pointer_mut(ancestor)
                    .and_then(Value::as_object_mut)
                    .ok_or(Fault::Invalid)?;
                obj.insert(key.to_owned(), json!({}));
            }
        }
        if let Some(allowed) = allow_array {
            patch::stage_profile_patch(&prior, input, &policy, LIMITS, allowed)
        } else {
            patch::stage_patch(&prior, input, &policy, LIMITS)
        }
    } else {
        patch::stage_profile_replacement(input, &policy, LIMITS, allow_array)
    }
    .map_err(|e| match e.kind {
        PatchErrorKind::Syntax => Fault::Malformed,
        PatchErrorKind::Limit => Fault::TooLarge,
        PatchErrorKind::TestFailed => Fault::Conflict,
        _ => Fault::Invalid,
    })?;
    let Value::Object(mut body) = staged.authoring else {
        return Err(Fault::Invalid);
    };
    let key = body
        .remove("key")
        .map(|v| match v {
            Value::String(s) => Ok(PortKey::new(s)),
            _ => Err(Fault::Invalid),
        })
        .transpose()?;
    let mut fields = if kind == Kind::Event && !collection {
        body
    } else {
        match body.remove("configuration") {
            Some(Value::Object(fields)) => fields,
            _ => return Err(Fault::Invalid),
        }
    };
    if !slots.is_empty()
        && matches!(
            kind,
            Kind::Allstarlink | Kind::Echolink | Kind::PhonePatch | Kind::EcholinkProxy
        )
        && let Some(container) = fields.remove("credentials")
        && !container.as_object().is_some_and(Map::is_empty)
    {
        return Err(Fault::Invalid);
    }
    let private = staged
        .private_edits
        .into_iter()
        .map(|e| (slots[e.slot].1, e.change))
        .collect();
    Ok(Edit {
        fields,
        key,
        private,
    })
}
