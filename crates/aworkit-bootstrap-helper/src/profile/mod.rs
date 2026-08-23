//! Managed-local v1 profile decisions and selector contracts (Milestone 11.4).

mod adapter;
mod error;
mod model;
mod ports;
mod selector;

#[cfg(test)]
mod tests;

pub use adapter::ManagedLocalBuildProfileAdapter;
pub use error::ProfileError;
pub use model::*;
pub use ports::{PlatformActivationPortV1, ProfileObservationPortV1, SelectorMutationPortV1};
pub use selector::{HermeticSelectorPort, ManagedLocalSelectorAdapter};
