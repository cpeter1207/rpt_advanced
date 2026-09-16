#![deny(warnings, missing_docs)]

//! Portable controller primitives with no Asterisk dependency.

/// Fixed native controller and radio sample rate.
pub const NATIVE_SAMPLE_RATE_HZ: u32 = 48_000;

pub mod access;
pub mod audio;
pub mod command;
pub mod config;
pub mod control;
pub mod controller;
pub mod link;
pub mod media;
pub mod policy;
pub mod runtime;
pub mod schedule;
pub mod template;
pub mod time;
