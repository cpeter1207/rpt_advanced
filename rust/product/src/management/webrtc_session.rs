//! Boot-local WebRTC signaling and media admission ownership.
use super::{Fault, Id, Observed, fresh};
use rptadv_peer_support::{
    https::BrowserPrincipal, sealed_records::SecretBytes, working_files::immutable::artifact_sha256,
};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicU64},
    time::Instant,
};

const MAX_SDP_BYTES: usize = 64 * 1024;
const MAX_CANDIDATE_BYTES: usize = 4 * 1024;
const MAX_CANDIDATES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    ReceiveOnly,
    Transceive,
}

/// Per-request identity and the active station view from the existing coordinator.
#[derive(Clone, Copy)]
pub(crate) struct Access<'a> {
    pub(crate) principal: &'a BrowserPrincipal,
    pub(crate) active: &'a Observed,
    pub(crate) now: Instant,
}

/// Fresh media authorization from the existing ingress and port policy.
#[derive(Clone, Copy)]
pub(crate) struct Approval {
    pub(crate) direction: Direction,
    pub(crate) until: Instant,
}

impl Approval {
    fn permits(self, direction: Direction, now: Instant) -> bool {
        now < self.until
            && (direction == Direction::ReceiveOnly || self.direction == Direction::Transceive)
    }
}

/// Internal key retained on provider work. The port and ICE generation fence each callback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct WorkKey {
    pub(crate) session: Id,
    pub(crate) port: Id,
    pub(crate) ice_generation: u64,
}

pub(crate) struct OfferWork {
    pub(crate) key: WorkKey,
    pub(crate) binding: Id,
    pub(crate) epochs: Arc<AtomicU64>,
    pub(crate) direction: Direction,
    pub(crate) sdp: SecretBytes,
}

#[derive(Clone, Copy)]
pub(crate) struct CandidateKey {
    pub(crate) offer: WorkKey,
    pub(crate) sequence: usize,
}

pub(crate) struct CandidateWork {
    pub(crate) key: CandidateKey,
    pub(crate) contents: Option<SecretBytes>,
}

/// Nonblocking Product media owner called by the serialized management poll.
pub(crate) trait Transport: Send {
    fn approve(
        &mut self,
        principal: &BrowserPrincipal,
        port: Id,
        direction: Direction,
        revision: &str,
        station_generation: u64,
        now: Instant,
    ) -> Result<Approval, Fault>;

    fn offer(
        &mut self,
        work: OfferWork,
        revision: &str,
        station_generation: u64,
        until: Instant,
    ) -> Result<(), Fault>;

    /// Replace ICE credentials on the same authorized session and revoke its old media.
    fn restart(&mut self, previous: WorkKey, work: OfferWork, until: Instant) -> Result<(), Fault>;

    fn candidate(&mut self, work: CandidateWork) -> Result<(), Fault>;

    fn renew(&mut self, key: WorkKey, until: Instant) -> Result<(), Fault>;

    /// Revoke media before releasing the transport. An exact close may be repeated.
    fn close(&mut self, key: WorkKey);

    fn next_receipt(&mut self) -> Option<Receipt>;
}

/// A provider result is fenced by the same port, session and ICE work identity.
pub(crate) enum Receipt {
    Answer {
        key: WorkKey,
        sdp: SecretBytes,
    },
    LocalCandidate {
        key: WorkKey,
        sequence: usize,
        contents: Option<SecretBytes>,
    },
    CandidateAccepted(CandidateKey),
    Attached(WorkKey),
    Closed(WorkKey),
}

pub(crate) enum State {
    Pending(Option<SecretBytes>),
    Active(SecretBytes),
    Closed,
}

struct Session {
    principal: BrowserPrincipal,
    key: WorkKey,
    binding: Id,
    epochs: Arc<AtomicU64>,
    station_generation: u64,
    revision: String,
    direction: Direction,
    until: Instant,
    state: State,
    // Only fingerprints survive the handoff. None is the end-of-candidates marker.
    candidates: Vec<Option<[u8; 32]>>,
    acknowledged: usize,
    local_candidates: Vec<Option<SecretBytes>>,
    retired: bool,
}

impl Session {
    fn check_key(&self, key: WorkKey) -> Result<(), Fault> {
        if self.key.port != key.port {
            return Err(Fault::Forbidden);
        }
        if self.key.ice_generation != key.ice_generation {
            return Err(Fault::Conflict);
        }
        Ok(())
    }

    fn check_live(&mut self, active: &Observed, now: Instant) -> Result<(), Fault> {
        if now >= self.until
            || !active.status_ready
            || active.active_revision.as_deref() != Some(self.revision.as_str())
            || active.owners.get(&self.key.port) != Some(&self.station_generation)
        {
            self.close();
        }
        if matches!(self.state, State::Closed) {
            return Err(Fault::Gone);
        }
        Ok(())
    }

    fn require_approval(&mut self, approval: Approval, now: Instant) -> Result<(), Fault> {
        if !approval.permits(self.direction, now) {
            self.close();
            return Err(Fault::Forbidden);
        }
        Ok(())
    }

    fn clear_candidates(&mut self) {
        self.candidates = Vec::new();
        self.acknowledged = 0;
        self.local_candidates = Vec::new();
    }

    fn close(&mut self) {
        self.state = State::Closed;
        self.clear_candidates();
    }
}

/// Runtime sessions use a fallible reservation for each new owner.
#[derive(Default)]
pub(crate) struct Book(HashMap<Id, Session>);

impl Book {
    pub(crate) fn offer(
        &mut self,
        access: Access<'_>,
        port: Id,
        direction: Direction,
        approval: Approval,
        sdp: SecretBytes,
    ) -> Result<OfferWork, Fault> {
        if !approval.permits(direction, access.now) {
            return Err(Fault::Forbidden);
        }
        bounded(&sdp, MAX_SDP_BYTES)?;
        let active = access.active;
        let revision = active.active_revision.as_ref().ok_or(Fault::Unavailable)?;
        if !active.status_ready {
            return Err(Fault::Unavailable);
        }
        let station_generation = *active.owners.get(&port).ok_or(Fault::Unavailable)?;
        let session = fresh().map_err(|_| Fault::Unavailable)?;
        let binding = fresh().map_err(|_| Fault::Unavailable)?;
        let epochs = Arc::new(AtomicU64::new(0));
        self.0.try_reserve(1).map_err(|_| Fault::Unavailable)?;
        let key = WorkKey {
            session,
            port,
            ice_generation: 1,
        };
        let std::collections::hash_map::Entry::Vacant(entry) = self.0.entry(session) else {
            return Err(Fault::Unavailable);
        };
        entry.insert(Session {
            principal: access.principal.clone(),
            key,
            binding,
            epochs: Arc::clone(&epochs),
            station_generation,
            revision: revision.clone(),
            direction,
            until: approval.until,
            state: State::Pending(None),
            candidates: Vec::new(),
            acknowledged: 0,
            local_candidates: Vec::new(),
            retired: false,
        });
        Ok(OfferWork {
            key,
            binding,
            epochs,
            direction,
            sdp,
        })
    }

    fn owned_port(
        &mut self,
        access: Access<'_>,
        port: Id,
        session: Id,
    ) -> Result<&mut Session, Fault> {
        let row = self.0.get_mut(&session).ok_or(Fault::Missing)?;
        if &row.principal != access.principal || row.key.port != port {
            return Err(Fault::Forbidden);
        }
        Ok(row)
    }

    fn owned(&mut self, access: Access<'_>, key: WorkKey) -> Result<&mut Session, Fault> {
        let row = self.owned_port(access, key.port, key.session)?;
        row.check_key(key)?;
        Ok(row)
    }

    /// Read discovery follows the current generation, while writes carry an exact key.
    pub(crate) fn current_key(
        &mut self,
        access: Access<'_>,
        port: Id,
        session: Id,
    ) -> Result<WorkKey, Fault> {
        Ok(self.owned_port(access, port, session)?.key)
    }

    fn provider(
        &mut self,
        key: WorkKey,
        active: &Observed,
        now: Instant,
    ) -> Result<&mut Session, Fault> {
        let row = self.0.get_mut(&key.session).ok_or(Fault::Missing)?;
        row.check_key(key)?;
        row.check_live(active, now)?;
        Ok(row)
    }

    fn direction(&mut self, access: Access<'_>, key: WorkKey) -> Result<Direction, Fault> {
        let row = self.owned(access, key)?;
        row.check_live(access.active, access.now)?;
        Ok(row.direction)
    }

    pub(crate) fn state(&mut self, access: Access<'_>, key: WorkKey) -> Result<&State, Fault> {
        let row = self.owned(access, key)?;
        let _ = row.check_live(access.active, access.now);
        Ok(&row.state)
    }

    /// The browser needs its answer before finishing ICE and media attachment.
    pub(crate) fn answer_ready(
        &mut self,
        key: WorkKey,
        active: &Observed,
        now: Instant,
        answer: SecretBytes,
    ) -> Result<(), Fault> {
        let row = self.provider(key, active, now)?;
        signaling_text(&answer, MAX_SDP_BYTES)?;
        match &mut row.state {
            State::Pending(slot @ None) => *slot = Some(answer),
            State::Pending(Some(previous)) | State::Active(previous)
                if previous.expose_bytes() == answer.expose_bytes() => {}
            _ => return Err(Fault::Conflict),
        }
        Ok(())
    }

    /// Only the exact acknowledged media attachment advances the session to active.
    pub(crate) fn activate(
        &mut self,
        key: WorkKey,
        active: &Observed,
        now: Instant,
    ) -> Result<(), Fault> {
        let row = self.provider(key, active, now)?;
        match &mut row.state {
            State::Pending(answer) => {
                row.state = State::Active(answer.take().ok_or(Fault::Conflict)?);
            }
            State::Active(_) => {}
            State::Closed => return Err(Fault::Gone),
        }
        Ok(())
    }

    pub(crate) fn candidate(
        &mut self,
        access: Access<'_>,
        key: WorkKey,
        sequence: usize,
        contents: Option<SecretBytes>,
    ) -> Result<Option<CandidateWork>, Fault> {
        let row = self.owned(access, key)?;
        row.check_live(access.active, access.now)?;
        let fingerprint = if let Some(bytes) = &contents {
            bounded(bytes, MAX_CANDIDATE_BYTES)?;
            Some(artifact_sha256(bytes.expose_bytes()))
        } else {
            None
        };
        if let Some(previous) = row.candidates.get(sequence) {
            return (*previous == fingerprint)
                .then_some(None)
                .ok_or(Fault::Conflict);
        }
        if sequence != row.candidates.len() || row.candidates.last() == Some(&None) {
            return Err(Fault::Conflict);
        }
        if row.candidates.len() == MAX_CANDIDATES {
            return Err(Fault::TooLarge);
        }
        row.candidates
            .try_reserve(1)
            .map_err(|_| Fault::Unavailable)?;
        row.candidates.push(fingerprint);
        Ok(Some(CandidateWork {
            key: CandidateKey {
                offer: key,
                sequence,
            },
            contents,
        }))
    }

    fn local_candidate(
        &mut self,
        key: WorkKey,
        active: &Observed,
        now: Instant,
        sequence: usize,
        candidate: Option<SecretBytes>,
    ) -> Result<(), Fault> {
        let row = self.provider(key, active, now)?;
        if let Some(bytes) = &candidate {
            signaling_text(bytes, MAX_CANDIDATE_BYTES)?;
        }
        if let Some(previous) = row.local_candidates.get(sequence) {
            let previous = previous.as_ref().map(SecretBytes::expose_bytes);
            let submitted = candidate.as_ref().map(SecretBytes::expose_bytes);
            return (previous == submitted).then_some(()).ok_or(Fault::Conflict);
        }
        if sequence != row.local_candidates.len()
            || row.local_candidates.last().is_some_and(Option::is_none)
        {
            return Err(Fault::Conflict);
        }
        if row.local_candidates.len() == MAX_CANDIDATES {
            return Err(Fault::TooLarge);
        }
        row.local_candidates
            .try_reserve(1)
            .map_err(|_| Fault::Unavailable)?;
        row.local_candidates.push(candidate);
        Ok(())
    }

    fn local_candidates(
        &mut self,
        access: Access<'_>,
        key: WorkKey,
    ) -> Result<&[Option<SecretBytes>], Fault> {
        let row = self.owned(access, key)?;
        row.check_live(access.active, access.now)?;
        Ok(&row.local_candidates)
    }

    /// The provider acknowledges each accepted candidate in submission order.
    pub(crate) fn acknowledge_candidate(
        &mut self,
        key: CandidateKey,
        active: &Observed,
        now: Instant,
    ) -> Result<(), Fault> {
        let row = self.provider(key.offer, active, now)?;
        if key.sequence >= row.candidates.len() || key.sequence > row.acknowledged {
            return Err(Fault::Conflict);
        }
        row.acknowledged = row.acknowledged.max(key.sequence + 1);
        Ok(())
    }

    pub(crate) fn renew(
        &mut self,
        access: Access<'_>,
        key: WorkKey,
        approval: Approval,
    ) -> Result<(), Fault> {
        let row = self.owned(access, key)?;
        row.check_live(access.active, access.now)?;
        row.require_approval(approval, access.now)?;
        row.until = approval.until;
        Ok(())
    }

    pub(crate) fn restart(
        &mut self,
        access: Access<'_>,
        key: WorkKey,
        approval: Approval,
        sdp: SecretBytes,
    ) -> Result<OfferWork, Fault> {
        let row = self.owned(access, key)?;
        row.check_live(access.active, access.now)?;
        row.require_approval(approval, access.now)?;
        bounded(&sdp, MAX_SDP_BYTES)?;
        row.key.ice_generation = row
            .key
            .ice_generation
            .checked_add(1)
            .ok_or(Fault::Unavailable)?;
        row.until = approval.until;
        row.state = State::Pending(None);
        row.clear_candidates();
        Ok(OfferWork {
            key: row.key,
            binding: row.binding,
            epochs: Arc::clone(&row.epochs),
            direction: row.direction,
            sdp,
        })
    }

    pub(crate) fn close(&mut self, access: Access<'_>, key: WorkKey) -> Result<(), Fault> {
        self.owned(access, key)?.close();
        Ok(())
    }

    pub(crate) fn revoke(&mut self, key: WorkKey) -> Result<(), Fault> {
        let row = self.0.get_mut(&key.session).ok_or(Fault::Missing)?;
        row.check_key(key)?;
        row.close();
        Ok(())
    }

    /// Revoke stale rows and discard sealed-off receipts when their local lifetime ends.
    pub(crate) fn sweep(
        &mut self,
        active: &Observed,
        now: Instant,
        mut retire: impl FnMut(WorkKey),
    ) {
        self.0.retain(|_, row| {
            let _ = row.check_live(active, now);
            if matches!(row.state, State::Closed) && !std::mem::replace(&mut row.retired, true) {
                retire(row.key);
            }
            now < row.until
        });
    }
}

fn bounded(bytes: &SecretBytes, maximum: usize) -> Result<(), Fault> {
    let length = bytes.expose_bytes().len();
    if length == 0 {
        return Err(Fault::Invalid);
    }
    if length > maximum {
        return Err(Fault::TooLarge);
    }
    Ok(())
}

fn signaling_text(bytes: &SecretBytes, maximum: usize) -> Result<&str, Fault> {
    bounded(bytes, maximum)?;
    std::str::from_utf8(bytes.expose_bytes()).map_err(|_| Fault::Invalid)
}

mod coordinator;
pub(super) use coordinator::Signaling;
mod hal;
#[cfg(test)]
mod tests;
pub(crate) mod transport;
