//! Site acquisition outside station control and audio callbacks.

pub mod acquisition;
pub mod backends;
pub mod iio;
pub mod radio;
pub mod worker;

pub(crate) mod command;

#[cfg(test)]
mod equipment_fixture;
