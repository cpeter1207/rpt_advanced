//! Serial control-plane ownership of peers, admission and reconnect intent.
use super::{Protocol, Route, decimal_identity, identity, valid_digit};
use crate::access::AccessPolicy;

/// Link setup or publication rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    /// Invalid direct ASL identity.
    Identity,
    /// Self, duplicate, retained intent, or transitive route.
    Loop,
    /// Incoming identity is unverified or current access policy denies it.
    Denied,
    /// The peer or text message does not exist or is invalid.
    Invalid,
    /// An asynchronous dial was cancelled, paused, or belongs to an old generation.
    Stale,
}

/// Independent destination-transmit and source-forwarding behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mode {
    transmit: bool,
    forward: bool,
}
impl Mode {
    /// Receive and transmit.
    pub const TRANSCEIVE: Self = Self {
        transmit: true,
        forward: true,
    };
    /// Receive and forward, without transmitting back to this peer.
    pub const MONITOR: Self = Self {
        transmit: false,
        forward: true,
    };
    /// Receive locally only.
    pub const LOCAL_MONITOR: Self = Self {
        transmit: false,
        forward: false,
    };
    /// Whether this destination receives program audio.
    pub fn transmits(self) -> bool {
        self.transmit
    }
    /// Whether this source contributes to other peers.
    pub fn forwards(self) -> bool {
        self.forward
    }
}

pub(super) struct Attached {
    pub name: String,
    pub mode: Mode,
    pub permanent: bool,
    pub ended: bool,
    pub routes: Vec<Route>,
}
struct Retry {
    name: String,
    mode: Mode,
    automatic: bool,
    paused: bool,
    due: u64,
    delay: u64,
    attempt: Option<u64>,
    blocked_topology: Option<TopologyEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TopologyEvidence(Vec<(String, char)>);

/// Owned token permits a dial outside the serialized control owner.
#[derive(Debug)]
pub struct RetryAttempt {
    name: String,
    mode: Mode,
    serial: u64,
    generation: u64,
}

/// One control-owned direct or retained link status record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    /// Exact direct remote identity.
    pub name: String,
    /// Preserved send and source-forwarding behavior.
    pub mode: Mode,
    /// Automatic permanent-link intent.
    pub permanent: bool,
    /// Transport has ended but readers have not been reclaimed.
    pub ended: bool,
    /// No published transport; retained connection intent exists.
    pub retrying: bool,
    /// Operator disconnect-all is holding this retained intent.
    pub paused: bool,
    /// Monotonic retry deadline, absent for published peers.
    pub due_ms: Option<u64>,
    /// Automatic recovery is waiting for relevant peer-advertised topology to change.
    pub topology_blocked: bool,
}
impl RetryAttempt {
    /// Destination identity for external directory and dial operations.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Original routing behavior retained over retries.
    pub fn mode(&self) -> Mode {
        self.mode
    }
}

/// Link control aggregate. Its mutating methods run only in the control owner.
pub struct LinkManager {
    pub(super) local: String,
    pub(super) peers: Vec<Attached>,
    retries: Vec<Retry>,
    pub(super) revision: u64,
    generation: u64,
    serial: u64,
}
impl LinkManager {
    /// Create an empty station link manager.
    pub fn new(local: &str) -> Result<Self, AdmissionError> {
        if crate::config::NodeId::new(local).is_err() || local.contains('\0') {
            return Err(AdmissionError::Identity);
        }
        Ok(Self {
            local: local.into(),
            peers: Vec::new(),
            retries: Vec::new(),
            revision: 1,
            generation: 1,
            serial: 0,
        })
    }
    /// Direct ended peers reserve identity; only live topology proves transit reachability.
    pub fn reaches(&self, name: &str, include_paused: bool) -> bool {
        self.peers.iter().any(|peer| {
            peer.name == name || (!peer.ended && peer.routes.iter().any(|route| route.node == name))
        }) || self
            .retries
            .iter()
            .any(|retry| retry.name == name && (include_paused || !retry.paused))
    }
    fn check(&self, name: &str, retries: bool) -> Result<(), AdmissionError> {
        if !decimal_identity(name) {
            return Err(AdmissionError::Identity);
        }
        if name == self.local
            || self.peers.iter().any(|peer| {
                peer.name == name
                    || (!peer.ended && peer.routes.iter().any(|route| route.node == name))
            })
            || (retries && self.retries.iter().any(|retry| retry.name == name))
        {
            return Err(AdmissionError::Loop);
        }
        Ok(())
    }
    /// Publish an externally prepared peer only after repeating current admission checks.
    pub fn attach(
        &mut self,
        name: &str,
        mode: Mode,
        permanent: bool,
    ) -> Result<(), AdmissionError> {
        self.check(name, true)?;
        self.publish(name, mode, permanent);
        Ok(())
    }
    fn publish(&mut self, name: &str, mode: Mode, permanent: bool) {
        self.peers.push(Attached {
            name: name.into(),
            mode,
            permanent,
            ended: false,
            routes: Vec::new(),
        });
        self.revision = self.revision.wrapping_add(1);
    }
    /// Apply directory verification and deny-first access before accepting incoming IAX.
    pub fn authorize_incoming(
        &self,
        name: &str,
        verified: bool,
        policy: &AccessPolicy,
    ) -> Result<(), AdmissionError> {
        if !policy.allows(name, verified) {
            return Err(AdmissionError::Denied);
        }
        self.check(name, true)
    }
    /// Recheck and publish an authorized incoming IAX peer.
    pub fn admit_incoming(
        &mut self,
        name: &str,
        verified: bool,
        policy: &AccessPolicy,
    ) -> Result<(), AdmissionError> {
        self.authorize_incoming(name, verified, policy)?;
        self.attach(name, Mode::TRANSCEIVE, false)
    }
    /// Recheck current inbound control permissions, independently of outbound audio intent.
    pub fn remote_digit(&self, name: &str, digit: char, policy: &AccessPolicy) -> bool {
        valid_digit(digit)
            && policy.allows(
                name,
                self.peers
                    .iter()
                    .any(|peer| peer.name == name && !peer.ended),
            )
    }
    /// Replace only a valid complete topology; return whether it proves another direct path.
    pub fn update_topology(&mut self, name: &str, bytes: &[u8]) -> Result<bool, AdmissionError> {
        let Some(Protocol::Topology(routes)) = Protocol::parse(bytes) else {
            return Err(AdmissionError::Invalid);
        };
        let looped = self.peers.iter().any(|peer| {
            peer.name != name && !peer.ended && routes.iter().any(|route| route.node == peer.name)
        });
        let changed = {
            let peer = self
                .peers
                .iter_mut()
                .find(|peer| peer.name == name)
                .ok_or(AdmissionError::Invalid)?;
            if peer.routes == routes {
                false
            } else {
                peer.routes = routes;
                true
            }
        };
        if changed {
            self.revision = self.revision.wrapping_add(1);
            self.release_changed_topology_blocks();
        }
        Ok(looped)
    }
    /// Suppress an ended peer before external reader reclamation.
    pub fn end(&mut self, name: &str) {
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.name == name) {
            peer.ended = true;
            self.revision = self.revision.wrapping_add(1);
        }
    }
    /// Reject a proven topology loop without scheduling permanent recovery into it.
    /// The direct identity remains reserved until the adapter reader has been joined.
    pub fn reject_loop(&mut self, name: &str) {
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.name == name) {
            peer.permanent = false;
        }
        self.end(name);
    }
    /// Remove an ended port after adapter quiescence; retain unexpected permanent failure.
    pub fn reclaim(&mut self, name: &str, now_ms: u64) {
        if let Some(index) = self
            .peers
            .iter()
            .position(|peer| peer.name == name && peer.ended)
        {
            let peer = self.peers.remove(index);
            if peer.permanent {
                self.retries.push(Retry {
                    name: peer.name,
                    mode: peer.mode,
                    automatic: true,
                    paused: false,
                    due: now_ms,
                    delay: 0,
                    attempt: None,
                    blocked_topology: None,
                });
            }
            self.revision = self.revision.wrapping_add(1);
        }
    }
    /// Retain the first failed permanent dial even when no channel was published.
    pub fn retain_retry(
        &mut self,
        name: &str,
        mode: Mode,
        now_ms: u64,
    ) -> Result<(), AdmissionError> {
        self.check(name, true)?;
        self.retries.push(Retry {
            name: name.into(),
            mode,
            automatic: true,
            paused: false,
            due: now_ms,
            delay: 0,
            attempt: None,
            blocked_topology: None,
        });
        Ok(())
    }
    /// Retain automatic intent after a final publication gate proves a topology loop.
    pub(crate) fn retain_topology_blocked(&mut self, name: &str, mode: Mode) {
        let evidence = self.topology_evidence(name);
        if let Some(retry) = self.retries.iter_mut().find(|retry| retry.name == name) {
            retry.mode = mode;
            retry.automatic = true;
            retry.paused = false;
            retry.attempt = None;
            retry.blocked_topology = Some(evidence);
            return;
        }
        self.retries.push(Retry {
            name: name.into(),
            mode,
            automatic: true,
            paused: false,
            due: 0,
            delay: 0,
            attempt: None,
            blocked_topology: Some(evidence),
        });
    }
    /// Acquire one due dial token, keeping external I/O outside control ownership.
    pub fn take_retry(&mut self, now_ms: u64) -> Option<RetryAttempt> {
        let retry = self.retries.iter_mut().find(|retry| {
            !retry.paused
                && retry.blocked_topology.is_none()
                && retry.attempt.is_none()
                && now_ms >= retry.due
        })?;
        self.serial = self.serial.wrapping_add(1);
        retry.attempt = Some(self.serial);
        Some(RetryAttempt {
            name: retry.name.clone(),
            mode: retry.mode,
            serial: self.serial,
            generation: self.generation,
        })
    }
    /// Final generation, cancellation, pause, and loop gate before answered-peer publication.
    pub fn publish_retry(&mut self, attempt: &RetryAttempt) -> Result<(), AdmissionError> {
        if attempt.generation != self.generation {
            return Err(AdmissionError::Stale);
        }
        let Some(index) = self.retries.iter().position(|retry| {
            retry.name == attempt.name && retry.attempt == Some(attempt.serial) && !retry.paused
        }) else {
            return Err(AdmissionError::Stale);
        };
        if let Err(error) = self.check(&attempt.name, false) {
            // Retained retry identities are already validated; only topology can
            // change admission while this serial control owner holds the token.
            self.retries[index].blocked_topology = Some(self.topology_evidence(&attempt.name));
            return Err(error);
        }
        let automatic = self.retries[index].automatic;
        self.publish(&attempt.name, attempt.mode, automatic);
        Ok(())
    }
    /// Release a dial token, retrying failures at 1, 2, 4...300 seconds.
    pub fn finish_retry(&mut self, attempt: RetryAttempt, success: bool, now_ms: u64) {
        let Some(index) = self
            .retries
            .iter()
            .position(|retry| retry.name == attempt.name && retry.attempt == Some(attempt.serial))
        else {
            return;
        };
        if self.retries[index].blocked_topology.is_some() {
            self.retries[index].attempt = None;
            return;
        }
        if success
            || attempt.generation != self.generation
            || self.peers.iter().any(|peer| peer.name == attempt.name)
            || !self.retries[index].automatic
        {
            self.retries.remove(index);
            return;
        }
        let retry = &mut self.retries[index];
        retry.attempt = None;
        if !retry.paused {
            retry.delay = retry.delay.saturating_mul(2).clamp(1000, 300000);
            retry.due = now_ms.saturating_add(retry.delay);
        }
    }
    /// Explicit cancellation invalidates an in-flight token immediately.
    pub fn cancel_retry(&mut self, name: &str) {
        self.retries.retain(|retry| retry.name != name);
    }
    /// Pause retained intent without cancelling it.
    pub fn pause_retries(&mut self) {
        for retry in &mut self.retries {
            retry.paused = true;
        }
    }
    /// Resume retained intent with immediate first attempts.
    pub fn resume_retries(&mut self, now_ms: u64) {
        for retry in &mut self.retries {
            retry.paused = false;
            retry.delay = 0;
            retry.due = now_ms;
            retry.blocked_topology = None;
        }
    }

    /// Invalidate pending asynchronous work when the owning runtime changes.
    pub fn invalidate_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        for retry in &mut self.retries {
            retry.attempt = None;
            retry.due = 0;
            retry.delay = 0;
            retry.blocked_topology = None;
        }
    }

    /// Exact permanent route ownership, including paused recovery intent.
    pub fn owns_permanent(&self, name: &str) -> bool {
        self.peers
            .iter()
            .any(|peer| peer.name == name && peer.permanent && !peer.ended)
            || self
                .retries
                .iter()
                .any(|retry| retry.name == name && retry.automatic)
    }

    /// Unpublish one temporary peer. The adapter must then stop and drain its reader.
    pub fn disconnect(&mut self, name: &str) -> bool {
        self.disconnect_mode(name, false)
    }

    /// Cancel recovery and unpublish one permanent peer, invalidating in-flight results.
    pub fn disconnect_permanent(&mut self, name: &str) -> bool {
        let count = self.retries.len();
        self.retries
            .retain(|retry| retry.name != name || !retry.automatic);
        self.disconnect_mode(name, true) || count != self.retries.len()
    }

    fn disconnect_mode(&mut self, name: &str, permanent: bool) -> bool {
        let before = self.peers.len();
        self.peers
            .retain(|peer| peer.name != name || peer.permanent != permanent);
        let changed = before != self.peers.len();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Copy status on control; no allocation or formatting occurs in audio workers.
    pub fn snapshot(&self) -> Vec<LinkStatus> {
        self.peers
            .iter()
            .map(|peer| LinkStatus {
                name: peer.name.clone(),
                mode: peer.mode,
                permanent: peer.permanent,
                ended: peer.ended,
                retrying: false,
                paused: false,
                due_ms: None,
                topology_blocked: false,
            })
            .chain(
                self.retries
                    .iter()
                    .filter(|retry| !self.peers.iter().any(|peer| peer.name == retry.name))
                    .map(|retry| LinkStatus {
                        name: retry.name.clone(),
                        mode: retry.mode,
                        permanent: retry.automatic,
                        ended: false,
                        retrying: true,
                        paused: retry.paused,
                        due_ms: retry.blocked_topology.is_none().then_some(retry.due),
                        topology_blocked: retry.blocked_topology.is_some(),
                    }),
            )
            .collect()
    }

    /// Unpublish temporary links; returned identities require external stop/drain.
    pub fn disconnect_temporary(&mut self) -> Vec<String> {
        let removed = self
            .peers
            .iter()
            .filter(|peer| !peer.permanent)
            .map(|peer| peer.name.clone())
            .collect();
        self.peers.retain(|peer| peer.permanent);
        self.revision = self.revision.wrapping_add(1);
        removed
    }

    /// Unpublish every link and pause original intent for explicit reconnect-all.
    pub fn disconnect_all(&mut self) -> Vec<String> {
        self.pause_retries();
        let mut removed = Vec::new();
        for peer in self.peers.drain(..) {
            removed.push(peer.name.clone());
            self.retries.push(Retry {
                name: peer.name,
                mode: peer.mode,
                automatic: peer.permanent,
                paused: true,
                due: 0,
                delay: 0,
                attempt: None,
                blocked_topology: None,
            });
        }
        self.revision = self.revision.wrapping_add(1);
        removed
    }

    /// Stop admissions from old dial tokens and unpublish all owned intent.
    /// The adapter drains readers before releasing returned channel identities.
    pub fn close(&mut self) -> Vec<String> {
        self.invalidate_generation();
        self.retries.clear();
        self.revision = self.revision.wrapping_add(1);
        self.peers.drain(..).map(|peer| peer.name).collect()
    }

    /// Route advisory key messages, reporting only local receiver carrier state.
    pub fn relay_key(
        &self,
        ingress: &str,
        message: &Protocol,
        local_receiving: bool,
        transitioned_ms: u64,
        now_ms: u64,
    ) -> Vec<(String, String)> {
        if !self
            .peers
            .iter()
            .any(|peer| peer.name == ingress && !peer.ended)
        {
            return Vec::new();
        }
        match message {
            Protocol::Query { requester } if requester != &self.local => {
                let age = if transitioned_ms == 0 {
                    0
                } else {
                    now_ms.saturating_sub(transitioned_ms) / 1000
                };
                let mut output = Vec::new();
                if identity(&self.local) {
                    output.push((
                        ingress.to_owned(),
                        format!(
                            "K {requester} {} {} {age}",
                            self.local,
                            u8::from(local_receiving)
                        ),
                    ));
                }
                output.extend(
                    self.eligible(ingress, requester)
                        .map(|peer| (peer.name.clone(), format!("K? * {requester} 0 0"))),
                );
                output
            }
            Protocol::Key {
                destination,
                source,
                keyed,
                age_seconds,
            } if destination != &self.local => {
                let text = format!(
                    "K {destination} {source} {} {age_seconds}",
                    u8::from(*keyed)
                );
                if let Some(peer) = self
                    .eligible(ingress, source)
                    .find(|peer| peer.name == *destination)
                {
                    return vec![(peer.name.clone(), text)];
                }
                self.eligible(ingress, source)
                    .map(|peer| (peer.name.clone(), text.clone()))
                    .collect()
            }
            _ => Vec::new(),
        }
    }
    fn eligible<'a>(
        &'a self,
        ingress: &'a str,
        source: &'a str,
    ) -> impl Iterator<Item = &'a Attached> {
        self.peers
            .iter()
            .filter(move |peer| !peer.ended && peer.name != ingress && peer.name != source)
    }

    fn topology_evidence(&self, name: &str) -> TopologyEvidence {
        let mut evidence = self
            .peers
            .iter()
            .filter(|peer| !peer.ended)
            .flat_map(|peer| {
                peer.routes
                    .iter()
                    .filter(move |route| route.node == name)
                    .map(|route| (peer.name.clone(), route.mode))
            })
            .collect::<Vec<_>>();
        evidence.sort_unstable();
        evidence.dedup();
        TopologyEvidence(evidence)
    }

    fn release_changed_topology_blocks(&mut self) {
        let released = self
            .retries
            .iter()
            .enumerate()
            .filter_map(|(index, retry)| {
                retry
                    .blocked_topology
                    .as_ref()
                    .filter(|evidence| **evidence != self.topology_evidence(&retry.name))
                    .map(|_| index)
            })
            .collect::<Vec<_>>();
        for index in released {
            let retry = &mut self.retries[index];
            retry.blocked_topology = None;
            retry.attempt = None;
            retry.delay = 0;
            retry.due = 0;
        }
    }
}
