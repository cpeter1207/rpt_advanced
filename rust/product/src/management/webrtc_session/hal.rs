//! Typed WebRTC request and private HAL representation helpers.
use super::{Direction, Fault, Id, MAX_CANDIDATE_BYTES, MAX_SDP_BYTES, State, WorkKey, bounded};
use crate::configuration_private::decode_secret_json_string;
use crate::management::Api;
use rptadv_json::{BorrowedChildren, BorrowedKey, BorrowedValue, decode_borrowed};
use rptadv_peer_support::sealed_records::SecretBytes;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

type Fields<'a> = BTreeMap<BorrowedKey, BorrowedValue<'a>>;

fn fields(input: &[u8]) -> Result<Fields<'_>, Fault> {
    match decode_borrowed(input)
        .map_err(|_| Fault::Malformed)?
        .children
    {
        BorrowedChildren::Object(fields) => Ok(fields),
        _ => Err(Fault::Invalid),
    }
}

fn token<'a>(fields: &'a Fields<'_>, name: &str) -> Result<&'a str, Fault> {
    fields
        .get(name)
        .map(BorrowedValue::raw)
        .ok_or(Fault::Invalid)
}

fn exact(fields: &Fields<'_>, expected: &[&str]) -> Result<(), Fault> {
    if fields.len() != expected.len() || expected.iter().any(|key| !fields.contains_key(*key)) {
        return Err(Fault::Invalid);
    }
    Ok(())
}

fn secret(value: &str, maximum: usize) -> Result<SecretBytes, Fault> {
    let bytes = decode_secret_json_string(value.as_bytes()).map_err(|_| Fault::Invalid)?;
    bounded(&bytes, maximum)?;
    Ok(bytes)
}

fn generation(value: &str) -> Result<u64, Fault> {
    let generation = value.parse::<u64>().map_err(|_| Fault::Invalid)?;
    if generation == 0 || generation.to_string() != value {
        return Err(Fault::Invalid);
    }
    Ok(generation)
}

fn body_generation(fields: &Fields<'_>) -> Result<u64, Fault> {
    let text: String =
        serde_json::from_str(token(fields, "ice_generation")?).map_err(|_| Fault::Invalid)?;
    generation(&text)
}

pub(in crate::management) struct Offer {
    pub(in crate::management) direction: Direction,
    pub(in crate::management) sdp: SecretBytes,
}

pub(in crate::management) fn offer(input: &[u8]) -> Result<Offer, Fault> {
    let fields = fields(input)?;
    exact(&fields, &["direction", "offer"])?;
    let direction =
        serde_json::from_str(token(&fields, "direction")?).map_err(|_| Fault::Invalid)?;
    let sdp = secret(token(&fields, "offer")?, MAX_SDP_BYTES)?;
    Ok(Offer { direction, sdp })
}

pub(in crate::management) enum Update {
    Renew {
        ice_generation: u64,
    },
    Restart {
        ice_generation: u64,
        sdp: SecretBytes,
    },
}

pub(in crate::management) fn update(input: &[u8]) -> Result<Update, Fault> {
    let fields = fields(input)?;
    let ice_generation = body_generation(&fields)?;
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Action {
        Renew,
        Restart,
    }
    let action = serde_json::from_str(token(&fields, "action")?).map_err(|_| Fault::Invalid)?;
    match action {
        Action::Renew => {
            exact(&fields, &["action", "ice_generation"])?;
            Ok(Update::Renew { ice_generation })
        }
        Action::Restart => {
            exact(&fields, &["action", "ice_generation", "offer"])?;
            let sdp = secret(token(&fields, "offer")?, MAX_SDP_BYTES)?;
            Ok(Update::Restart {
                ice_generation,
                sdp,
            })
        }
    }
}

pub(in crate::management) struct Candidate {
    pub(in crate::management) ice_generation: u64,
    pub(in crate::management) sequence: usize,
    pub(in crate::management) contents: Option<SecretBytes>,
}

pub(in crate::management) fn candidate(input: &[u8]) -> Result<Candidate, Fault> {
    let fields = fields(input)?;
    exact(&fields, &["ice_generation", "sequence", "candidate"])?;
    let ice_generation = body_generation(&fields)?;
    let sequence = serde_json::from_str(token(&fields, "sequence")?).map_err(|_| Fault::Invalid)?;
    let raw = token(&fields, "candidate")?;
    let contents = if raw == "null" {
        None
    } else {
        Some(secret(raw, MAX_CANDIDATE_BYTES)?)
    };
    Ok(Candidate {
        ice_generation,
        sequence,
        contents,
    })
}

pub(in crate::management) fn close_generation(query: Option<&str>) -> Result<u64, Fault> {
    let raw = query
        .and_then(|query| query.strip_prefix("ice_generation="))
        .ok_or(Fault::Invalid)?;
    generation(raw)
}

const SESSIONS: &str = "webrtc-sessions";
const SESSION: &str = "webrtc-session";
const CANDIDATES: &str = "webrtc-candidates";

fn collection(port: Id) -> String {
    format!("/webrtc/ports/{port}/sessions/")
}

fn session(boot: Id, key: WorkKey) -> String {
    format!("{}{boot}/{}/", collection(key.port), key.session)
}

pub(in crate::management) fn collection_link(api: &Api, port: Id) -> Value {
    api.href(&collection(port))
}

pub(in crate::management) fn session_location(api: &Api, boot: Id, key: WorkKey) -> String {
    format!("{}{}", api.mount, session(boot, key))
}

#[derive(Serialize)]
pub(in crate::management) struct Session<'a> {
    #[serde(rename = "_links")]
    links: Value,
    session_id: String,
    port_id: String,
    ice_generation: String,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer: Option<&'a str>,
}

/// Call only with state returned by the owner-checked book. The SDP is
/// borrowed directly into the owner response serializer.
pub(in crate::management) fn session_document<'a>(
    api: &Api,
    boot: Id,
    key: WorkKey,
    state: &'a State,
) -> Result<Session<'a>, Fault> {
    let path = session(boot, key);
    let (phase, answer) = match state {
        State::Pending(answer) => ("pending", answer.as_ref()),
        State::Active(answer) => ("active", Some(answer)),
        State::Closed => ("closed", None),
    };
    let answer = answer
        .map(|answer| std::str::from_utf8(answer.expose_bytes()).map_err(|_| Fault::Unavailable))
        .transpose()?;
    let mut links = api.links(&path, SESSION);
    links["up"] = collection_link(api, key.port);
    if !matches!(state, State::Closed) {
        links["rpt:webrtc-candidates"] = api.href(&format!("{path}candidates/"));
        links["rpt:close"] = api.href(&format!("{path}?ice_generation={}", key.ice_generation));
    }
    Ok(Session {
        links,
        session_id: key.session.to_string(),
        port_id: key.port.to_string(),
        ice_generation: key.ice_generation.to_string(),
        phase,
        answer,
    })
}

#[derive(Serialize)]
pub(in crate::management) struct LocalCandidate<'a> {
    pub(in crate::management) sequence: usize,
    pub(in crate::management) candidate: Option<&'a str>,
}

#[derive(Serialize)]
pub(in crate::management) struct CandidateDocument<'a> {
    #[serde(rename = "_links")]
    links: Value,
    ice_generation: String,
    candidates: &'a [LocalCandidate<'a>],
}

/// The transport gives the current generation's ordered local candidate view.
pub(in crate::management) fn candidate_document<'a>(
    api: &Api,
    boot: Id,
    key: WorkKey,
    candidates: &'a [LocalCandidate<'a>],
) -> CandidateDocument<'a> {
    let parent = session(boot, key);
    let mut links = api.links(&format!("{parent}candidates/"), CANDIDATES);
    links["up"] = api.href(&parent);
    CandidateDocument {
        links,
        ice_generation: key.ice_generation.to_string(),
        candidates,
    }
}

pub(in crate::management) fn collection_document(api: &Api, port: Id) -> Value {
    let mut links = api.links(&collection(port), SESSIONS);
    links["up"] = api.href(&format!("/status/ports/{port}/"));
    serde_json::json!({"_links": links})
}
