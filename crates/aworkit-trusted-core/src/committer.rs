//! Core-only routing into exactly one canonical Chat-history backend.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use aworkit_protocol::{
    CommitBatchV1, CommitOutcomeV1, HistoryBackendV1, HistoryPortErrorV1, LocalHistoryCommitPort,
    PendingOutboxV1, PortableCommitReceiptV1, PortablePrepareV1, StableId,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::PortableCommitGate;

/// A Chat chooses its canonical history once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryBinding {
    LocalSqlite,
    PortableProject { repository_id: String },
}

/// Compatibility local request. Portable callers use the complete request enum
/// so expected-generation/head fences cannot be omitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRequest {
    pub binding: HistoryBinding,
    pub batch: CommitBatchV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalCommitRequestV1 {
    Local {
        batch: CommitBatchV1,
    },
    Portable {
        repository_id: StableId,
        prepare: PortablePrepareV1,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalCommitOutcomeV1 {
    Local(CommitOutcomeV1),
    Portable(PortableCommitReceiptV1),
}

/// The trusted core's sole canonical semantic-history append router.
#[derive(Clone)]
pub struct CanonicalCommitter {
    local: Arc<dyn LocalHistoryCommitPort>,
    portable: Arc<Mutex<BTreeMap<String, PortableCommitGate>>>,
    bindings: Arc<Mutex<BTreeMap<String, HistoryBinding>>>,
}

impl CanonicalCommitter {
    #[must_use]
    pub fn new(local: impl LocalHistoryCommitPort + 'static) -> Self {
        Self::from_local_port(Arc::new(local))
    }

    #[must_use]
    pub fn from_local_port(local: Arc<dyn LocalHistoryCommitPort>) -> Self {
        Self {
            local,
            portable: Arc::new(Mutex::new(BTreeMap::new())),
            bindings: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn register_portable_repository(
        &self,
        repository_id: StableId,
        gate: PortableCommitGate,
    ) -> Result<(), CoreCommitError> {
        let mut portable = self
            .portable
            .lock()
            .map_err(|_| CoreCommitError::Poisoned)?;
        if portable.contains_key(repository_id.as_str()) {
            return Err(CoreCommitError::RepositoryAlreadyRegistered);
        }
        portable.insert(repository_id.as_str().to_owned(), gate);
        Ok(())
    }

    /// Registers or checks the irreversible history binding for a Chat.
    pub fn bind_chat(&self, chat_id: &str, binding: HistoryBinding) -> Result<(), CoreCommitError> {
        let chat_id = StableId::parse(chat_id).map_err(|_| CoreCommitError::InvalidRequest)?;
        let mut bindings = self
            .bindings
            .lock()
            .map_err(|_| CoreCommitError::Poisoned)?;
        match bindings.get(chat_id.as_str()) {
            Some(existing) if existing != &binding => Err(CoreCommitError::HistoryBindingConflict),
            Some(_) => Ok(()),
            None => {
                bindings.insert(chat_id.as_str().to_owned(), binding);
                Ok(())
            }
        }
    }

    /// Local compatibility entry point. It refuses portable requests because a
    /// `CommitRequest` cannot carry expected-generation/head publication fences.
    pub fn commit(&self, request: &CommitRequest) -> Result<CommitOutcomeV1, CoreCommitError> {
        if request.binding != HistoryBinding::LocalSqlite
            || request.batch.backend != HistoryBackendV1::LocalSqlite
        {
            return Err(CoreCommitError::PortableFencesRequired);
        }
        self.bind_chat(request.batch.chat_id.as_str(), HistoryBinding::LocalSqlite)?;
        self.local.commit(&request.batch).map_err(history_error)
    }

    pub fn commit_v1(
        &self,
        request: &CanonicalCommitRequestV1,
    ) -> Result<CanonicalCommitOutcomeV1, CoreCommitError> {
        match request {
            CanonicalCommitRequestV1::Local { batch } => {
                if batch.backend != HistoryBackendV1::LocalSqlite {
                    return Err(CoreCommitError::BackendMismatch);
                }
                self.bind_chat(batch.chat_id.as_str(), HistoryBinding::LocalSqlite)?;
                Ok(CanonicalCommitOutcomeV1::Local(
                    self.local.commit(batch).map_err(history_error)?,
                ))
            }
            CanonicalCommitRequestV1::Portable {
                repository_id,
                prepare,
            } => {
                let record = portable_batch(&prepare.record)?;
                if record.chat_id != prepare.chat_id
                    || record.branch_id != prepare.branch_id
                    || record.backend
                        != (HistoryBackendV1::PortableProject {
                            repository_id: repository_id.clone(),
                        })
                {
                    return Err(CoreCommitError::BackendMismatch);
                }
                let binding = HistoryBinding::PortableProject {
                    repository_id: repository_id.as_str().to_owned(),
                };
                self.bind_chat(prepare.chat_id.as_str(), binding)?;
                let gate = self
                    .portable
                    .lock()
                    .map_err(|_| CoreCommitError::Poisoned)?
                    .get(repository_id.as_str())
                    .cloned()
                    .ok_or(CoreCommitError::UnknownPortableRepository)?;
                Ok(CanonicalCommitOutcomeV1::Portable(
                    gate.commit(prepare).map_err(CoreCommitError::Portable)?,
                ))
            }
        }
    }

    /// Constructs the complete process-neutral portable preparation request.
    /// The caller supplies the head generation/hash observed when planning; the
    /// repository rechecks them atomically at publication.
    pub fn portable_request(
        batch: &CommitBatchV1,
        operation_id: StableId,
        expected_generation: u64,
        expected_head_hash: Option<String>,
    ) -> Result<PortablePrepareV1, CoreCommitError> {
        if !matches!(batch.backend, HistoryBackendV1::PortableProject { .. })
            || batch.events.is_empty()
        {
            return Err(CoreCommitError::InvalidRequest);
        }
        let record = serde_json::to_value(batch).map_err(|_| CoreCommitError::InvalidRequest)?;
        Self::portable_request_from_record(
            batch,
            operation_id,
            expected_generation,
            expected_head_hash,
            record,
        )
    }

    /// Constructs a portable request carrying the canonical scrubbed snapshot
    /// and provenance value. The portable repository validates that context;
    /// the core still authenticates every byte in the preparation hash.
    pub fn portable_request_with_context(
        batch: &CommitBatchV1,
        portable_context: Value,
        operation_id: StableId,
        expected_generation: u64,
        expected_head_hash: Option<String>,
    ) -> Result<PortablePrepareV1, CoreCommitError> {
        if !portable_context.is_object() {
            return Err(CoreCommitError::InvalidRequest);
        }
        let record = serde_json::json!({
            "batch": batch,
            "context": portable_context,
        });
        Self::portable_request_from_record(
            batch,
            operation_id,
            expected_generation,
            expected_head_hash,
            record,
        )
    }

    fn portable_request_from_record(
        batch: &CommitBatchV1,
        operation_id: StableId,
        expected_generation: u64,
        expected_head_hash: Option<String>,
        record: Value,
    ) -> Result<PortablePrepareV1, CoreCommitError> {
        if !matches!(batch.backend, HistoryBackendV1::PortableProject { .. })
            || batch.events.is_empty()
        {
            return Err(CoreCommitError::InvalidRequest);
        }
        let checkpoint = batch
            .checkpoint
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| CoreCommitError::InvalidRequest)?;
        Ok(PortablePrepareV1 {
            operation_id,
            chat_id: batch.chat_id.clone(),
            branch_id: batch.branch_id.clone(),
            expected_generation,
            expected_next_ordinal: batch.expected_head,
            expected_head_hash,
            record_hash: portable_value_hash("portable-record-v1", &record)?,
            record,
            checkpoint_hash: portable_value_hash(
                "portable-checkpoint-v1",
                checkpoint.as_ref().unwrap_or(&Value::Null),
            )?,
            checkpoint,
        })
    }

    /// Returns only committed delivery work for the local worker/UI fan-out loop.
    pub fn pending_delivery(&self, limit: u32) -> Result<Vec<PendingOutboxV1>, CoreCommitError> {
        self.pending_delivery_after(0, limit)
    }

    pub fn pending_delivery_after(
        &self,
        after_cursor: u64,
        limit: u32,
    ) -> Result<Vec<PendingOutboxV1>, CoreCommitError> {
        self.local
            .pending_outbox(after_cursor, limit)
            .map_err(history_error)
    }

    pub fn mark_delivered_v1(
        &self,
        outbox_id: &StableId,
        expected_cursor: u64,
    ) -> Result<(), CoreCommitError> {
        self.local
            .mark_outbox_delivered(outbox_id, expected_cursor)
            .map_err(history_error)
    }

    /// Compatibility acknowledgement looks up the committed cursor first and
    /// still delegates to the cursor-fenced port.
    pub fn mark_delivered(&self, outbox_id: &str) -> Result<(), CoreCommitError> {
        let outbox_id = StableId::parse(outbox_id).map_err(|_| CoreCommitError::InvalidRequest)?;
        let pending = self.pending_delivery_after(0, u32::MAX)?;
        let cursor = pending
            .iter()
            .find(|entry| entry.outbox_id == outbox_id)
            .map(|entry| entry.delivery_cursor)
            .ok_or(CoreCommitError::UnknownOutbox)?;
        self.mark_delivered_v1(&outbox_id, cursor)
    }
}

fn portable_batch(record: &Value) -> Result<CommitBatchV1, CoreCommitError> {
    if let Ok(batch) = serde_json::from_value::<CommitBatchV1>(record.clone()) {
        return Ok(batch);
    }
    record
        .get("batch")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or(CoreCommitError::InvalidRequest)
}

fn portable_value_hash(domain: &str, value: &Value) -> Result<String, CoreCommitError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| CoreCommitError::InvalidRequest)?;
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn history_error(error: HistoryPortErrorV1) -> CoreCommitError {
    CoreCommitError::LocalPort {
        code: error.code,
        message: error.message,
        retryable: error.retryable,
    }
}

#[derive(Debug, Error)]
pub enum CoreCommitError {
    #[error("a Chat cannot change its canonical history backend")]
    HistoryBindingConflict,
    #[error("canonical history backend does not match the request record")]
    BackendMismatch,
    #[error("portable commit requires expected-generation and expected-head fences")]
    PortableFencesRequired,
    #[error("portable repository is not registered")]
    UnknownPortableRepository,
    #[error("portable repository ID is already registered")]
    RepositoryAlreadyRegistered,
    #[error("canonical commit request is malformed")]
    InvalidRequest,
    #[error("delivery outbox entry is not pending")]
    UnknownOutbox,
    #[error("core commit state is unavailable")]
    Poisoned,
    #[error("local history port error {code}: {message}")]
    LocalPort {
        code: String,
        message: String,
        retryable: bool,
    },
    #[error(transparent)]
    Portable(#[from] crate::PortableGateError),
}
