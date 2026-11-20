//! Prepared typed command and macro language, independent of a live dispatcher.
mod action;
mod binding;
mod fields;
pub(crate) use fields::scaled;
mod host;
mod invocation;
pub use invocation::{Invocation, MacroBudget, ParameterOrigin};
mod profiles;
mod radio;
mod shape;
#[cfg(test)]
mod tests;

pub use action::{
    Action, Destination, MuteTarget, PreparedAction, prepare_macro_action, prepare_runtime_action,
};
pub use binding::{Completed, Mapping};
pub use host::{HostAction, InterfaceAction, MaintenanceAction, MixerTarget, Pin};
pub use profiles::{CommandLanguage, LanguageError, LanguageWarning, MacroLimits};
pub use radio::{CapabilityValue, OperatingSignal, OperatingState, RadioAction};

/// Value-free validation diagnostics. Field identifiers always come from the decoder.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ActionError {
    /// Unrecognized action or malformed object.
    Invalid,
    /// Missing or invalid recognized field.
    Field(&'static str),
    /// A field belongs to another variant or is unknown.
    Extra,
    /// Source-bound or credential-bearing action cannot be persisted.
    SourceOnly,
    /// A legitimate source requested an operation without a runtime owner.
    Unavailable,
    /// Extra, missing or conflicting action parameter.
    Parameter,
}

/// Trusted caller and exact response owner, supplied by the ingress adapter.
#[derive(Clone)]
pub enum CommandSource {
    /// Local receiver (captured generation belongs to InvocationContext).
    Receiver,
    /// Direct or addressed command from this exact live peer/private call.
    Connection {
        /// Exact transport token, including private phone identity where applicable.
        reply: crate::link::PeerReply,
        /// Source label used jointly with the exact reply for existing per-source state.
        origin: String,
    },
    /// Serialized local scheduler for the owning scope.
    Scheduler,
    /// Authenticated management service identified by its host-provided principal.
    Management(String),
    /// Actual lifecycle or provider callback owner.
    Host,
    /// Actual incoming protocol callback owner.
    Provider,
}

/// Identity retained across every action and continuation. Grants are checked live.
#[derive(Clone)]
pub struct InvocationContext {
    /// Scope assigned by the registry.
    pub owner: crate::config::resources::candidate::ResourceScope,
    /// Generation to recheck before effects or resumed work.
    pub generation: std::sync::Arc<crate::runtime::GenerationWork>,
    /// Originating principal and private reply association.
    pub source: CommandSource,
    /// A protected mapping anywhere in the caller chain requires a live source grant.
    pub requires_grant: bool,
}

/// Terminal outcome of one macro sequence, used to release scheduled ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacroOutcome {
    /// All actions reached normal admission, including nested sequences.
    Completed,
    /// A domain handler rejected an operation.
    Rejected,
    /// Owner, authorization or generation was lost.
    Cancelled,
    /// The invocation exhausted its shared execution bounds.
    Limit,
}
