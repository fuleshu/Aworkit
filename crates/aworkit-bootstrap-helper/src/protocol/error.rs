//! Errors raised by the authenticated bootstrap protocol gateway.
//!
//! Every error is fail-closed: it either refuses admission before quiescence,
//! rejects a stale, replayed, or corrupt command, or reports that the exact
//! durable journal record was not committed. No acknowledgement is ever
//! returned before its underlying journal write succeeds.

use aworkit_trusted_core::PlatformReasonV1;
use thiserror::Error;

use crate::journal::BootstrapJournalError;

/// A bounded, deterministic rejection from the bootstrap gateway.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// A DTO failed its byte, field, version, or schema bound.
    #[error("bounded or schema violation: {0}")]
    Bounded(&'static str),

    /// The peer identity does not match the issued challenge or session.
    #[error("peer identity mismatch")]
    PeerMismatch,

    /// No challenge exists, or it is expired, for this admission.
    #[error("challenge is unknown or expired")]
    ChallengeInvalid,

    /// The one-use challenge has already been consumed.
    #[error("challenge already consumed")]
    ChallengeConsumed,

    /// The command or baton id was already seen with different content.
    #[error("replayed or corrupted command id")]
    CommandReplayed,

    /// A command carried a process generation that does not fence to the
    /// admitted session.
    #[error("stale or mismatched process generation")]
    StaleGeneration,

    /// The capability generation changed, drifted, or expired.
    #[error("capability generation changed or expired")]
    CapabilityDrift,

    /// The managed-local guarantee is absent; returns before quiescence.
    #[error("unsupported before quiescence: {0:?}")]
    Unsupported(PlatformReasonV1),

    /// Enrollment and activation may never overlap, and a second transaction
    /// is not allowed while one is active.
    #[error("a bootstrap transaction is already active")]
    TransactionActive,

    /// No transaction is active to accept the command.
    #[error("no active bootstrap transaction")]
    NoActiveTransaction,

    /// The command is not legal in the durable activation phase.
    #[error("command is not legal in the current activation phase")]
    IllegalPhase,

    /// The reader is not the sealed recipient generation.
    #[error("reader is not the sealed recipient generation")]
    RecipientMismatch,

    /// The underlying journal write failed; nothing was acknowledged.
    #[error("activation journal error: {0}")]
    Journal(#[from] BootstrapJournalError),

    /// A downstream port (preflight or enrollment) returned an error.
    #[error("bootstrap port error: {0}")]
    Port(String),
}

impl GatewayError {
    /// Whether the rejection means the managed-local guarantee is absent.
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}
