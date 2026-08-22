//! Process-neutral canonical-history contracts.
//!
//! These DTOs are owned by Aworkit rather than SQLite or portable-store
//! implementations. They let the Trusted Core depend on a sealed port while
//! concrete storage adapters remain in their isolated process crates.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::StableId;

/// The one canonical history backend selected for a Chat.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HistoryBackendV1 {
    LocalSqlite,
    PortableProject { repository_id: StableId },
}

/// One immutable semantic state transition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventV1 {
    pub event_id: StableId,
    pub schema_version: u16,
    pub kind: String,
    pub payload: Value,
}

/// One immutable execution-attempt fact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptV1 {
    pub attempt_id: StableId,
    pub operation_id: StableId,
    pub ordinal: u32,
    pub outcome_class: String,
}

/// Pure-reducer state captured at the new committed head.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointV1 {
    pub reducer_version: String,
    pub state_hash: String,
    pub frozen_snapshot_ref: Option<StableId>,
}

/// Stable command or invocation identity. The storage adapter computes the
/// request digest from the complete canonical batch; callers do not supply it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DedupV1 {
    pub key_type: String,
    pub key: StableId,
}

/// A committed delivery record, never visible before its semantic transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxV1 {
    pub outbox_id: StableId,
    pub destination: String,
    pub schema_version: u16,
    pub payload: Value,
}

/// A previously prepared content object admitted into the semantic transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedArtifactRefV1 {
    pub token_id: StableId,
    pub artifact_id: StableId,
    pub content_hash: String,
    pub byte_size: u64,
    pub staging_generation: u64,
    pub origin_event_id: StableId,
}

/// Complete state that must advance atomically for LocalSqlite history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitBatchV1 {
    pub backend: HistoryBackendV1,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub branch_id: StableId,
    pub expected_head: u64,
    pub expected_aggregate_version: u64,
    pub events: Vec<EventV1>,
    pub attempts: Vec<AttemptV1>,
    pub checkpoint: Option<CheckpointV1>,
    pub deduplication: Option<DedupV1>,
    pub outbox: Vec<OutboxV1>,
    pub prepared_artifacts: Vec<PreparedArtifactRefV1>,
}

/// Durable facts returned only after a verified local transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitReceiptV1 {
    pub head_sequence: u64,
    pub aggregate_version: u64,
    pub event_ids: Vec<StableId>,
    pub checkpoint_hash: Option<String>,
    pub outbox_ids: Vec<StableId>,
    pub request_hash: String,
}

/// New durable commit or the exact receipt of an idempotent retry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "receipt", rename_all = "snake_case")]
pub enum CommitOutcomeV1 {
    Committed(CommitReceiptV1),
    Existing(CommitReceiptV1),
}

/// One ordered committed delivery awaiting acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingOutboxV1 {
    pub outbox_id: StableId,
    pub chat_id: StableId,
    pub branch_id: StableId,
    pub commit_sequence: u64,
    pub delivery_cursor: u64,
    pub destination: String,
    pub schema_version: u16,
    pub payload: Value,
    pub payload_hash: String,
}

/// Stable process-port error, containing no SQLite-native value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryPortErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub inspectable_read_only: bool,
}

/// Object-safe LocalSqlite port consumed by Trusted Core.
pub trait LocalHistoryCommitPort: Send + Sync {
    fn commit(&self, batch: &CommitBatchV1) -> Result<CommitOutcomeV1, HistoryPortErrorV1>;
    fn pending_outbox(
        &self,
        after_cursor: u64,
        limit: u32,
    ) -> Result<Vec<PendingOutboxV1>, HistoryPortErrorV1>;
    fn mark_outbox_delivered(
        &self,
        outbox_id: &StableId,
        expected_cursor: u64,
    ) -> Result<(), HistoryPortErrorV1>;
}

/// Portable commit preparation request issued before head publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortablePrepareV1 {
    pub operation_id: StableId,
    pub chat_id: StableId,
    pub branch_id: StableId,
    pub expected_generation: u64,
    pub expected_next_ordinal: u64,
    pub expected_head_hash: Option<String>,
    /// Sanitized provider-neutral semantic record. The adapter canonicalizes
    /// these bytes and verifies `record_hash`; no out-of-band payload exists.
    pub record: Value,
    pub record_hash: String,
    /// Optional reducer checkpoint whose canonical bytes must match the hash.
    pub checkpoint: Option<Value>,
    pub checkpoint_hash: String,
}

/// Identity of immutable portable bytes prepared but not yet published.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortablePreparedV1 {
    pub operation_id: StableId,
    pub commit_id: StableId,
    pub object_hash: String,
    pub expected_generation: u64,
}

/// Verified portable publication receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableCommitReceiptV1 {
    pub operation_id: StableId,
    pub commit_id: StableId,
    pub branch_id: StableId,
    pub previous_head_hash: Option<String>,
    pub published_head_hash: String,
    pub generation: u64,
    pub checkpoint_hash: String,
}

/// Noncanonical journal begin facts written before portable publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableRuntimeBeginV1 {
    pub operation_id: StableId,
    pub machine_instance_id: StableId,
    pub binding_generation: u64,
    pub expected_generation: u64,
    pub chat_id: StableId,
    pub branch_id: StableId,
    pub commit_id: StableId,
    pub expected_head_hash: Option<String>,
    pub candidate_head_hash: String,
    pub checkpoint_hash: String,
}

/// Exact head-linked facts durable before core acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableRuntimeFinalizeV1 {
    pub operation_id: StableId,
    pub verified_receipt: PortableCommitReceiptV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum PortableRuntimeFactsV1 {
    Pending {
        begin: PortableRuntimeBeginV1,
    },
    HeadLinked {
        begin: PortableRuntimeBeginV1,
        receipt: PortableCommitReceiptV1,
    },
    Quarantined {
        begin: PortableRuntimeBeginV1,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortablePortErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub uncertain_publication: bool,
}

/// Canonical portable history process port. Prepare is effect-free with respect
/// to the branch head; publish performs the expected-generation head change;
/// verify never republishes.
pub trait PortableCanonicalCommitPort: Send + Sync {
    fn prepare(
        &self,
        request: &PortablePrepareV1,
    ) -> Result<PortablePreparedV1, PortablePortErrorV1>;
    fn publish(
        &self,
        prepared: &PortablePreparedV1,
    ) -> Result<PortableCommitReceiptV1, PortablePortErrorV1>;
    fn verify(
        &self,
        receipt: &PortableCommitReceiptV1,
    ) -> Result<PortableCommitReceiptV1, PortablePortErrorV1>;
    fn read_head(
        &self,
        branch_id: &StableId,
    ) -> Result<Option<PortableCommitReceiptV1>, PortablePortErrorV1>;
}

/// Strictly noncanonical machine-local companion for portable publication.
pub trait PortableRuntimeJournalPort: Send + Sync {
    fn begin(&self, request: &PortableRuntimeBeginV1) -> Result<(), HistoryPortErrorV1>;
    fn finalize(&self, request: &PortableRuntimeFinalizeV1) -> Result<(), HistoryPortErrorV1>;
    fn facts(
        &self,
        operation_id: &StableId,
    ) -> Result<Option<PortableRuntimeFactsV1>, HistoryPortErrorV1>;
    fn quarantine(&self, operation_id: &StableId, reason: &str) -> Result<(), HistoryPortErrorV1>;
}
