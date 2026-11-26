//! Whole-candidate DTMF collection on serialized control, before domain admission.
use super::{DigitEvent, DigitOutcome};
use crate::{
    command_language::{CommandLanguage, ProtectedOperand},
    config::resources::candidate::ResourceScope,
    link::PeerReply,
};
use std::sync::Arc;
use zeroize::{Zeroize, Zeroizing};

/// Shared immutable settings and exact registry owner installed by the node.
pub(crate) struct CommandProfile {
    pub owner: ResourceScope,
    pub public_key: crate::config::resources::PortKey,
    pub network_node: Option<String>,
    pub route_generation: Option<u64>,
    pub language: Arc<CommandLanguage>,
}

/// A consumed buffer and its originating connection. Raw symbols remain protected through queues.
#[derive(Clone)]
pub struct MappedInput {
    pub(crate) profile: Arc<CommandProfile>,
    pub(crate) symbols: ProtectedOperand<String>,
    _collector: Arc<()>,
    consumed: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) connection: Option<(String, PeerReply)>,
    pub(crate) generation: u64,
}
impl MappedInput {
    /// Whether this completed command came from the port's own receiver.
    pub fn local_receiver(&self) -> bool {
        self.connection.is_none()
    }

    /// Borrow the exact private recipient without exposing the collected command symbols.
    pub fn phone_reply(&self) -> Option<&PeerReply> {
        self.connection
            .as_ref()
            .map(|(_, reply)| reply)
            .filter(|reply| reply.phone_source().is_some())
    }

    pub(crate) fn claim(&self) -> bool {
        !self
            .consumed
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    }
}
impl std::fmt::Debug for MappedInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedInput")
            .field("owner", &self.profile.owner)
            .finish_non_exhaustive()
    }
}
impl PartialEq for MappedInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.profile, &other.profile)
            && self.generation == other.generation
            && self.connection == other.connection
            && self.symbols == other.symbols
    }
}
impl Eq for MappedInput {}

pub(crate) struct MappedCollector {
    pub profile: Arc<CommandProfile>,
    pub generation: u64,
    lifetime: Arc<()>,
    buffer: Zeroizing<String>,
    deadline: Option<u64>,
    overflow: bool,
}
impl MappedCollector {
    pub fn new(profile: Arc<CommandProfile>, generation: u64) -> Self {
        Self {
            profile,
            generation,
            lifetime: Arc::new(()),
            buffer: Zeroizing::new(String::new()),
            deadline: None,
            overflow: false,
        }
    }
    pub(super) fn pin(&self) -> Arc<()> {
        self.lifetime.clone()
    }
    pub fn peer_unkey(&self) -> bool {
        self.profile
            .language
            .match_completed(&self.buffer)
            .is_some_and(|m| m.mapping.radio())
    }
    pub fn pending(&self) -> bool {
        self.active() || Arc::strong_count(&self.lifetime) > 1
    }
    pub fn active(&self) -> bool {
        self.deadline.is_some()
    }
    fn finish(&mut self, grant: bool) -> Option<DigitOutcome> {
        self.deadline = None;
        if std::mem::take(&mut self.overflow)
            || self
                .profile
                .language
                .match_completed(&self.buffer)
                .is_none_or(|matched| matched.mapping.protected && !grant)
        {
            self.buffer.zeroize();
            return None;
        }
        Some(DigitOutcome::Mapped(MappedInput {
            profile: self.profile.clone(),
            generation: self.generation,
            _collector: self.lifetime.clone(),
            symbols: std::mem::take(&mut *self.buffer).into(),
            connection: None,
            consumed: Arc::new(false.into()),
        }))
    }
    pub fn feed(&mut self, event: DigitEvent, grant: bool) -> Option<DigitOutcome> {
        match event {
            DigitEvent::Lost => {
                self.buffer.zeroize();
                self.deadline = None;
                self.overflow = false;
                None
            }
            DigitEvent::Unkey { .. } => self.finish(grant),
            DigitEvent::Digit { digit, .. } if digit == self.profile.language.terminator => {
                self.finish(grant)
            }
            DigitEvent::Digit { digit, now_ms } => {
                let expired = self.deadline.is_some_and(|due| now_ms >= due);
                let completed = if expired { self.finish(grant) } else { None };
                if crate::link::valid_digit(digit) {
                    self.deadline =
                        Some(now_ms.saturating_add(self.profile.language.inactivity_timeout_ms));
                    if !self.overflow {
                        if self.buffer.len() == self.profile.language.maximum_digits {
                            self.buffer.zeroize();
                            self.overflow = true;
                        } else {
                            // Reserve the complete bound before storing any sensitive symbols.
                            if self.buffer.capacity() == 0 {
                                self.buffer
                                    .reserve_exact(self.profile.language.maximum_digits);
                            }
                            self.buffer.push(digit);
                        }
                    }
                }
                completed
            }
        }
    }
}
