#![deny(warnings, missing_docs)]

//! Portable controller primitives with no Asterisk dependency.

/// Fixed native controller and radio sample rate.
pub const NATIVE_SAMPLE_RATE_HZ: u32 = 48_000;

pub mod config;
