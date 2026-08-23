//! Fail-closed managed-local profile errors.

use thiserror::Error;

/// Deterministic profile or selector rejection.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ProfileError {
    #[error("managed-local profile input is invalid: {0}")]
    Invalid(&'static str),
    #[error("managed-local profile is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("capability generation or digest is stale")]
    CapabilityDrift,
    #[error("selector source, destination, or identity is stale")]
    SelectorDrift,
    #[error("selector mutation outcome is ambiguous")]
    AmbiguousSelector,
    #[error("selector mutation replay changed bytes")]
    MutationReplay,
    #[error("profile observation failed: {0}")]
    Observation(String),
    #[error("native selector port failed: {0}")]
    Selector(String),
    #[error("build-slot verification failed: {0}")]
    Slot(String),
}
