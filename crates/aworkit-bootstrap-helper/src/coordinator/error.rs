//! Bounded coordinator failures.

use thiserror::Error;

/// A failure before a protected terminal result can be returned.
#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("another enrollment or activation is already being coordinated")]
    Busy,
    #[error("activation input does not match its durable baton: {0}")]
    Fence(&'static str),
    #[error("activation has no durable accepted baton")]
    MissingBaton,
    #[error("activation journal is missing")]
    MissingJournal,
    #[error("journal operation failed: {0}")]
    Journal(String),
    #[error("slot operation failed: {0}")]
    Slot(String),
    #[error("selector operation failed: {0}")]
    Selector(String),
    #[error("terminal recovery requires activation execution context")]
    MissingRecoveryContext,
}
