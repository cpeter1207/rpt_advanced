//! Shared host-side ownership and descriptor glue; no controller or codec algorithms.

pub mod control;
pub mod echolink;
pub mod reload;
pub mod secrets;

/// Experimental bounded SIP parsing shared by both host compositions.
pub mod sip;
