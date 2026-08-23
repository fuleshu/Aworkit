//! Fail-closed immutable-slot errors.

use thiserror::Error;

/// Deterministic failure while verifying, materializing, or reopening a slot.
#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum BuildSlotError {
    #[error("slot input exceeds a bounded quota: {0}")]
    Bounded(&'static str),
    #[error("slot manifest is malformed or not self-consistent: {0}")]
    Manifest(&'static str),
    #[error("slot entry path is unsafe or collides: {0}")]
    UnsafePath(String),
    #[error("slot content integrity failed for {0}")]
    Integrity(String),
    #[error("slot provenance does not match the admitted build")]
    ProvenanceMismatch,
    #[error("slot is incompatible with this helper or platform: {0}")]
    Unsupported(&'static str),
    #[error("anchored slot identity changed after verification")]
    IdentityChanged,
    #[error("slot ownership, volume, immutability, or no-follow guarantee is absent")]
    StorageGuaranteeAbsent,
    #[error("slot role transition is not legal")]
    IllegalRoleTransition,
    #[error("slot or artifact was not found")]
    NotFound,
    #[error("artifact port failed: {0}")]
    Artifact(String),
    #[error("slot storage port failed: {0}")]
    Storage(String),
}
