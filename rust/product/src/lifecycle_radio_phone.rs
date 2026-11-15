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
                        .map(|(claim, destination)| Some((claim, destination, source, report)))
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
        if let Some((claim, destination, source, report)) = prepared {
            self.launch_phone(call, claim, revision, destination, Some(source), report)?;
        }
        Ok(())
    }
}
