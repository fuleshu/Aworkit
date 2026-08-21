//! Core-only routing into exactly one canonical Chat-history backend.

use std::{collections::BTreeMap, sync::{Arc, Mutex}};

use aworkit_local_store::{CommitBatch, CommitOutcome, LocalHistoryStore, StoreError};
use thiserror::Error;

/// A Chat chooses its canonical history once; portable routing is added later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryBinding { LocalSqlite, PortableProject { repository_id: String } }

/// A named local commit request, including the binding selected at Chat creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRequest { pub binding: HistoryBinding, pub batch: CommitBatch }

/// The trusted core's only local semantic-event append path.
#[derive(Clone)]
pub struct CanonicalCommitter {
    local: LocalHistoryStore,
    bindings: Arc<Mutex<BTreeMap<String, HistoryBinding>>>,
}

impl CanonicalCommitter {
    #[must_use]
    pub fn new(local: LocalHistoryStore) -> Self { Self { local, bindings: Arc::new(Mutex::new(BTreeMap::new())) } }

    /// Registers or checks the one irreversible history binding for a Chat.
    pub fn bind_chat(&self, chat_id: &str, binding: HistoryBinding) -> Result<(), CoreCommitError> {
        let mut bindings = self.bindings.lock().map_err(|_| CoreCommitError::Poisoned)?;
        match bindings.get(chat_id) {
            Some(existing) if existing != &binding => Err(CoreCommitError::HistoryBindingConflict),
            Some(_) => Ok(()),
            None => { bindings.insert(chat_id.to_owned(), binding); Ok(()) }
        }
    }

    /// Commits local events, checkpoints, deduplication, and delivery outboxes atomically.
    pub fn commit(&self, request: &CommitRequest) -> Result<CommitOutcome, CoreCommitError> {
        self.bind_chat(&request.batch.chat_id, request.binding.clone())?;
        match request.binding {
            HistoryBinding::LocalSqlite => self.local.commit(&request.batch).map_err(CoreCommitError::from),
            HistoryBinding::PortableProject { .. } => Err(CoreCommitError::PortableUnavailable),
        }
    }

    /// Returns only committed delivery work for the core's worker/UI fan-out loop.
    pub fn pending_delivery(&self, limit: u32) -> Result<Vec<aworkit_local_store::PendingOutbox>, CoreCommitError> { Ok(self.local.pending_outbox(limit)?) }
    /// Records idempotent post-commit delivery without ever modifying event history.
    pub fn mark_delivered(&self, outbox_id: &str) -> Result<(), CoreCommitError> { Ok(self.local.mark_outbox_delivered(outbox_id)?) }
}

/// Commit routing failures are explicit so callers never acknowledge uncertain work.
#[derive(Debug, Error)]
pub enum CoreCommitError {
    #[error("a Chat cannot change its canonical history backend")]
    HistoryBindingConflict,
    #[error("portable history routing is not available in the local runtime")]
    PortableUnavailable,
    #[error("core commit state is unavailable")]
    Poisoned,
    #[error(transparent)]
    Store(#[from] StoreError),
}
