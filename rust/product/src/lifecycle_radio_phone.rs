//! Local RF telephone admission reuses existing serialized control and call owners.
use super::*;

pub(super) fn action(action: LinkAction) -> bool {
    matches!(action, LinkAction::PhoneDial | LinkAction::PhoneHangup)
}

impl Engine {
    /// Admit source, configuration and exact call in one serialized revision turn.
    pub(super) fn radio_phone(
        self: &Arc<Self>,
        local: String,
        operation: DigitOperation,
        revision: u64,
        dtmf_source: bool,
    ) -> Result<(), RuntimeError> {
        let call = self.enter()?;
        let owner = Arc::clone(self);
        let available = self.services.phone_dial_available();
        let prepared = self.run(move |host| {
            if !owner.current(revision) {
                return Err(RuntimeError::Rejected);
            }
            // Queue time consumes macro validity; capture after serialized admission.
            let clock = owner.clock();
            let LinkEffect::Phone(source) =
                host.runtime.command(&local, operation, clock, false)?
            else {
                return Err(RuntimeError::Rejected);
            };
            if !source.current(clock.now_ms) {
                return Err(RuntimeError::Rejected);
            }
            // Only actual RF dispatch, never every reply=None request.
            let report = dtmf_source && source.local_operation_result();
            let admitted = if source.operation().command.action == LinkAction::PhoneDial {
                if available {
                    host.prepare_radio_phone(&local, &source.operation().command.node)
                        .and_then(|(claim, destination)| {
                            let acknowledgment = if report {
                                match host.runtime.queue_operation_acknowledgment(&local) {
                                    Ok(completion) => Some(completion),
                                    Err(error) => {
                                        let _ = host.cancel_outgoing_phone(&local, claim.id());
                                        return Err(error);
                                    }
                                }
                            } else {
                                None
                            };
                            Ok(Some((claim, destination, source, acknowledgment)))
                        })
                } else {
                    Err(RuntimeError::Rejected)
                }
            } else {
                host.hangup_radio_phone(&local).map(|()| None)
            };
            match admitted {
                Ok(Some(prepared)) => Ok(Some(prepared)),
                Ok(None) => {
                    if report {
                        host.radio_phone_result(&local, true);
                    }
                    Ok(None)
                }
                Err(error) => {
                    if report {
                        host.radio_phone_result(&local, false);
                    }
                    Err(error)
                }
            }
        })?;
        if let Some((claim, destination, source, acknowledgment)) = prepared {
            self.launch_phone(
                call,
                claim,
                revision,
                destination,
                Some(source),
                acknowledgment,
            )?;
        }
        Ok(())
    }
}

impl Engine {
    /// Revalidate a delayed progress record on the same serialized owner as call admission.
    pub(super) fn queue_radio_ringback(
        self: &Arc<Self>,
        revision: u64,
        generation: Option<u64>,
        claim: Arc<crate::phone::OutgoingClaim>,
    ) -> Result<rpt_advanced_core::controller::PlaybackCompletion, RuntimeError> {
        let owner = Arc::clone(self);
        self.run_inner(true, move |host| {
            if !owner.current(revision)
                || !claim.current()
                || !generation.is_some_and(|generation| {
                    host.runtime
                        .node(claim.local())
                        .and_then(|node| node.status(owner.clock().now_ms).active)
                        == Some(generation)
                })
            {
                return Err(RuntimeError::Rejected);
            }
            host.runtime.queue_phone_ringback(claim.local())
        })
    }
}
