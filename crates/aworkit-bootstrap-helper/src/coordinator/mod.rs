//! Single-flight enrollment, activation, rollback, and recovery coordination.
//!
//! The coordinator contains policy and ordering only. Durable facts go through
//! the activation journal; slots, selectors, and processes are accessed only
//! through their typed platform-neutral ports.

mod coordinator;
mod error;
mod model;
mod ports;

#[cfg(test)]
mod tests;

pub use coordinator::ActivationRollbackCoordinator;
pub use error::CoordinatorError;
pub use model::*;
pub use ports::ActivationControlPortV1;
