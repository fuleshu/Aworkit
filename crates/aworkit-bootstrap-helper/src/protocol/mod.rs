//! The authenticated bootstrap protocol and generation fence (Milestone 11.2).
//!
//! This module is the helper's sole core-facing, version-negotiated local
//! protocol boundary. It issues one-use challenges, bounds and validates every
//! wire DTO, deduplicates command IDs, fences the current, candidate, and
//! rollback application generations, and writes accepted facts to the
//! activation journal before acknowledgement. It routes capability
//! classification and enrollment materialization to unprivileged ports that
//! later milestones implement natively.

mod error;
mod gateway;
mod model;
mod ports;

#[cfg(test)]
mod tests;

pub use error::GatewayError;
pub use gateway::{BootstrapGateway, CHALLENGE_TTL_MS};
pub use model::*;
pub use ports::{
    ArcActivationJournal, BootstrapEnrollmentPortV1, BootstrapPreflightPortV1,
    BootstrapProtocolPortV1,
};
