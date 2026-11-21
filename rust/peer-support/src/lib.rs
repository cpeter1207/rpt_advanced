//! Shared host-side ownership and descriptor glue; no controller or codec algorithms.

pub mod control;
pub mod echolink;
pub mod remote_ticket;
pub mod secrets;

#[cfg(target_os = "linux")]
pub mod sealed_records;

#[cfg(target_os = "linux")]
pub mod working_files;

#[cfg(target_os = "linux")]
pub mod https;
