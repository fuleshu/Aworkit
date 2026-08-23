//! Immutable, content-addressed managed-local build slots (Milestone 11.3).

mod error;
mod manager;
mod model;
mod ports;
mod storage;

#[cfg(test)]
mod tests;

pub use error::BuildSlotError;
pub use manager::ImmutableBuildSlotManager;
pub use model::*;
pub use ports::{BootstrapArtifactReadPortV1, BuildSlotStoragePortV1, BuildSlotVerifyPortV1};
pub use storage::InMemoryBuildSlotStorage;
