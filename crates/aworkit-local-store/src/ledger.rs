//! SQLite-backed local semantic history and its single atomic commit port.
//!
//! This module intentionally exposes no independent event insert operation.
//! `commit` is the only mutation API so an acknowledged state transition always
//! includes its event rows, stream head, optional attempt/checkpoint, dedup
//! result, and delivery outbox rows.

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::{
    Connection, Error as SqlError, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_EVENTS_PER_COMMIT: usize = 64;
const MAX_COMMIT_BYTES: usize = 1024 * 1024;

/// An immutable, meaningful Aworkit Chat/Run state transition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Globally stable event identity, never regenerated on recovery.
    pub event_id: String,
    /// A domain-owned semantic event kind.
    pub kind: String,
    /// Schema-versioned Aworkit JSON, never provider-native payloads or secrets.
    pub payload: Value,
}

/// Stable identity assigned to an execution attempt without rewriting prior attempts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// Globally stable attempt identity.
    pub attempt_id: String,
    /// The logical operation this is attempting.
    pub operation_id: String,
    /// The monotonically increasing retry ordinal for that operation.
    pub ordinal: u32,
    /// A bounded outcome classification.
    pub outcome_class: String,
}

/// A reducer checkpoint created in the same transaction as its head.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The reducer version that produced the checkpoint state hash.
    pub reducer_version: String,
    /// The hash of the reconstructed state at the committed sequence.
    pub state_hash: String,
    /// Optional immutable frozen-snapshot artifact reference.
    pub frozen_snapshot_ref: Option<String>,
}

/// The idempotency identity for a command or invocation commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Deduplication {
    /// Separates command and invocation key spaces.
    pub key_type: String,
    /// Stable idempotency key.
    pub key: String,
    /// Exact request digest; a reused key with a different digest is rejected.
    pub request_hash: String,
}

/// A payload to deliver only after its transaction is durably committed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Globally stable outbox identity.
    pub outbox_id: String,
    /// The projected destination, not an executable command.
    pub destination: String,
    /// A schema-versioned, redacted payload.
    pub payload: Value,
}

/// All local state that must advance as one canonical semantic commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitBatch {
    /// Persistent Chat identity; a Run is the same logical object in v1.
    pub chat_id: String,
    /// Branch identity within this Chat stream.
    pub branch_id: String,
    /// The previously observed durable stream head.
    pub expected_head: u64,
    /// The meaningful semantic transitions to append contiguously.
    pub events: Vec<Event>,
    /// Optional stable attempt record associated with this transition.
    pub attempt: Option<Attempt>,
    /// Optional reducer checkpoint for the new committed head.
    pub checkpoint: Option<Checkpoint>,
    /// Optional command or invocation idempotency result.
    pub deduplication: Option<Deduplication>,
    /// Projection notifications that become visible only after the commit.
    pub outbox: Vec<OutboxEntry>,
}

/// Durable facts that the core may acknowledge and publish to subscribers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitReceipt {
    /// The durable stream head after this transaction.
    pub head_sequence: u64,
    /// The exact immutable events made visible by this transaction.
    pub event_ids: Vec<String>,
    /// The checkpoint state hash when this batch created one.
    pub checkpoint_hash: Option<String>,
    /// Committed outbox identities in delivery order.
    pub outbox_ids: Vec<String>,
}

/// The result of either a new transaction or a verified idempotent retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitOutcome {
    /// This call inserted and durably committed a new semantic transition.
    Committed(CommitReceipt),
    /// An identical deduplication key already has this durable receipt.
    Existing(CommitReceipt),
}

/// A committed outbox row awaiting idempotent projection delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOutbox {
    /// Stable outbox identity.
    pub outbox_id: String,
    /// Chat whose semantic commit produced this entry.
    pub chat_id: String,
    /// The sequence of the commit that produced this entry.
    pub commit_sequence: u64,
    /// Destination selected by the trusted core.
    pub destination: String,
    /// Redacted payload parsed after its canonical commit.
    pub payload: Value,
}

/// A committed source row used only to rebuild evictable projections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalTimelineEntry {
    pub(crate) event_id: String,
    pub(crate) chat_id: String,
    pub(crate) branch_id: String,
    pub(crate) sequence: u64,
    pub(crate) kind: String,
}

/// The LocalSqlite implementation of the trusted core's history commit port.
#[derive(Clone)]
pub struct LocalHistoryStore {
    connection: Arc<Mutex<Connection>>,
}

impl LocalHistoryStore {
    /// Opens a local SQLite database with foreign keys and durable WAL commits.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;",
        )?;
        create_schema(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Atomically verifies the expected head and persists the complete batch.
    pub fn commit(&self, batch: &CommitBatch) -> Result<CommitOutcome, StoreError> {
        validate_batch(batch)?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(deduplication) = &batch.deduplication {
            if let Some((request_hash, receipt)) = load_deduplication(&transaction, deduplication)?
            {
                if request_hash == deduplication.request_hash {
                    transaction.commit()?;
                    return Ok(CommitOutcome::Existing(receipt));
                }
                return Err(StoreError::DeduplicationKeyReused);
            }
        }

        let current_head = stream_head(&transaction, &batch.chat_id, &batch.branch_id)?;
        if current_head != batch.expected_head {
            return Err(StoreError::HeadConflict {
                expected: batch.expected_head,
                actual: current_head,
            });
        }
        let new_head =
            current_head + u64::try_from(batch.events.len()).expect("event limit fits u64");
        create_or_update_stream(&transaction, batch, new_head)?;
        insert_events(&transaction, batch, current_head)?;
        insert_attempt(&transaction, batch)?;
        insert_checkpoint(&transaction, batch, new_head)?;
        let receipt = CommitReceipt {
            head_sequence: new_head,
            event_ids: batch
                .events
                .iter()
                .map(|event| event.event_id.clone())
                .collect(),
            checkpoint_hash: batch
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.state_hash.clone()),
            outbox_ids: batch
                .outbox
                .iter()
                .map(|entry| entry.outbox_id.clone())
                .collect(),
        };
        insert_outbox(&transaction, batch, new_head)?;
        insert_deduplication(&transaction, batch, &receipt)?;
        transaction.commit()?;
        Ok(CommitOutcome::Committed(receipt))
    }

    /// Returns ordered committed outbox entries without exposing uncommitted work.
    pub fn pending_outbox(&self, limit: u32) -> Result<Vec<PendingOutbox>, StoreError> {
        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT outbox_id, chat_id, commit_sequence, destination, payload
             FROM delivery_outbox WHERE delivered = 0
             ORDER BY commit_sequence, outbox_id LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit)], |row| {
            let payload: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                payload,
            ))
        })?;
        rows.map(|row| {
            let (outbox_id, chat_id, commit_sequence, destination, payload) = row?;
            Ok(PendingOutbox {
                outbox_id,
                chat_id,
                commit_sequence: u64::try_from(commit_sequence)
                    .map_err(|_| StoreError::InvalidStoredData)?,
                destination,
                payload: serde_json::from_str(&payload)?,
            })
        })
        .collect()
    }

    /// Marks one already-committed delivery entry as delivered; canonical events stay immutable.
    pub fn mark_outbox_delivered(&self, outbox_id: &str) -> Result<(), StoreError> {
        validate_id(outbox_id)?;
        let connection = self.lock_connection()?;
        let changed = connection.execute(
            "UPDATE delivery_outbox SET delivered = 1 WHERE outbox_id = ?1",
            [outbox_id],
        )?;
        if changed == 0 {
            return Err(StoreError::UnknownOutbox);
        }
        Ok(())
    }

    /// Returns committed event IDs in sequence order for recovery verification.
    pub fn event_ids(&self, chat_id: &str, branch_id: &str) -> Result<Vec<String>, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT event_id FROM semantic_events
             WHERE chat_id = ?1 AND branch_id = ?2 ORDER BY sequence",
        )?;
        Ok(statement
            .query_map(params![chat_id, branch_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns committed semantic events in durable sequence order for reducers.
    ///
    /// This read surface never exposes an append operation, so recovery can fold
    /// state without becoming an alternate history writer.
    pub fn events(&self, chat_id: &str, branch_id: &str) -> Result<Vec<Event>, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT event_id, kind, payload FROM semantic_events
             WHERE chat_id = ?1 AND branch_id = ?2 ORDER BY sequence",
        )?;
        statement
            .query_map(params![chat_id, branch_id], |row| {
                let payload: String = row.get(2)?;
                let payload = serde_json::from_str(&payload).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
                })?;
                Ok(Event { event_id: row.get(0)?, kind: row.get(1)?, payload })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Provides committed records for one projection rebuild.
    pub(crate) fn committed_timeline(
        &self,
        chat_id: &str,
        branch_id: &str,
    ) -> Result<Vec<CanonicalTimelineEntry>, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT event_id, chat_id, branch_id, sequence, kind FROM semantic_events
             WHERE chat_id = ?1 AND branch_id = ?2 ORDER BY sequence",
        )?;
        statement
            .query_map(params![chat_id, branch_id], |row| {
                Ok(CanonicalTimelineEntry {
                    event_id: row.get(0)?,
                    chat_id: row.get(1)?,
                    branch_id: row.get(2)?,
                    sequence: u64::try_from(row.get::<_, i64>(3)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 0))?,
                    kind: row.get(4)?,
                })
            })?
            .map(|row| Ok(row?))
            .collect()
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)
    }
}

fn create_schema(connection: &Connection) -> Result<(), SqlError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_streams (
             chat_id TEXT NOT NULL, branch_id TEXT NOT NULL, head_sequence INTEGER NOT NULL,
             PRIMARY KEY (chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS semantic_events (
             event_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
             sequence INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
             UNIQUE (chat_id, branch_id, sequence),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS attempts (
             attempt_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
             operation_id TEXT NOT NULL, ordinal INTEGER NOT NULL, outcome_class TEXT NOT NULL,
             UNIQUE (chat_id, branch_id, operation_id, ordinal),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS checkpoints (
             chat_id TEXT NOT NULL, branch_id TEXT NOT NULL, committed_sequence INTEGER NOT NULL,
             reducer_version TEXT NOT NULL, state_hash TEXT NOT NULL, frozen_snapshot_ref TEXT,
             PRIMARY KEY (chat_id, branch_id, committed_sequence),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS deduplication (
             key_type TEXT NOT NULL, key TEXT NOT NULL, request_hash TEXT NOT NULL,
             receipt TEXT NOT NULL, PRIMARY KEY (key_type, key)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS delivery_outbox (
             outbox_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
             commit_sequence INTEGER NOT NULL,
             destination TEXT NOT NULL, payload TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0,
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS delivery_outbox_pending
             ON delivery_outbox(delivered, commit_sequence, outbox_id);",
    )
}

fn stream_head(
    transaction: &Transaction<'_>,
    chat_id: &str,
    branch_id: &str,
) -> Result<u64, StoreError> {
    let head: Option<i64> = transaction
        .query_row(
            "SELECT head_sequence FROM chat_streams WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
            |row| row.get(0),
        )
        .optional()?;
    head.map(|head| u64::try_from(head).map_err(|_| StoreError::InvalidStoredData))
        .transpose()
        .map(|head| head.unwrap_or(0))
}

fn create_or_update_stream(
    transaction: &Transaction<'_>,
    batch: &CommitBatch,
    new_head: u64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO chat_streams(chat_id, branch_id, head_sequence) VALUES (?1, ?2, ?3)
         ON CONFLICT(chat_id, branch_id) DO UPDATE SET head_sequence = excluded.head_sequence",
        params![
            batch.chat_id,
            batch.branch_id,
            i64::try_from(new_head).map_err(|_| StoreError::InvalidStoredData)?
        ],
    )?;
    Ok(())
}

fn insert_events(
    transaction: &Transaction<'_>,
    batch: &CommitBatch,
    old_head: u64,
) -> Result<(), StoreError> {
    for (offset, event) in batch.events.iter().enumerate() {
        let sequence = old_head + u64::try_from(offset).expect("batch length fits u64") + 1;
        transaction.execute(
            "INSERT INTO semantic_events(event_id, chat_id, branch_id, sequence, kind, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.event_id,
                batch.chat_id,
                batch.branch_id,
                i64::try_from(sequence).map_err(|_| StoreError::InvalidStoredData)?,
                event.kind,
                serde_json::to_string(&event.payload)?,
            ],
        )?;
    }
    Ok(())
}

fn insert_attempt(transaction: &Transaction<'_>, batch: &CommitBatch) -> Result<(), StoreError> {
    let Some(attempt) = &batch.attempt else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO attempts(attempt_id, chat_id, branch_id, operation_id, ordinal, outcome_class)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            attempt.attempt_id,
            batch.chat_id,
            batch.branch_id,
            attempt.operation_id,
            i64::from(attempt.ordinal),
            attempt.outcome_class
        ],
    )?;
    Ok(())
}

fn insert_checkpoint(
    transaction: &Transaction<'_>,
    batch: &CommitBatch,
    head: u64,
) -> Result<(), StoreError> {
    let Some(checkpoint) = &batch.checkpoint else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO checkpoints(chat_id, branch_id, committed_sequence, reducer_version, state_hash, frozen_snapshot_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![batch.chat_id, batch.branch_id, i64::try_from(head).map_err(|_| StoreError::InvalidStoredData)?, checkpoint.reducer_version, checkpoint.state_hash, checkpoint.frozen_snapshot_ref],
    )?;
    Ok(())
}

fn insert_outbox(
    transaction: &Transaction<'_>,
    batch: &CommitBatch,
    head: u64,
) -> Result<(), StoreError> {
    for entry in &batch.outbox {
        transaction.execute(
            "INSERT INTO delivery_outbox(outbox_id, chat_id, branch_id, commit_sequence, destination, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.outbox_id,
                batch.chat_id,
                batch.branch_id,
                i64::try_from(head).map_err(|_| StoreError::InvalidStoredData)?,
                entry.destination,
                serde_json::to_string(&entry.payload)?
            ],
        )?;
    }
    Ok(())
}

fn insert_deduplication(
    transaction: &Transaction<'_>,
    batch: &CommitBatch,
    receipt: &CommitReceipt,
) -> Result<(), StoreError> {
    let Some(deduplication) = &batch.deduplication else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO deduplication(key_type, key, request_hash, receipt) VALUES (?1, ?2, ?3, ?4)",
        params![
            deduplication.key_type,
            deduplication.key,
            deduplication.request_hash,
            serde_json::to_string(receipt)?
        ],
    )?;
    Ok(())
}

fn load_deduplication(
    transaction: &Transaction<'_>,
    deduplication: &Deduplication,
) -> Result<Option<(String, CommitReceipt)>, StoreError> {
    transaction
        .query_row(
            "SELECT request_hash, receipt FROM deduplication WHERE key_type = ?1 AND key = ?2",
            params![deduplication.key_type, deduplication.key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(request_hash, receipt)| Ok((request_hash, serde_json::from_str(&receipt)?)))
        .transpose()
}

fn validate_batch(batch: &CommitBatch) -> Result<(), StoreError> {
    validate_id(&batch.chat_id)?;
    validate_id(&batch.branch_id)?;
    if batch.events.is_empty() || batch.events.len() > MAX_EVENTS_PER_COMMIT {
        return Err(StoreError::InvalidEventBatch);
    }
    let mut event_ids = BTreeSet::new();
    let mut outbox_ids = BTreeSet::new();
    let bytes = serde_json::to_vec(batch)?.len();
    if bytes > MAX_COMMIT_BYTES {
        return Err(StoreError::CommitTooLarge);
    }
    for event in &batch.events {
        validate_id(&event.event_id)?;
        validate_nonempty(&event.kind)?;
        validate_payload(&event.payload)?;
        if !event_ids.insert(&event.event_id) {
            return Err(StoreError::DuplicateEventInBatch);
        }
    }
    if let Some(attempt) = &batch.attempt {
        validate_id(&attempt.attempt_id)?;
        validate_id(&attempt.operation_id)?;
        validate_nonempty(&attempt.outcome_class)?;
    }
    if let Some(checkpoint) = &batch.checkpoint {
        validate_nonempty(&checkpoint.reducer_version)?;
        validate_nonempty(&checkpoint.state_hash)?;
        if let Some(reference) = &checkpoint.frozen_snapshot_ref {
            validate_id(reference)?;
        }
    }
    if let Some(deduplication) = &batch.deduplication {
        validate_nonempty(&deduplication.key_type)?;
        validate_id(&deduplication.key)?;
        validate_nonempty(&deduplication.request_hash)?;
    }
    for entry in &batch.outbox {
        validate_id(&entry.outbox_id)?;
        validate_nonempty(&entry.destination)?;
        validate_payload(&entry.payload)?;
        if !outbox_ids.insert(&entry.outbox_id) {
            return Err(StoreError::DuplicateOutboxInBatch);
        }
    }
    Ok(())
}

fn validate_payload(payload: &Value) -> Result<(), StoreError> {
    match payload {
        Value::Object(fields) => {
            for (key, value) in fields {
                let normalized_key = key.to_ascii_lowercase();
                if normalized_key.contains("secret")
                    || normalized_key.contains("token")
                    || normalized_key.contains("lease")
                {
                    return Err(StoreError::ForbiddenSecretMaterial);
                }
                validate_payload(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_payload(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<(), StoreError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidId)
    }
}

fn validate_nonempty(value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 256 {
        Err(StoreError::InvalidText)
    } else {
        Ok(())
    }
}

/// Errors that prevent an acknowledged local semantic commit.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Filesystem evidence operations could not durably complete.
    #[error("local store filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite did not durably complete the requested operation.
    #[error("local history database failed: {0}")]
    Sql(#[from] SqlError),
    /// A stored or requested JSON representation is invalid.
    #[error("local history JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// The in-process connection mutex has been poisoned.
    #[error("local history connection is unavailable after a previous panic")]
    PoisonedConnection,
    /// The durable stream head differs from the caller's expected state.
    #[error("stream head conflict: expected {expected}, found {actual}")]
    HeadConflict { expected: u64, actual: u64 },
    /// Duplicate idempotency keys may only be reused with the same request hash.
    #[error("deduplication key was reused with a different request hash")]
    DeduplicationKeyReused,
    /// A transaction must carry a bounded non-empty semantic event batch.
    #[error("commit must contain between 1 and 64 semantic events")]
    InvalidEventBatch,
    /// Stable IDs are bounded logical IDs, never paths or opaque secrets.
    #[error("identifier must be 1-128 ASCII letters, digits, '.', '_' or '-')")]
    InvalidId,
    /// Free-form classification and type fields are bounded but may not be empty.
    #[error("text field must contain 1-256 bytes")]
    InvalidText,
    /// Event payloads and outbox rows may not hold secret values or material.
    #[error("semantic history must not contain secret, token, or lease material")]
    ForbiddenSecretMaterial,
    /// A single transaction may not exceed the one MiB bounded-commit policy.
    #[error("commit exceeds the one MiB bound")]
    CommitTooLarge,
    /// An event identity cannot occur twice inside one proposed transaction.
    #[error("event ID is duplicated inside the commit batch")]
    DuplicateEventInBatch,
    /// An outbox identity cannot occur twice inside one proposed transaction.
    #[error("outbox ID is duplicated inside the commit batch")]
    DuplicateOutboxInBatch,
    /// Corrupt integer values from the database are not interpreted optimistically.
    #[error("local history contains an invalid stored numeric value")]
    InvalidStoredData,
    /// A caller attempted to acknowledge a nonexistent outbox row.
    #[error("outbox entry does not exist")]
    UnknownOutbox,
    /// A prepared artifact token is absent or no longer valid.
    #[error("prepared artifact token does not exist")]
    UnknownArtifactToken,
    /// Prepared bytes may only be finalized for one semantic event.
    #[error("prepared artifact token is already finalized for another event")]
    ArtifactTokenAlreadyFinalized,
    /// No finalized metadata uses this artifact ID.
    #[error("artifact does not exist")]
    UnknownArtifact,
    /// Artifact bytes do not match their immutable metadata.
    #[error("artifact object is missing or corrupt")]
    CorruptArtifact,
    /// A backup destination must not be nested under the store it snapshots.
    #[error("backup destination must be outside the local store root")]
    BackupLocationInsideStore,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn store() -> (LocalHistoryStore, std::path::PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aworkit-ledger-{nonce}.sqlite"));
        (LocalHistoryStore::open(&path).expect("store"), path)
    }

    fn batch(expected_head: u64) -> CommitBatch {
        CommitBatch {
            chat_id: "chat_01".into(),
            branch_id: "main".into(),
            expected_head,
            events: vec![Event {
                event_id: format!("event_{expected_head}"),
                kind: "input.accepted".into(),
                payload: serde_json::json!({"schemaVersion": 1, "text": "hello"}),
            }],
            attempt: Some(Attempt {
                attempt_id: format!("attempt_{expected_head}"),
                operation_id: "input".into(),
                ordinal: u32::try_from(expected_head + 1).expect("ordinal"),
                outcome_class: "accepted".into(),
            }),
            checkpoint: Some(Checkpoint {
                reducer_version: "v1".into(),
                state_hash: format!("state_{expected_head}"),
                frozen_snapshot_ref: None,
            }),
            deduplication: Some(Deduplication {
                key_type: "command".into(),
                key: format!("key_{expected_head}"),
                request_hash: format!("hash_{expected_head}"),
            }),
            outbox: vec![OutboxEntry {
                outbox_id: format!("outbox_{expected_head}"),
                destination: "desktop".into(),
                payload: serde_json::json!({"schemaVersion": 1}),
            }],
        }
    }

    #[test]
    fn commits_events_attempt_checkpoint_dedup_and_outbox_together() {
        let (store, path) = store();
        let committed = store.commit(&batch(0)).expect("commit");
        assert!(matches!(
            committed,
            CommitOutcome::Committed(CommitReceipt {
                head_sequence: 1,
                ..
            })
        ));
        assert_eq!(
            store.event_ids("chat_01", "main").expect("events"),
            vec!["event_0"]
        );
        assert_eq!(store.pending_outbox(8).expect("outbox").len(), 1);
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn same_deduplication_key_returns_the_original_receipt() {
        let (store, path) = store();
        let batch = batch(0);
        let first = store.commit(&batch).expect("first");
        let second = store.commit(&batch).expect("retry");
        assert_eq!(
            first,
            match second {
                CommitOutcome::Existing(receipt) => CommitOutcome::Committed(receipt),
                CommitOutcome::Committed(_) => panic!("expected duplicate"),
            }
        );
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn stale_head_leaves_no_partial_transition() {
        let (store, path) = store();
        store.commit(&batch(0)).expect("first");
        let mut stale = batch(0);
        stale.events[0].event_id = "event_stale".into();
        stale.deduplication.as_mut().expect("deduplication").key = "key_stale".into();
        let error = store.commit(&stale).expect_err("stale head");
        assert!(matches!(
            error,
            StoreError::HeadConflict {
                expected: 0,
                actual: 1
            }
        ));
        assert_eq!(store.event_ids("chat_01", "main").expect("events").len(), 1);
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn rejects_secret_material_before_writing() {
        let (store, path) = store();
        let mut batch = batch(0);
        batch.events[0].payload = serde_json::json!({"secret": "not allowed"});
        assert!(matches!(
            store.commit(&batch),
            Err(StoreError::ForbiddenSecretMaterial)
        ));
        assert!(
            store
                .event_ids("chat_01", "main")
                .expect("events")
                .is_empty()
        );
        fs::remove_file(path).expect("cleanup");
    }
}
