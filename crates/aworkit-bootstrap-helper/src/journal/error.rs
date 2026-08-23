//! Fail-closed error type for the activation journal.
//!
//! The journal is a single-writer durable record; every error is a definite,
//! bounded condition. Nothing here invents success — corruption, fencing
//! mismatch, or a torn tail either blocks further switching or routes to manual
//! recovery.

use thiserror::Error;

/// Failure surfaced by [`ActivationJournalPortV1`](super::ActivationJournalPortV1).
#[derive(Debug, Error)]
pub enum BootstrapJournalError {
    #[error("journal record could not be encoded canonically")]
    Encoding,

    #[error("journal maintenance lock is held by another transaction")]
    Busy,

    #[error("journal mutation is not fenced to the held single-flight owner")]
    NotLocked,

    #[error("journal transaction kind conflicts with the durable header")]
    KindConflict,

    #[error("journal transaction identity conflicts with the durable header")]
    IdentityConflict,

    #[error("journal fencing mismatch: expected ordinal {expected}, chain is at {actual}")]
    StaleOrdinal { expected: u64, actual: u64 },

    #[error("journal fencing mismatch: expected phase {expected:?}, chain is at {actual:?}")]
    StalePhase { expected: String, actual: String },

    #[error("journal phase transition {from:?} -> {to:?} is not allowed")]
    IllegalPhaseTransition { from: String, to: String },

    #[error("journal terminal phase is immutable")]
    TerminalImmutable,

    #[error("journal already has a sealed terminal receipt")]
    TerminalSealed,

    #[error("journal terminal receipt is missing")]
    TerminalMissing,

    #[error("record {ordinal} breaks the hash chain")]
    ChainBroken { ordinal: u64 },

    #[error("journal header is corrupt or unsupported")]
    HeaderCorrupt,

    #[error("journal exceeded its capped durable record count")]
    RecordCapExceeded,

    #[error("journal record or field failed validation: {0}")]
    Invalid(&'static str),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl BootstrapJournalError {
    /// Whether the error leaves the journal in an ambiguous state that requires
    /// manual recovery rather than a simple retry.
    #[must_use]
    pub const fn requires_manual_recovery(&self) -> bool {
        matches!(self, Self::ChainBroken { .. })
    }
}
