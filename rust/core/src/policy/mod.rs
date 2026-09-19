//! Node-local, allocation-free control policies.

mod duplex;
mod identifier;

pub use duplex::DuplexPolicy;
pub use identifier::{IdentifierPolicy, IdentifierRule};
