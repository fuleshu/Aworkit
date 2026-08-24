//! SQLite-backed local semantic history and its single atomic commit port.
//!
//! The adapter has no independent event, checkpoint, outbox, or artifact-finalize
//! mutation. A receipt is returned only after the complete backend-bound batch is
//! committed, or after an uncertain commit is verified from a fresh connection.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use aworkit_protocol::{
    AttemptV1, CheckpointV1, CommitBatchV1, CommitOutcomeV1, CommitReceiptV1, DedupV1, EventV1,
    HistoryBackendV1, HistoryPortErrorV1, LocalHistoryCommitPort, OutboxV1, PendingOutboxV1,
    StableId,
};
use rusqlite::{
    Connection, Error as SqlError, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    database::{open_history_database, quarantine, quarantine_reason},
    maintenance::MaintenanceGate,
};

const MAX_EVENTS_PER_COMMIT: usize = 64;
const MAX_ATTEMPTS_PER_COMMIT: usize = 64;
const MAX_OUTBOX_PER_COMMIT: usize = 64;
const MAX_ARTIFACTS_PER_COMMIT: usize = 64;
const MAX_COMMIT_BYTES: usize = 1024 * 1024;
const MAX_PAGE_SIZE: u32 = 512;
const SUPPORTED_SEMANTIC_SCHEMA: u16 = 1;

/// Legacy v1 facade retained for callers while process-neutral ports use
/// [`aworkit_protocol::EventV1`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub kind: String,
    pub payload: Value,
}

/// Legacy v1 facade for one execution attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub attempt_id: String,
    pub operation_id: String,
    pub ordinal: u32,
    pub outcome_class: String,
}

/// Legacy v1 facade for one reducer checkpoint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub reducer_version: String,
    pub state_hash: String,
    pub frozen_snapshot_ref: Option<String>,
}

/// Legacy idempotency facade. `request_hash` is accepted for compatibility but
/// is never trusted; the adapter hashes the complete canonical batch itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Deduplication {
    pub key_type: String,
    pub key: String,
    pub request_hash: String,
}

/// Legacy delivery facade.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub outbox_id: String,
    pub destination: String,
    pub payload: Value,
}

/// Legacy local-only batch facade. New cross-process callers use
/// [`CommitBatchV1`], which also carries backend/run/aggregate/artifact facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitBatch {
    pub chat_id: String,
    pub branch_id: String,
    pub expected_head: u64,
    pub events: Vec<Event>,
    pub attempt: Option<Attempt>,
    pub checkpoint: Option<Checkpoint>,
    pub deduplication: Option<Deduplication>,
    pub outbox: Vec<OutboxEntry>,
}

/// Legacy receipt facade.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub head_sequence: u64,
    pub event_ids: Vec<String>,
    pub checkpoint_hash: Option<String>,
    pub outbox_ids: Vec<String>,
}

/// Legacy result facade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitOutcome {
    Committed(CommitReceipt),
    Existing(CommitReceipt),
}

/// A committed outbox row awaiting idempotent delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOutbox {
    pub outbox_id: String,
    pub chat_id: String,
    pub commit_sequence: u64,
    pub destination: String,
    pub payload: Value,
}

/// Canonical event material used only to rebuild disposable projections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalTimelineEntry {
    pub(crate) event_id: String,
    pub(crate) chat_id: String,
    pub(crate) branch_id: String,
    pub(crate) sequence: u64,
    pub(crate) schema_version: u16,
    pub(crate) kind: String,
    pub(crate) payload: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalTimelinePage {
    pub(crate) entries: Vec<CanonicalTimelineEntry>,
    pub(crate) next_cursor: Option<u64>,
}

/// The LocalSqlite adapter for the process-neutral history commit port.
#[derive(Clone)]
pub struct LocalHistoryStore {
    path: Arc<PathBuf>,
    root: Arc<PathBuf>,
    gate: MaintenanceGate,
    connection: Arc<Mutex<Connection>>,
}

impl LocalHistoryStore {
    /// Opens a local SQLite history database and upgrades supported legacy
    /// tables before releasing the writer.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = absolute(path.as_ref())?;
        let root = path
            .parent()
            .ok_or(StoreError::InvalidStorePath)?
            .to_path_buf();
        fs::create_dir_all(&root)?;
        let gate = MaintenanceGate::for_root(&root)?;
        let _lease = gate.shared()?;
        let connection = open_history_database(&path)?;
        Ok(Self {
            path: Arc::new(path),
            root: Arc::new(root),
            gate,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) fn database_path(&self) -> &Path {
        self.path.as_ref()
    }

    /// Compatibility commit. The caller-supplied request digest is ignored and
    /// aggregate fencing is inferred from the current legacy stream semantics.
    pub fn commit(&self, batch: &CommitBatch) -> Result<CommitOutcome, StoreError> {
        let protocol_batch = legacy_batch(batch)?;
        let outcome = self.commit_inner(&protocol_batch, false)?;
        Ok(match outcome {
            CommitOutcomeV1::Committed(receipt) => {
                CommitOutcome::Committed(legacy_receipt(receipt))
            }
            CommitOutcomeV1::Existing(receipt) => CommitOutcome::Existing(legacy_receipt(receipt)),
        })
    }

    /// Commits the complete process-neutral LocalSqlite contract.
    pub fn commit_v1(&self, batch: &CommitBatchV1) -> Result<CommitOutcomeV1, StoreError> {
        self.commit_inner(batch, true)
    }

    fn commit_inner(
        &self,
        batch: &CommitBatchV1,
        enforce_aggregate_version: bool,
    ) -> Result<CommitOutcomeV1, StoreError> {
        validate_batch(batch)?;
        let request_hash = canonical_request_hash(batch)?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock_connection()?;
        if let Some(reason) = quarantine_reason(&connection)? {
            return Err(StoreError::StoreQuarantined { reason });
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(deduplication) = &batch.deduplication {
            if let Some(existing) = load_deduplication(&transaction, deduplication)? {
                if existing.request_hash != request_hash
                    || existing.chat_id != batch.chat_id.as_str()
                    || existing.branch_id != batch.branch_id.as_str()
                {
                    return Err(StoreError::DeduplicationKeyReused);
                }
                transaction.commit()?;
                return Ok(CommitOutcomeV1::Existing(existing.receipt));
            }
        }

        let stream = stream_state(
            &transaction,
            batch.chat_id.as_str(),
            batch.branch_id.as_str(),
        )?;
        let (current_head, current_aggregate) = match stream {
            Some(state) => {
                if state.run_id != batch.run_id.as_str() {
                    return Err(StoreError::BackendBindingMismatch);
                }
                (state.head_sequence, state.aggregate_version)
            }
            None => (0, 0),
        };
        if current_head != batch.expected_head {
            return Err(StoreError::HeadConflict {
                expected: batch.expected_head,
                actual: current_head,
            });
        }
        if enforce_aggregate_version && current_aggregate != batch.expected_aggregate_version {
            return Err(StoreError::AggregateVersionConflict {
                expected: batch.expected_aggregate_version,
                actual: current_aggregate,
            });
        }

        let event_count =
            u64::try_from(batch.events.len()).map_err(|_| StoreError::InvalidStoredData)?;
        let new_head = current_head
            .checked_add(event_count)
            .ok_or(StoreError::InvalidStoredData)?;
        let new_aggregate = current_aggregate
            .checked_add(1)
            .ok_or(StoreError::InvalidStoredData)?;
        upsert_stream(&transaction, batch, new_head, new_aggregate)?;
        insert_events(&transaction, batch, current_head)?;
        #[cfg(test)]
        inject_fault(CommitFaultPoint::AfterEvents)?;
        insert_attempts(&transaction, batch)?;
        insert_checkpoint(&transaction, batch, new_head)?;
        let outbox_cursors = insert_outbox(&transaction, batch, new_head)?;
        finalize_artifacts(&transaction, &self.root, batch)?;

        let receipt = CommitReceiptV1 {
            head_sequence: new_head,
            aggregate_version: new_aggregate,
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
            request_hash: request_hash.clone(),
        };
        insert_deduplication(&transaction, batch, &request_hash, &receipt)?;
        #[cfg(test)]
        inject_fault(CommitFaultPoint::BeforeCommit)?;

        match transaction.commit() {
            Ok(()) => {
                #[cfg(test)]
                inject_fault(CommitFaultPoint::AfterCommitBeforeAck)?;
                Ok(CommitOutcomeV1::Committed(receipt))
            }
            Err(commit_error) => {
                drop(connection);
                match self.verify_durable_commit(batch, &request_hash, &receipt, &outbox_cursors)? {
                    Verification::Exact => Ok(CommitOutcomeV1::Committed(receipt)),
                    Verification::Absent => Err(StoreError::Sql(commit_error)),
                    Verification::Partial(reason) => {
                        let connection = Connection::open(self.path.as_ref())?;
                        quarantine(&connection, &reason)?;
                        Err(StoreError::AmbiguousCommitQuarantined { reason })
                    }
                }
            }
        }
    }

    /// Returns ordered committed outbox entries from the beginning.
    pub fn pending_outbox(&self, limit: u32) -> Result<Vec<PendingOutbox>, StoreError> {
        Ok(self
            .pending_outbox_v1(0, limit)?
            .into_iter()
            .map(|entry| PendingOutbox {
                outbox_id: entry.outbox_id.to_string(),
                chat_id: entry.chat_id.to_string(),
                commit_sequence: entry.commit_sequence,
                destination: entry.destination,
                payload: entry.payload,
            })
            .collect())
    }

    /// Returns a cursor-bounded page of committed, undelivered outbox rows.
    pub fn pending_outbox_v1(
        &self,
        after_cursor: u64,
        limit: u32,
    ) -> Result<Vec<PendingOutboxV1>, StoreError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT outbox_id, chat_id, branch_id, commit_sequence, delivery_cursor,
                    destination, schema_version, payload, payload_hash
             FROM delivery_outbox
             WHERE delivered = 0 AND delivery_cursor > ?1
             ORDER BY delivery_cursor LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                to_i64(after_cursor)?,
                i64::from(limit.clamp(1, MAX_PAGE_SIZE))
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                outbox_id,
                chat_id,
                branch_id,
                sequence,
                cursor,
                destination,
                schema,
                payload,
                hash,
            ) = row?;
            let parsed_payload: Value = serde_json::from_str(&payload)?;
            if sha256_hex(payload.as_bytes()) != hash {
                return Err(StoreError::CorruptOutbox);
            }
            entries.push(PendingOutboxV1 {
                outbox_id: stable(outbox_id)?,
                chat_id: stable(chat_id)?,
                branch_id: stable(branch_id)?,
                commit_sequence: from_i64(sequence)?,
                delivery_cursor: from_i64(cursor)?,
                destination,
                schema_version: u16::try_from(schema).map_err(|_| StoreError::InvalidStoredData)?,
                payload: parsed_payload,
                payload_hash: hash,
            });
        }
        Ok(entries)
    }

    /// Compatibility acknowledgement without a caller cursor.
    pub fn mark_outbox_delivered(&self, outbox_id: &str) -> Result<(), StoreError> {
        validate_id(outbox_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        if quarantine_reason(&connection)?.is_some() {
            return Err(StoreError::StoreQuarantined {
                reason: "history store is quarantined".into(),
            });
        }
        let changed = connection.execute(
            "UPDATE delivery_outbox SET delivered = 1 WHERE outbox_id = ?1",
            [outbox_id],
        )?;
        if changed == 0 {
            return Err(StoreError::UnknownOutbox);
        }
        Ok(())
    }

    /// Cursor-fenced acknowledgement used by the process-neutral port.
    pub fn mark_outbox_delivered_v1(
        &self,
        outbox_id: &StableId,
        expected_cursor: u64,
    ) -> Result<(), StoreError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        if let Some(reason) = quarantine_reason(&connection)? {
            return Err(StoreError::StoreQuarantined { reason });
        }
        let changed = connection.execute(
            "UPDATE delivery_outbox SET delivered = 1
             WHERE outbox_id = ?1 AND delivery_cursor = ?2",
            params![outbox_id.as_str(), to_i64(expected_cursor)?],
        )?;
        if changed == 0 {
            let actual: Option<i64> = connection
                .query_row(
                    "SELECT delivery_cursor FROM delivery_outbox WHERE outbox_id = ?1",
                    [outbox_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            return match actual {
                None => Err(StoreError::UnknownOutbox),
                Some(actual) => Err(StoreError::OutboxCursorConflict {
                    expected: expected_cursor,
                    actual: from_i64(actual)?,
                }),
            };
        }
        Ok(())
    }

    /// Returns committed event IDs in sequence order.
    pub fn event_ids(&self, chat_id: &str, branch_id: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .committed_timeline_page(chat_id, branch_id, 0, MAX_PAGE_SIZE)?
            .entries
            .into_iter()
            .map(|entry| entry.event_id)
            .collect())
    }

    /// Returns committed semantic events in durable sequence order.
    pub fn events(&self, chat_id: &str, branch_id: &str) -> Result<Vec<Event>, StoreError> {
        let mut cursor = 0;
        let mut events = Vec::new();
        loop {
            let page = self.committed_timeline_page(chat_id, branch_id, cursor, MAX_PAGE_SIZE)?;
            events.extend(page.entries.into_iter().map(|entry| Event {
                event_id: entry.event_id,
                kind: entry.kind,
                payload: entry.payload,
            }));
            let Some(next) = page.next_cursor else { break };
            cursor = next;
        }
        Ok(events)
    }

    pub(crate) fn committed_timeline_page(
        &self,
        chat_id: &str,
        branch_id: &str,
        after_sequence: u64,
        limit: u32,
    ) -> Result<CanonicalTimelinePage, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let mut statement = connection.prepare(
            "SELECT event_id, chat_id, branch_id, sequence, schema_version, kind, payload
             FROM semantic_events
             WHERE chat_id = ?1 AND branch_id = ?2 AND sequence > ?3
             ORDER BY sequence LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                chat_id,
                branch_id,
                to_i64(after_sequence)?,
                i64::from(limit)
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        let mut entries = Vec::new();
        for row in rows {
            let (event_id, chat_id, branch_id, sequence, schema_version, kind, payload) = row?;
            entries.push(CanonicalTimelineEntry {
                event_id,
                chat_id,
                branch_id,
                sequence: from_i64(sequence)?,
                schema_version: u16::try_from(schema_version)
                    .map_err(|_| StoreError::InvalidStoredData)?,
                kind,
                payload: serde_json::from_str(&payload)?,
            });
        }
        let next_cursor = (entries.len() == usize::try_from(limit).expect("u32 fits usize"))
            .then(|| entries.last().map(|entry| entry.sequence))
            .flatten();
        Ok(CanonicalTimelinePage {
            entries,
            next_cursor,
        })
    }

    fn verify_durable_commit(
        &self,
        batch: &CommitBatchV1,
        request_hash: &str,
        expected_receipt: &CommitReceiptV1,
        expected_outbox_cursors: &[u64],
    ) -> Result<Verification, StoreError> {
        let connection = Connection::open(self.path.as_ref())?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;

        let mut dedup_exact = batch.deduplication.is_none();
        let mut dedup_present = false;
        if let Some(deduplication) = &batch.deduplication {
            let stored: Option<(String, String, String, String)> = connection
                .query_row(
                    "SELECT request_hash, chat_id, branch_id, receipt FROM deduplication
                     WHERE key_type = ?1 AND key = ?2",
                    params![deduplication.key_type, deduplication.key.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if let Some((stored_hash, chat_id, branch_id, receipt_json)) = stored {
                dedup_present = true;
                let stored_receipt: CommitReceiptV1 = serde_json::from_str(&receipt_json)?;
                if stored_hash == request_hash
                    && chat_id == batch.chat_id.as_str()
                    && branch_id == batch.branch_id.as_str()
                    && stored_receipt == *expected_receipt
                {
                    dedup_exact = true;
                } else {
                    return Ok(Verification::Partial(
                        "deduplication receipt differs after uncertain commit".into(),
                    ));
                }
            }
        }

        let stream: Option<(String, i64, i64)> = connection
            .query_row(
                "SELECT run_id, head_sequence, aggregate_version FROM chat_streams
                 WHERE chat_id = ?1 AND branch_id = ?2",
                params![batch.chat_id.as_str(), batch.branch_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let present_events = count_present_ids(
            &connection,
            "semantic_events",
            "event_id",
            &expected_receipt.event_ids,
        )?;
        let present_outbox = count_present_ids(
            &connection,
            "delivery_outbox",
            "outbox_id",
            &expected_receipt.outbox_ids,
        )?;
        let no_evidence = stream.as_ref().is_none_or(|(_, head, aggregate)| {
            from_i64(*head).unwrap_or(u64::MAX) == batch.expected_head
                && from_i64(*aggregate).unwrap_or(u64::MAX) == batch.expected_aggregate_version
        }) && present_events == 0
            && present_outbox == 0
            && !dedup_present;
        if no_evidence {
            return Ok(Verification::Absent);
        }
        let exact_stream = stream.is_some_and(|(run_id, head, aggregate)| {
            run_id == batch.run_id.as_str()
                && from_i64(head).ok() == Some(expected_receipt.head_sequence)
                && from_i64(aggregate).ok() == Some(expected_receipt.aggregate_version)
        });
        let cursors_match = load_outbox_cursors(&connection, &expected_receipt.outbox_ids)?
            == expected_outbox_cursors;
        if dedup_exact
            && exact_stream
            && present_events == expected_receipt.event_ids.len()
            && present_outbox == expected_receipt.outbox_ids.len()
            && cursors_match
            && events_match(&connection, batch)?
            && attempts_match(&connection, batch)?
            && checkpoint_matches(&connection, batch, expected_receipt.head_sequence)?
            && outbox_rows_match(&connection, batch, expected_outbox_cursors)?
            && artifacts_match(&connection, batch)?
            && artifact_objects_match(&self.root, batch)?
        {
            Ok(Verification::Exact)
        } else {
            Ok(Verification::Partial(
                "partial or conflicting rows found after uncertain commit".into(),
            ))
        }
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)
    }
}

impl LocalHistoryCommitPort for LocalHistoryStore {
    fn commit(&self, batch: &CommitBatchV1) -> Result<CommitOutcomeV1, HistoryPortErrorV1> {
        self.commit_v1(batch).map_err(history_port_error)
    }

    fn pending_outbox(
        &self,
        after_cursor: u64,
        limit: u32,
    ) -> Result<Vec<PendingOutboxV1>, HistoryPortErrorV1> {
        self.pending_outbox_v1(after_cursor, limit)
            .map_err(history_port_error)
    }

    fn mark_outbox_delivered(
        &self,
        outbox_id: &StableId,
        expected_cursor: u64,
    ) -> Result<(), HistoryPortErrorV1> {
        self.mark_outbox_delivered_v1(outbox_id, expected_cursor)
            .map_err(history_port_error)
    }
}

#[derive(Clone, Debug)]
struct StreamState {
    run_id: String,
    head_sequence: u64,
    aggregate_version: u64,
}

#[derive(Clone, Debug)]
struct StoredDeduplication {
    request_hash: String,
    chat_id: String,
    branch_id: String,
    receipt: CommitReceiptV1,
}

enum Verification {
    Exact,
    Absent,
    Partial(String),
}

fn stream_state(
    transaction: &Transaction<'_>,
    chat_id: &str,
    branch_id: &str,
) -> Result<Option<StreamState>, StoreError> {
    transaction
        .query_row(
            "SELECT run_id, head_sequence, aggregate_version FROM chat_streams
             WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?
        .map(|(run_id, head, aggregate)| {
            Ok(StreamState {
                run_id,
                head_sequence: from_i64(head)?,
                aggregate_version: from_i64(aggregate)?,
            })
        })
        .transpose()
}

fn upsert_stream(
    transaction: &Transaction<'_>,
    batch: &CommitBatchV1,
    new_head: u64,
    new_aggregate: u64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO chat_streams(
           chat_id, branch_id, run_id, head_sequence, aggregate_version
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(chat_id, branch_id) DO UPDATE SET
           head_sequence = excluded.head_sequence,
           aggregate_version = excluded.aggregate_version",
        params![
            batch.chat_id.as_str(),
            batch.branch_id.as_str(),
            batch.run_id.as_str(),
            to_i64(new_head)?,
            to_i64(new_aggregate)?,
        ],
    )?;
    Ok(())
}

fn insert_events(
    transaction: &Transaction<'_>,
    batch: &CommitBatchV1,
    old_head: u64,
) -> Result<(), StoreError> {
    for (offset, event) in batch.events.iter().enumerate() {
        let sequence = old_head
            .checked_add(u64::try_from(offset).map_err(|_| StoreError::InvalidStoredData)?)
            .and_then(|value| value.checked_add(1))
            .ok_or(StoreError::InvalidStoredData)?;
        transaction.execute(
            "INSERT INTO semantic_events(
               event_id, chat_id, branch_id, sequence, schema_version, kind, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_id.as_str(),
                batch.chat_id.as_str(),
                batch.branch_id.as_str(),
                to_i64(sequence)?,
                i64::from(event.schema_version),
                event.kind,
                serde_json::to_string(&event.payload)?,
            ],
        )?;
    }
    Ok(())
}

fn insert_attempts(transaction: &Transaction<'_>, batch: &CommitBatchV1) -> Result<(), StoreError> {
    for attempt in &batch.attempts {
        transaction.execute(
            "INSERT INTO attempts(
               attempt_id, chat_id, branch_id, operation_id, ordinal, outcome_class
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                attempt.attempt_id.as_str(),
                batch.chat_id.as_str(),
                batch.branch_id.as_str(),
                attempt.operation_id.as_str(),
                i64::from(attempt.ordinal),
                attempt.outcome_class,
            ],
        )?;
    }
    Ok(())
}

fn insert_checkpoint(
    transaction: &Transaction<'_>,
    batch: &CommitBatchV1,
    head: u64,
) -> Result<(), StoreError> {
    let Some(checkpoint) = &batch.checkpoint else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO checkpoints(
           chat_id, branch_id, committed_sequence, reducer_version, state_hash,
           frozen_snapshot_ref
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            batch.chat_id.as_str(),
            batch.branch_id.as_str(),
            to_i64(head)?,
            checkpoint.reducer_version,
            checkpoint.state_hash,
            checkpoint
                .frozen_snapshot_ref
                .as_ref()
                .map(StableId::as_str),
        ],
    )?;
    Ok(())
}

fn insert_outbox(
    transaction: &Transaction<'_>,
    batch: &CommitBatchV1,
    head: u64,
) -> Result<Vec<u64>, StoreError> {
    let current_cursor: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(delivery_cursor), 0) FROM delivery_outbox",
        [],
        |row| row.get(0),
    )?;
    let mut cursor = from_i64(current_cursor)?;
    let mut cursors = Vec::with_capacity(batch.outbox.len());
    for entry in &batch.outbox {
        cursor = cursor.checked_add(1).ok_or(StoreError::InvalidStoredData)?;
        let payload = serde_json::to_string(&entry.payload)?;
        let payload_hash = sha256_hex(payload.as_bytes());
        transaction.execute(
            "INSERT INTO delivery_outbox(
               outbox_id, chat_id, branch_id, commit_sequence, delivery_cursor,
               destination, schema_version, payload, payload_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                entry.outbox_id.as_str(),
                batch.chat_id.as_str(),
                batch.branch_id.as_str(),
                to_i64(head)?,
                to_i64(cursor)?,
                entry.destination,
                i64::from(entry.schema_version),
                payload,
                payload_hash,
            ],
        )?;
        cursors.push(cursor);
    }
    Ok(cursors)
}

fn finalize_artifacts(
    transaction: &Transaction<'_>,
    root: &Path,
    batch: &CommitBatchV1,
) -> Result<(), StoreError> {
    let event_ids: BTreeSet<&str> = batch
        .events
        .iter()
        .map(|event| event.event_id.as_str())
        .collect();
    for reference in &batch.prepared_artifacts {
        if !event_ids.contains(reference.origin_event_id.as_str()) {
            return Err(StoreError::ArtifactOriginOutsideCommit);
        }
        let prepared = transaction
            .query_row(
                "SELECT artifact_id, content_hash, byte_size, media_type, logical_name,
                        staging_generation, finalized_event_id
                 FROM prepared_artifacts WHERE token_id = ?1",
                [reference.token_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::UnknownArtifactToken)?;
        let (artifact_id, hash, size, media_type, logical_name, generation, finalized_event) =
            prepared;
        if artifact_id != reference.artifact_id.as_str()
            || hash != reference.content_hash
            || from_i64(size)? != reference.byte_size
            || from_i64(generation)? != reference.staging_generation
        {
            return Err(StoreError::ArtifactTokenMismatch);
        }
        if finalized_event
            .as_deref()
            .is_some_and(|existing| existing != reference.origin_event_id.as_str())
        {
            return Err(StoreError::ArtifactTokenAlreadyFinalized);
        }
        verify_object(root, &hash, reference.byte_size)?;

        let existing: Option<(String, i64, String, String)> = transaction
            .query_row(
                "SELECT content_hash, byte_size, media_type, logical_name
                 FROM artifacts WHERE artifact_id = ?1",
                [artifact_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((existing_hash, existing_size, existing_media, existing_name)) = existing {
            if existing_hash != hash
                || from_i64(existing_size)? != reference.byte_size
                || existing_media != media_type
                || existing_name != logical_name
            {
                return Err(StoreError::ArtifactIdentityConflict);
            }
        } else {
            transaction.execute(
                "INSERT INTO artifacts(
                   artifact_id, content_hash, byte_size, media_type, logical_name,
                   created_generation, retention_class, availability
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'chat', 'available')",
                params![
                    artifact_id,
                    hash,
                    to_i64(reference.byte_size)?,
                    media_type,
                    logical_name,
                    to_i64(reference.staging_generation)?,
                ],
            )?;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO artifact_references(artifact_id, origin_event_id)
             VALUES (?1, ?2)",
            params![
                reference.artifact_id.as_str(),
                reference.origin_event_id.as_str()
            ],
        )?;
        transaction.execute(
            "UPDATE prepared_artifacts SET finalized_event_id = ?2 WHERE token_id = ?1",
            params![
                reference.token_id.as_str(),
                reference.origin_event_id.as_str()
            ],
        )?;
    }
    Ok(())
}

fn insert_deduplication(
    transaction: &Transaction<'_>,
    batch: &CommitBatchV1,
    request_hash: &str,
    receipt: &CommitReceiptV1,
) -> Result<(), StoreError> {
    let Some(deduplication) = &batch.deduplication else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO deduplication(
           key_type, key, request_hash, chat_id, branch_id, receipt
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            deduplication.key_type,
            deduplication.key.as_str(),
            request_hash,
            batch.chat_id.as_str(),
            batch.branch_id.as_str(),
            serde_json::to_string(receipt)?,
        ],
    )?;
    Ok(())
}

fn load_deduplication(
    transaction: &Transaction<'_>,
    deduplication: &DedupV1,
) -> Result<Option<StoredDeduplication>, StoreError> {
    transaction
        .query_row(
            "SELECT request_hash, chat_id, branch_id, receipt FROM deduplication
             WHERE key_type = ?1 AND key = ?2",
            params![deduplication.key_type, deduplication.key.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(request_hash, chat_id, branch_id, receipt)| {
            Ok(StoredDeduplication {
                request_hash,
                chat_id,
                branch_id,
                receipt: serde_json::from_str(&receipt)?,
            })
        })
        .transpose()
}

fn validate_batch(batch: &CommitBatchV1) -> Result<(), StoreError> {
    if !matches!(batch.backend, HistoryBackendV1::LocalSqlite) {
        return Err(StoreError::WrongHistoryBackend);
    }
    to_i64(batch.expected_head)?;
    to_i64(batch.expected_aggregate_version)?;
    if batch.events.is_empty() || batch.events.len() > MAX_EVENTS_PER_COMMIT {
        return Err(StoreError::InvalidEventBatch);
    }
    if batch.attempts.len() > MAX_ATTEMPTS_PER_COMMIT
        || batch.outbox.len() > MAX_OUTBOX_PER_COMMIT
        || batch.prepared_artifacts.len() > MAX_ARTIFACTS_PER_COMMIT
    {
        return Err(StoreError::CommitCollectionTooLarge);
    }
    if serde_json::to_vec(batch)?.len() > MAX_COMMIT_BYTES {
        return Err(StoreError::CommitTooLarge);
    }

    let mut event_ids = BTreeSet::new();
    let mut attempt_ids = BTreeSet::new();
    let mut outbox_ids = BTreeSet::new();
    let mut token_ids = BTreeSet::new();
    for event in &batch.events {
        if event.schema_version != SUPPORTED_SEMANTIC_SCHEMA {
            return Err(StoreError::UnsupportedSchemaVersion(event.schema_version));
        }
        validate_text(&event.kind)?;
        validate_payload(&event.payload)?;
        if !event_ids.insert(event.event_id.as_str()) {
            return Err(StoreError::DuplicateEventInBatch);
        }
    }
    for attempt in &batch.attempts {
        validate_text(&attempt.outcome_class)?;
        if !attempt_ids.insert(attempt.attempt_id.as_str()) {
            return Err(StoreError::DuplicateAttemptInBatch);
        }
    }
    if let Some(checkpoint) = &batch.checkpoint {
        validate_text(&checkpoint.reducer_version)?;
        validate_hash_or_id(&checkpoint.state_hash)?;
    }
    if let Some(deduplication) = &batch.deduplication {
        validate_text(&deduplication.key_type)?;
    }
    for entry in &batch.outbox {
        if entry.schema_version != SUPPORTED_SEMANTIC_SCHEMA {
            return Err(StoreError::UnsupportedSchemaVersion(entry.schema_version));
        }
        validate_text(&entry.destination)?;
        validate_payload(&entry.payload)?;
        if !outbox_ids.insert(entry.outbox_id.as_str()) {
            return Err(StoreError::DuplicateOutboxInBatch);
        }
    }
    for reference in &batch.prepared_artifacts {
        validate_sha256(&reference.content_hash)?;
        to_i64(reference.byte_size)?;
        to_i64(reference.staging_generation)?;
        if !token_ids.insert(reference.token_id.as_str()) {
            return Err(StoreError::DuplicateArtifactTokenInBatch);
        }
    }
    Ok(())
}

fn validate_payload(payload: &Value) -> Result<(), StoreError> {
    match payload {
        Value::Object(fields) => {
            for (key, value) in fields {
                let normalized: String = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                if matches!(
                    normalized.as_str(),
                    "secret"
                        | "secrets"
                        | "token"
                        | "accesstoken"
                        | "refreshtoken"
                        | "leasetoken"
                        | "credential"
                        | "credentials"
                        | "password"
                        | "authorization"
                        | "apikey"
                        | "privatekey"
                ) {
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
    stable(value.to_owned()).map(|_| ())
}

fn validate_text(value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        Err(StoreError::InvalidText)
    } else {
        Ok(())
    }
}

fn validate_hash_or_id(value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        Err(StoreError::InvalidText)
    } else {
        Ok(())
    }
}

fn validate_sha256(value: &str) -> Result<(), StoreError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StoreError::InvalidContentHash)
    }
}

fn canonical_request_hash(batch: &CommitBatchV1) -> Result<String, StoreError> {
    Ok(sha256_hex(&serde_json::to_vec(batch)?))
}

fn payload_schema(payload: &Value) -> Result<u16, StoreError> {
    match payload.get("schemaVersion") {
        None => Ok(SUPPORTED_SEMANTIC_SCHEMA),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(StoreError::UnsupportedSchemaVersion(0)),
        Some(_) => Err(StoreError::UnsupportedSchemaVersion(0)),
    }
}

fn legacy_batch(batch: &CommitBatch) -> Result<CommitBatchV1, StoreError> {
    let chat_id = stable(batch.chat_id.clone())?;
    Ok(CommitBatchV1 {
        backend: HistoryBackendV1::LocalSqlite,
        chat_id: chat_id.clone(),
        run_id: chat_id,
        branch_id: stable(batch.branch_id.clone())?,
        expected_head: batch.expected_head,
        expected_aggregate_version: batch.expected_head,
        events: batch
            .events
            .iter()
            .map(|event| {
                Ok(EventV1 {
                    event_id: stable(event.event_id.clone())?,
                    schema_version: payload_schema(&event.payload)?,
                    kind: event.kind.clone(),
                    payload: event.payload.clone(),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?,
        attempts: batch
            .attempt
            .iter()
            .map(|attempt| {
                Ok(AttemptV1 {
                    attempt_id: stable(attempt.attempt_id.clone())?,
                    operation_id: stable(attempt.operation_id.clone())?,
                    ordinal: attempt.ordinal,
                    outcome_class: attempt.outcome_class.clone(),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?,
        checkpoint: batch
            .checkpoint
            .as_ref()
            .map(|checkpoint| {
                Ok::<CheckpointV1, StoreError>(CheckpointV1 {
                    reducer_version: checkpoint.reducer_version.clone(),
                    state_hash: checkpoint.state_hash.clone(),
                    frozen_snapshot_ref: checkpoint
                        .frozen_snapshot_ref
                        .as_ref()
                        .map(|reference| stable(reference.clone()))
                        .transpose()?,
                })
            })
            .transpose()?,
        deduplication: batch
            .deduplication
            .as_ref()
            .map(|deduplication| {
                Ok::<DedupV1, StoreError>(DedupV1 {
                    key_type: deduplication.key_type.clone(),
                    key: stable(deduplication.key.clone())?,
                })
            })
            .transpose()?,
        outbox: batch
            .outbox
            .iter()
            .map(|entry| {
                Ok(OutboxV1 {
                    outbox_id: stable(entry.outbox_id.clone())?,
                    destination: entry.destination.clone(),
                    schema_version: payload_schema(&entry.payload)?,
                    payload: entry.payload.clone(),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?,
        prepared_artifacts: Vec::new(),
    })
}

fn legacy_receipt(receipt: CommitReceiptV1) -> CommitReceipt {
    CommitReceipt {
        head_sequence: receipt.head_sequence,
        event_ids: receipt
            .event_ids
            .into_iter()
            .map(|id| id.to_string())
            .collect(),
        checkpoint_hash: receipt.checkpoint_hash,
        outbox_ids: receipt
            .outbox_ids
            .into_iter()
            .map(|id| id.to_string())
            .collect(),
    }
}

fn verify_object(root: &Path, expected_hash: &str, expected_size: u64) -> Result<(), StoreError> {
    validate_sha256(expected_hash)?;
    let path = root
        .join("objects")
        .join(&expected_hash[..2])
        .join(expected_hash);
    let bytes = fs::read(path).map_err(|_| StoreError::CorruptArtifact)?;
    if u64::try_from(bytes.len()).map_err(|_| StoreError::InvalidStoredData)? != expected_size
        || sha256_hex(&bytes) != expected_hash
    {
        return Err(StoreError::CorruptArtifact);
    }
    Ok(())
}

fn events_match(connection: &Connection, batch: &CommitBatchV1) -> Result<bool, StoreError> {
    for (offset, event) in batch.events.iter().enumerate() {
        let sequence = batch
            .expected_head
            .checked_add(u64::try_from(offset).map_err(|_| StoreError::InvalidStoredData)?)
            .and_then(|value| value.checked_add(1))
            .ok_or(StoreError::InvalidStoredData)?;
        let found: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM semantic_events
                 WHERE event_id = ?1 AND chat_id = ?2 AND branch_id = ?3
                   AND sequence = ?4 AND schema_version = ?5 AND kind = ?6
                   AND payload = ?7",
                params![
                    event.event_id.as_str(),
                    batch.chat_id.as_str(),
                    batch.branch_id.as_str(),
                    to_i64(sequence)?,
                    i64::from(event.schema_version),
                    event.kind,
                    serde_json::to_string(&event.payload)?,
                ],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn attempts_match(connection: &Connection, batch: &CommitBatchV1) -> Result<bool, StoreError> {
    for attempt in &batch.attempts {
        let found: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM attempts
                 WHERE attempt_id = ?1 AND chat_id = ?2 AND branch_id = ?3
                   AND operation_id = ?4 AND ordinal = ?5 AND outcome_class = ?6",
                params![
                    attempt.attempt_id.as_str(),
                    batch.chat_id.as_str(),
                    batch.branch_id.as_str(),
                    attempt.operation_id.as_str(),
                    i64::from(attempt.ordinal),
                    attempt.outcome_class,
                ],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn checkpoint_matches(
    connection: &Connection,
    batch: &CommitBatchV1,
    committed_head: u64,
) -> Result<bool, StoreError> {
    let stored: Option<(String, String, Option<String>)> = connection
        .query_row(
            "SELECT reducer_version, state_hash, frozen_snapshot_ref FROM checkpoints
             WHERE chat_id = ?1 AND branch_id = ?2 AND committed_sequence = ?3",
            params![
                batch.chat_id.as_str(),
                batch.branch_id.as_str(),
                to_i64(committed_head)?,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(match (&batch.checkpoint, stored) {
        (None, None) => true,
        (Some(expected), Some((version, hash, reference))) => {
            version == expected.reducer_version
                && hash == expected.state_hash
                && reference.as_deref()
                    == expected.frozen_snapshot_ref.as_ref().map(StableId::as_str)
        }
        _ => false,
    })
}

fn outbox_rows_match(
    connection: &Connection,
    batch: &CommitBatchV1,
    cursors: &[u64],
) -> Result<bool, StoreError> {
    if batch.outbox.len() != cursors.len() {
        return Ok(false);
    }
    let commit_sequence = batch
        .expected_head
        .checked_add(u64::try_from(batch.events.len()).map_err(|_| StoreError::InvalidStoredData)?)
        .ok_or(StoreError::InvalidStoredData)?;
    for (entry, cursor) in batch.outbox.iter().zip(cursors) {
        let payload = serde_json::to_string(&entry.payload)?;
        let found: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM delivery_outbox
                 WHERE outbox_id = ?1 AND chat_id = ?2 AND branch_id = ?3
                   AND commit_sequence = ?4 AND delivery_cursor = ?5
                   AND destination = ?6 AND schema_version = ?7
                   AND payload = ?8 AND payload_hash = ?9",
                params![
                    entry.outbox_id.as_str(),
                    batch.chat_id.as_str(),
                    batch.branch_id.as_str(),
                    to_i64(commit_sequence)?,
                    to_i64(*cursor)?,
                    entry.destination,
                    i64::from(entry.schema_version),
                    payload,
                    sha256_hex(payload.as_bytes()),
                ],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn artifact_objects_match(root: &Path, batch: &CommitBatchV1) -> Result<bool, StoreError> {
    for reference in &batch.prepared_artifacts {
        if verify_object(root, &reference.content_hash, reference.byte_size).is_err() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn artifacts_match(connection: &Connection, batch: &CommitBatchV1) -> Result<bool, StoreError> {
    for reference in &batch.prepared_artifacts {
        let prepared: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM prepared_artifacts
                 WHERE token_id = ?1 AND artifact_id = ?2 AND content_hash = ?3
                   AND byte_size = ?4 AND staging_generation = ?5
                   AND finalized_event_id = ?6",
                params![
                    reference.token_id.as_str(),
                    reference.artifact_id.as_str(),
                    reference.content_hash,
                    to_i64(reference.byte_size)?,
                    to_i64(reference.staging_generation)?,
                    reference.origin_event_id.as_str(),
                ],
                |row| row.get(0),
            )
            .optional()?;
        if prepared.is_none() {
            return Ok(false);
        }
        let found: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM artifacts a
                 JOIN artifact_references r ON r.artifact_id = a.artifact_id
                 WHERE a.artifact_id = ?1 AND a.content_hash = ?2 AND a.byte_size = ?3
                   AND r.origin_event_id = ?4",
                params![
                    reference.artifact_id.as_str(),
                    reference.content_hash,
                    to_i64(reference.byte_size)?,
                    reference.origin_event_id.as_str(),
                ],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn count_present_ids(
    connection: &Connection,
    table: &str,
    column: &str,
    ids: &[StableId],
) -> Result<usize, StoreError> {
    let sql = format!("SELECT 1 FROM {table} WHERE {column} = ?1");
    let mut count = 0;
    for id in ids {
        if connection
            .query_row(&sql, [id.as_str()], |row| row.get::<_, i64>(0))
            .optional()?
            .is_some()
        {
            count += 1;
        }
    }
    Ok(count)
}

fn load_outbox_cursors(connection: &Connection, ids: &[StableId]) -> Result<Vec<u64>, StoreError> {
    let mut cursors = Vec::with_capacity(ids.len());
    for id in ids {
        let cursor: Option<i64> = connection
            .query_row(
                "SELECT delivery_cursor FROM delivery_outbox WHERE outbox_id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(cursor) = cursor else {
            return Ok(Vec::new());
        };
        cursors.push(from_i64(cursor)?);
    }
    Ok(cursors)
}

fn stable(value: String) -> Result<StableId, StoreError> {
    StableId::parse(value).map_err(|_| StoreError::InvalidId)
}

fn absolute(path: &Path) -> Result<PathBuf, StoreError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidStoredData)
}

fn from_i64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStoredData)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn history_port_error(error: StoreError) -> HistoryPortErrorV1 {
    let (code, retryable, inspectable_read_only) = match &error {
        StoreError::HeadConflict { .. } | StoreError::AggregateVersionConflict { .. } => {
            ("history_conflict", true, false)
        }
        StoreError::Sql(_) | StoreError::Io(_) => ("storage_unavailable", true, false),
        StoreError::StoreQuarantined { .. }
        | StoreError::AmbiguousCommitQuarantined { .. }
        | StoreError::CorruptArtifact
        | StoreError::CorruptOutbox
        | StoreError::UnsupportedStorageVersion { .. } => ("inspectable_read_only", false, true),
        StoreError::WrongHistoryBackend | StoreError::BackendBindingMismatch => {
            ("backend_binding", false, false)
        }
        _ => ("invalid_history_commit", false, false),
    };
    HistoryPortErrorV1 {
        code: code.into(),
        message: error.to_string(),
        retryable,
        inspectable_read_only,
    }
}

/// Errors that prevent an acknowledged local semantic commit or maintenance operation.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("local store filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("local history database failed: {0}")]
    Sql(#[from] SqlError),
    #[error("local history JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local history connection is unavailable after a previous panic")]
    PoisonedConnection,
    #[error("stream head conflict: expected {expected}, found {actual}")]
    HeadConflict { expected: u64, actual: u64 },
    #[error("aggregate version conflict: expected {expected}, found {actual}")]
    AggregateVersionConflict { expected: u64, actual: u64 },
    #[error("deduplication key was reused for a different canonical request or stream")]
    DeduplicationKeyReused,
    #[error("commit must contain between 1 and 64 semantic events")]
    InvalidEventBatch,
    #[error("one of the bounded commit collections exceeds 64 entries")]
    CommitCollectionTooLarge,
    #[error("identifier must be 1-128 ASCII letters, digits, '.', '_' or '-'")]
    InvalidId,
    #[error("text field must contain 1-256 non-NUL bytes")]
    InvalidText,
    #[error("semantic history must not contain credential or secret material")]
    ForbiddenSecretMaterial,
    #[error("commit exceeds the one MiB bound")]
    CommitTooLarge,
    #[error("event ID is duplicated inside the commit batch")]
    DuplicateEventInBatch,
    #[error("attempt ID is duplicated inside the commit batch")]
    DuplicateAttemptInBatch,
    #[error("outbox ID is duplicated inside the commit batch")]
    DuplicateOutboxInBatch,
    #[error("prepared artifact token is duplicated inside the commit batch")]
    DuplicateArtifactTokenInBatch,
    #[error("local history contains an invalid stored numeric value")]
    InvalidStoredData,
    #[error("semantic schema version {0} is unsupported for canonical writes")]
    UnsupportedSchemaVersion(u16),
    #[error("history commit was routed to the wrong canonical backend")]
    WrongHistoryBackend,
    #[error("Chat/Run/backend binding differs from the durable stream binding")]
    BackendBindingMismatch,
    #[error("outbox entry does not exist")]
    UnknownOutbox,
    #[error("outbox cursor conflict: expected {expected}, found {actual}")]
    OutboxCursorConflict { expected: u64, actual: u64 },
    #[error("outbox payload hash does not match its committed bytes")]
    CorruptOutbox,
    #[error("prepared artifact token does not exist")]
    UnknownArtifactToken,
    #[error("prepared artifact token fields do not match the durable staging record")]
    ArtifactTokenMismatch,
    #[error("prepared artifact token is already finalized for another event")]
    ArtifactTokenAlreadyFinalized,
    #[error("artifact reference must target an event in the same semantic commit")]
    ArtifactOriginOutsideCommit,
    #[error("artifact identity is already bound to different immutable metadata")]
    ArtifactIdentityConflict,
    #[error("artifact does not exist")]
    UnknownArtifact,
    #[error("artifact object is missing or corrupt")]
    CorruptArtifact,
    #[error("artifact content hash must be a 64-character SHA-256 hex digest")]
    InvalidContentHash,
    #[error("artifact exceeds the 64 MiB per-object bound")]
    ArtifactTooLarge,
    #[error("unfinalized artifact staging exceeds the 512 MiB quota")]
    ArtifactQuotaExceeded,
    #[error("artifact range request exceeds the 8 MiB response bound")]
    ArtifactRangeTooLarge,
    #[error("artifact finalization is only allowed inside a semantic history commit")]
    ArtifactFinalizationRequiresCommit,
    #[error("backup destination must be outside the local store root")]
    BackupLocationInsideStore,
    #[error("restore source or target would overlap the active local store")]
    RestoreLocationOverlapsStore,
    #[error("local store path has no parent directory")]
    InvalidStorePath,
    #[error("storage schema {found} is newer than supported schema {supported}")]
    UnsupportedStorageVersion { found: u32, supported: u32 },
    #[error("canonical history stream does not exist")]
    UnknownHistoryStream,
    #[error("projection cursor belongs to an older rebuild generation")]
    StaleProjectionCursor,
    #[error("search query must contain 1-512 non-NUL bytes")]
    InvalidSearchQuery,
    #[error("replaceable projection database is corrupt: {0}")]
    CorruptProjection(String),
    #[error("backup manifest is invalid: {0}")]
    InvalidBackup(String),
    #[error("storage is already at the current migration version")]
    MigrationNotRequired,
    #[error("an interrupted migration journal requires explicit recovery")]
    MigrationRecoveryRequired,
    #[error("restore promotion failed: {0}")]
    RestorePromotionFailed(String),
    #[error("local store is quarantined and inspectable read-only: {reason}")]
    StoreQuarantined { reason: String },
    #[error("uncertain commit left conflicting durable facts; store was quarantined: {reason}")]
    AmbiguousCommitQuarantined { reason: String },
    #[error("injected commit crash point: {0}")]
    InjectedCommitFault(&'static str),
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitFaultPoint {
    AfterEvents,
    BeforeCommit,
    AfterCommitBeforeAck,
}

#[cfg(test)]
thread_local! {
    static COMMIT_FAULT: std::cell::Cell<Option<CommitFaultPoint>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn inject_fault(point: CommitFaultPoint) -> Result<(), StoreError> {
    COMMIT_FAULT.with(|fault| {
        if fault.get() == Some(point) {
            fault.set(None);
            Err(StoreError::InjectedCommitFault(match point {
                CommitFaultPoint::AfterEvents => "after_events",
                CommitFaultPoint::BeforeCommit => "before_commit",
                CommitFaultPoint::AfterCommitBeforeAck => "after_commit_before_ack",
            }))
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aworkit-ledger-{nonce}"))
    }

    fn store() -> (LocalHistoryStore, PathBuf) {
        let root = root();
        fs::create_dir_all(&root).expect("root");
        (
            LocalHistoryStore::open(root.join("history.sqlite")).expect("store"),
            root,
        )
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
                request_hash: "caller_cannot_choose_identity".into(),
            }),
            outbox: vec![OutboxEntry {
                outbox_id: format!("outbox_{expected_head}"),
                destination: "desktop".into(),
                payload: serde_json::json!({"schemaVersion": 1}),
            }],
        }
    }

    #[test]
    fn commits_every_local_history_fact_atomically() {
        let (store, root) = store();
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
            ["event_0"]
        );
        let pending = store.pending_outbox_v1(0, 8).expect("outbox");
        assert_eq!(pending.len(), 1);
        assert_ne!(pending[0].payload_hash, "");
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn server_computes_dedup_identity_and_rejects_changed_semantics() {
        let (store, root) = store();
        let first_batch = batch(0);
        let first = store.commit(&first_batch).expect("first");
        let mut caller_hash_changed = first_batch.clone();
        caller_hash_changed
            .deduplication
            .as_mut()
            .expect("dedup")
            .request_hash = "spoofed".into();
        let retry = store
            .commit(&caller_hash_changed)
            .expect("same canonical retry");
        assert_eq!(
            first,
            match retry {
                CommitOutcome::Existing(receipt) => CommitOutcome::Committed(receipt),
                CommitOutcome::Committed(_) => panic!("expected existing"),
            }
        );
        let mut changed = caller_hash_changed;
        changed.events[0].payload = serde_json::json!({"schemaVersion": 1, "text": "different"});
        assert!(matches!(
            store.commit(&changed),
            Err(StoreError::DeduplicationKeyReused)
        ));
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn stale_head_leaves_no_partial_transition() {
        let (store, root) = store();
        store.commit(&batch(0)).expect("first");
        let mut stale = batch(0);
        stale.events[0].event_id = "event_stale".into();
        stale.deduplication.as_mut().expect("dedup").key = "key_stale".into();
        assert!(matches!(
            store.commit(&stale),
            Err(StoreError::HeadConflict {
                expected: 0,
                actual: 1
            })
        ));
        assert_eq!(store.event_ids("chat_01", "main").expect("events").len(), 1);
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn secret_keys_are_blocked_without_rejecting_benign_token_counts() {
        let (store, root) = store();
        let mut accepted = batch(0);
        accepted.events[0].payload = serde_json::json!({"schemaVersion": 1, "tokenCount": 7});
        store.commit(&accepted).expect("benign token count");
        let mut rejected = batch(1);
        rejected.events[0].payload = serde_json::json!({"schemaVersion": 1, "accessToken": "bad"});
        assert!(matches!(
            store.commit(&rejected),
            Err(StoreError::ForbiddenSecretMaterial)
        ));
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn protocol_commit_enforces_backend_run_and_aggregate_binding() {
        let (store, root) = store();
        let legacy = batch(0);
        let mut protocol = legacy_batch(&legacy).expect("protocol");
        let first = store.commit_v1(&protocol).expect("first");
        assert!(matches!(first, CommitOutcomeV1::Committed(_)));

        protocol.deduplication = Some(DedupV1 {
            key_type: "command".into(),
            key: stable("other_key".into()).expect("id"),
        });
        protocol.events[0].event_id = stable("other_event".into()).expect("id");
        protocol.expected_head = 1;
        protocol.expected_aggregate_version = 0;
        assert!(matches!(
            store.commit_v1(&protocol),
            Err(StoreError::AggregateVersionConflict { .. })
        ));
        protocol.expected_aggregate_version = 1;
        protocol.run_id = stable("different_run".into()).expect("id");
        assert!(matches!(
            store.commit_v1(&protocol),
            Err(StoreError::BackendBindingMismatch)
        ));
        protocol.run_id = stable("chat_01".into()).expect("id");
        protocol.backend = HistoryBackendV1::PortableProject {
            repository_id: stable("repo_01".into()).expect("id"),
        };
        assert!(matches!(
            store.commit_v1(&protocol),
            Err(StoreError::WrongHistoryBackend)
        ));
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn outbox_cursor_pages_and_acknowledgements_are_fenced() {
        let (store, root) = store();
        store.commit(&batch(0)).expect("first");
        store.commit(&batch(1)).expect("second");
        let first = store.pending_outbox_v1(0, 1).expect("page one");
        let second = store
            .pending_outbox_v1(first[0].delivery_cursor, 1)
            .expect("page two");
        assert_eq!(second.len(), 1);
        assert!(matches!(
            store.mark_outbox_delivered_v1(&first[0].outbox_id, first[0].delivery_cursor + 1),
            Err(StoreError::OutboxCursorConflict { .. })
        ));
        store
            .mark_outbox_delivered_v1(&first[0].outbox_id, first[0].delivery_cursor)
            .expect("ack");
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn injected_crash_points_roll_back_or_recover_by_deduplicated_retry() {
        for point in [
            CommitFaultPoint::AfterEvents,
            CommitFaultPoint::BeforeCommit,
        ] {
            let (store, root) = store();
            COMMIT_FAULT.with(|fault| fault.set(Some(point)));
            assert!(matches!(
                store.commit(&batch(0)),
                Err(StoreError::InjectedCommitFault(_))
            ));
            assert!(
                store
                    .event_ids("chat_01", "main")
                    .expect("rolled-back events")
                    .is_empty()
            );
            assert!(
                store
                    .pending_outbox(8)
                    .expect("rolled-back outbox")
                    .is_empty()
            );
            store.commit(&batch(0)).expect("retry after rollback");
            drop(store);
            fs::remove_dir_all(root).expect("cleanup");
        }

        let (store, root) = store();
        COMMIT_FAULT.with(|fault| fault.set(Some(CommitFaultPoint::AfterCommitBeforeAck)));
        assert!(matches!(
            store.commit(&batch(0)),
            Err(StoreError::InjectedCommitFault("after_commit_before_ack"))
        ));
        assert_eq!(
            store.event_ids("chat_01", "main").expect("durable event"),
            ["event_0"]
        );
        assert!(matches!(
            store.commit(&batch(0)).expect("ambiguous retry"),
            CommitOutcome::Existing(_)
        ));
        assert_eq!(
            store.event_ids("chat_01", "main").expect("one event").len(),
            1
        );
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn ambiguous_commit_verification_detects_partial_rows_for_quarantine() {
        let (store, root) = store();
        let protocol = legacy_batch(&batch(0)).expect("protocol batch");
        let receipt = match store.commit_v1(&protocol).expect("commit") {
            CommitOutcomeV1::Committed(receipt) => receipt,
            CommitOutcomeV1::Existing(_) => panic!("new commit expected"),
        };
        let request_hash = canonical_request_hash(&protocol).expect("request hash");
        let cursor = store.pending_outbox_v1(0, 1).expect("outbox")[0].delivery_cursor;
        assert!(matches!(
            store
                .verify_durable_commit(&protocol, &request_hash, &receipt, &[cursor])
                .expect("verify"),
            Verification::Exact
        ));

        let connection = Connection::open(root.join("history.sqlite")).expect("tamper fixture");
        connection
            .execute("DELETE FROM delivery_outbox", [])
            .expect("simulate partial evidence");
        assert!(matches!(
            store
                .verify_durable_commit(&protocol, &request_hash, &receipt, &[cursor])
                .expect("partial verify"),
            Verification::Partial(_)
        ));
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
