//! Trusted invocation metadata and shared accounting for the cooperative executor.
use super::ProtectedOperand;
use super::{
    Action, ActionError, CommandLanguage, CommandSource, InvocationContext, MacroOutcome,
    PreparedAction, host::HostAction,
};
use std::sync::Arc;

/// Origin of the generic parameter; only a DTMF node operand can use the remembered-target shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterOrigin {
    /// A complete runtime or macro action.
    Typed,
    /// Exact tail of a completed mapping, including the empty tail.
    Dtmf,
}
/// Input to the domain dispatcher. The normal handler makes final capability and effect admission.
pub struct Invocation {
    /// Retained owner, generation, private reply and propagated mapping grant requirement.
    pub context: InvocationContext,
    /// Existing domain intent or cooperative macro operation.
    pub action: Action,
    /// Ephemeral raw parameter; deliberately excluded from Debug, serialization and receipts.
    pub parameter: Option<ProtectedOperand<String>>,
    /// Typed literals and DTMF input have distinct node-zero semantics.
    pub origin: ParameterOrigin,
}
impl InvocationContext {
    /// Check the retained generation/connection and any mapping's actual-source grant.
    pub fn recheck(&self, live_grant: bool) -> Result<(), ActionError> {
        let actual = matches!(
            self.source,
            CommandSource::Receiver | CommandSource::Connection { .. }
        );
        if !self.generation.is_current()
            || self.requires_grant && !(actual && live_grant)
            || matches!(&self.source,CommandSource::Connection{reply:r,..} if r.ended())
            || matches!(&self.source,CommandSource::Connection{reply,origin} if reply.phone_source().is_some_and(|p|p.call_id().to_string()!=*origin))
        {
            return Err(ActionError::SourceOnly);
        }
        Ok(())
    }
}
impl Action {
    pub(super) fn check_scope(&self, context: &InvocationContext) -> Result<(), ActionError> {
        use crate::config::resources::candidate::ResourceScope;
        if matches!(context.owner, ResourceScope::Site(_))
            && matches!(
                self,
                Self::Link(_)
                    | Self::Radio(_)
                    | Self::PhoneDial(_)
                    | Self::Maintenance(_)
                    | Self::Interface(_)
                    | Self::Message(..)
                    | Self::Mute(_)
                    | Self::ExitCommandMode
                    | Self::SendCommandDigit(..)
                    | Self::Administration(..)
                    | Self::PadTest
                    | Self::Host(
                        HostAction::LinkStatus
                            | HostAction::Symbol(_)
                            | HostAction::ReceiverUnkey
                            | HostAction::Input(_)
                            | HostAction::ReceivePolicy
                            | HostAction::PhoneGrammar(_)
                            | HostAction::PhoneAuthenticate(..)
                            | HostAction::PhoneRetire
                            | HostAction::AuthorizeIncoming
                            | HostAction::AdmitIncoming
                    )
            )
        {
            return Err(ActionError::SourceOnly);
        }
        Ok(())
    }

    pub(crate) fn permits_phone_dial(source: &CommandSource) -> bool {
        matches!(source, CommandSource::Receiver | CommandSource::Management(_))
    }
}
impl PreparedAction {
    /// Check trust provenance without inventing a live generation. Final admission
    /// rechecks this policy together with the actual owner, grant and generation.
    pub fn check_source(&self, source: &CommandSource) -> Result<(), ActionError> {
        let phone =
            matches!(source,CommandSource::Connection{reply:r,..} if r.phone_source().is_some());
        let input = matches!(
            source,
            CommandSource::Receiver | CommandSource::Connection { .. }
        );
        let allowed = match &self.action {
            Action::PhoneDial(_) => Action::permits_phone_dial(source),
            Action::Host(HostAction::PhoneAuthenticate(..) | HostAction::PhoneRetire) => phone,
            _ if matches!(source,CommandSource::Connection{reply:r,..} if r.phone_source().is_some() && !r.phone_authenticated()) => {
                false
            }
            Action::Host(HostAction::PhoneGrammar(_)) => phone,
            Action::Host(
                HostAction::Start
                | HostAction::Stop
                | HostAction::ReloadPoll
                | HostAction::ReceivePolicy,
            ) => matches!(source, CommandSource::Host),
            Action::Host(HostAction::AuthorizeIncoming | HostAction::AdmitIncoming) => {
                matches!(source, CommandSource::Provider)
            }
            Action::Host(HostAction::Symbol(_) | HostAction::Input(_)) => input,
            Action::Host(HostAction::ReceiverUnkey) | Action::PadTest => {
                matches!(source, CommandSource::Receiver)
            }
            Action::Administration(..) => input,
            Action::Maintenance(_) => {
                matches!(source, CommandSource::Management(_) | CommandSource::Host)
            }
            _ => true,
        };
        if !allowed {
            return Err(ActionError::SourceOnly);
        }
        Ok(())
    }

    /// Bind a typed action to the real source before dispatch. Ingress adapters alone can submit ingress.
    pub fn admit(
        &self,
        context: &InvocationContext,
        live_grant: bool,
    ) -> Result<Invocation, ActionError> {
        context.recheck(live_grant)?;
        self.action.check_scope(context)?;
        self.check_source(&context.source)?;
        Ok(Invocation {
            context: context.clone(),
            action: self.action.clone(),
            parameter: self.parameter.clone(),
            origin: ParameterOrigin::Typed,
        })
    }
}
/// One invocation's immutable settings snapshot and cumulative accounting across nested calls.
pub struct MacroBudget {
    language: Arc<CommandLanguage>,
    start_ms: u64,
    context: InvocationContext,
    parent_grants: Vec<bool>,
    actions: usize,
    symbols: usize,
    stopped: Option<MacroOutcome>,
}
impl MacroBudget {
    /// Begin the initial macro at depth one using the already captured monotonic time.
    pub fn start(
        language: Arc<CommandLanguage>,
        context: InvocationContext,
        start_ms: u64,
    ) -> Self {
        Self {
            language,
            start_ms,
            context,
            parent_grants: Vec::new(),
            actions: 0,
            symbols: 0,
            stopped: None,
        }
    }
    /// Actual caller, retained through the current frame.
    pub fn context(&self) -> &InvocationContext {
        &self.context
    }
    /// Borrow the settings and action lists captured at invocation start.
    pub fn language(&self) -> &CommandLanguage {
        &self.language
    }
    /// Recheck after each wait and before each effect. Cancellation and limit failures are terminal.
    pub fn resume(&mut self, live_grant: bool, now_ms: u64) -> Result<(), MacroOutcome> {
        if let Some(reason) = self.stopped {
            return Err(reason);
        }
        if self.context.recheck(live_grant).is_err() {
            return self.stop(MacroOutcome::Cancelled);
        }
        if now_ms
            .checked_sub(self.start_ms)
            .is_none_or(|elapsed| elapsed > self.language.limits.max_duration_ms)
        {
            return self.stop(MacroOutcome::Limit);
        }
        Ok(())
    }
    /// Charge one expanded action and its injected DTMF symbols exactly once before admission.
    pub fn charge(&mut self, action: &Action) -> Result<(), MacroOutcome> {
        if let Some(reason) = self.stopped {
            return Err(reason);
        }
        let symbols = match action {
            Action::ExecuteDtmf(d) => d.len(),
            _ => 0,
        };
        if self.actions >= self.language.limits.max_actions
            || symbols > self.language.limits.max_symbols - self.symbols
        {
            return self.stop(MacroOutcome::Limit);
        }
        self.actions += 1;
        self.symbols += symbols;
        Ok(())
    }
    /// Begin an inline child; no action, symbol or duration counter is reset.
    pub fn enter_child(&mut self, protected: bool) -> Result<(), MacroOutcome> {
        if let Some(reason) = self.stopped {
            return Err(reason);
        }
        if self.parent_grants.len() + 1 >= self.language.limits.max_depth {
            return self.stop(MacroOutcome::Limit);
        }
        self.parent_grants.push(self.context.requires_grant);
        self.context.requires_grant |= protected;
        Ok(())
    }
    /// Release one completed frame. The outermost frame supplies the scheduler's terminal outcome.
    pub fn finish_frame(&mut self) -> Option<MacroOutcome> {
        if self.stopped.is_some() {
            return self.stopped;
        }
        if let Some(parent) = self.parent_grants.pop() {
            self.context.requires_grant = parent;
            None
        } else {
            self.stopped = Some(MacroOutcome::Completed);
            self.stopped
        }
    }
    /// Record a terminal result. A completed effect is never rolled back or replayed.
    pub fn stop(&mut self, reason: MacroOutcome) -> Result<(), MacroOutcome> {
        Err(*self.stopped.get_or_insert(reason))
    }
}
