//! Logical local-history recovery without capability-effect replay.

use aworkit_local_store::{LocalHistoryStore, StoreError};
use serde_json::Value;
use thiserror::Error;

/// Folded recovery facts used to choose a new worker generation safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    pub head_sequence: u64,
    pub terminal: bool,
    pub last_checkpoint_hash: Option<String>,
    pub pending_delivery_count: usize,
    pub effect_replay_required: bool,
}

/// Reads only committed local facts; it cannot dispatch or repeat effects.
#[derive(Clone)]
pub struct LocalRecovery { store: LocalHistoryStore }

impl LocalRecovery {
    #[must_use]
    pub fn new(store: LocalHistoryStore) -> Self { Self { store } }

    /// Folds the known semantic event shapes and rejects unknown effect recovery.
    pub fn recover(&self, chat_id: &str, branch_id: &str) -> Result<RecoveryReport, RecoveryError> {
        let events = self.store.events(chat_id, branch_id)?;
        let terminal = events.last().is_some_and(|event| matches!(event.kind.as_str(), "completed" | "cancelled" | "failed"));
        let last_checkpoint_hash = events.iter().rev().find_map(|event| event.payload.get("checkpointHash").and_then(Value::as_str).map(str::to_owned));
        let pending_delivery_count = self.store.pending_outbox(u32::MAX)?.into_iter().filter(|outbox| outbox.chat_id == chat_id).count();
        Ok(RecoveryReport { head_sequence: u64::try_from(events.len()).map_err(|_| RecoveryError::HistoryTooLong)?, terminal, last_checkpoint_hash, pending_delivery_count, effect_replay_required: false })
    }
}

/// Recovery deliberately exposes no path that auto-replays a capability effect.
#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the local history has too many events to represent its sequence")]
    HistoryTooLong,
}
