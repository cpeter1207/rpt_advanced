//! Shared host-side ownership and descriptor glue; no controller or codec algorithms.

pub mod control;
pub mod echolink;
pub mod secrets;

#[cfg(target_os = "linux")]
pub mod working_files;
