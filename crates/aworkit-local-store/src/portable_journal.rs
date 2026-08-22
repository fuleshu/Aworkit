//! Strictly noncanonical portable-Run operational journal.
//!
//! The table contains publication fences and verified receipts only. It has no
//! semantic-event or delivery columns, so it cannot accidentally become a
//! second canonical history backend.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use aworkit_protocol::{
    HistoryPortErrorV1, PortableRuntimeBeginV1, PortableRuntimeFactsV1, PortableRuntimeFinalizeV1,
    PortableRuntimeJournalPort, StableId,
};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Durably visible phase of a two-store portable commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortableJournalPhase {
    PendingPortableCommit,
    HeadLinked,
    Quarantined,
}

impl PortableJournalPhase {
    fn parse(value: &str) -> Result<Self, PortableJournalError> {
        match value {
            "pending" => Ok(Self::PendingPortableCommit),
            "linked" => Ok(Self::HeadLinked),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(PortableJournalError::Corrupt),
        }
    }
}

/// Compatibility recovery facts used by the first portable gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableJournalRecord {
    pub chat_id: String,
    pub branch_id: String,
    pub commit_id: String,
    pub machine_instance_id: String,
    pub binding_generation: u64,
    pub expected_head_generation: u64,
    pub head_segment_hash: Option<String>,
    pub phase: PortableJournalPhase,
}

/// Machine-local runtime journal; it deliberately cannot append canonical events.
#[derive(Clone)]
pub struct PortableRuntimeJournal {
    connection: Arc<Mutex<Connection>>,
}

impl PortableRuntimeJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PortableJournalError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS portable_runtime_journal_v2 (
                 operation_id TEXT PRIMARY KEY,
                 commit_id TEXT NOT NULL UNIQUE,
                 chat_id TEXT NOT NULL,
                 branch_id TEXT NOT NULL,
                 machine_instance_id TEXT NOT NULL,
                 binding_generation INTEGER NOT NULL CHECK(binding_generation >= 0),
                 expected_head_generation INTEGER NOT NULL CHECK(expected_head_generation >= 0),
                 expected_head_hash TEXT,
                 candidate_head_hash TEXT NOT NULL,
                 checkpoint_hash TEXT NOT NULL,
                 phase TEXT NOT NULL CHECK(phase IN ('pending', 'linked', 'quarantined')),
                 verified_receipt_json TEXT,
                 quarantine_reason TEXT,
                 CHECK((phase = 'pending' AND verified_receipt_json IS NULL AND quarantine_reason IS NULL)
                    OR (phase = 'linked' AND verified_receipt_json IS NOT NULL AND quarantine_reason IS NULL)
                    OR (phase = 'quarantined' AND quarantine_reason IS NOT NULL))
             ) STRICT;",
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Persists the legacy fence before portable object publication starts.
    pub fn record_pending(
        &self,
        record: &PortableJournalRecord,
    ) -> Result<(), PortableJournalError> {
        if record.phase != PortableJournalPhase::PendingPortableCommit {
            return Err(PortableJournalError::InvalidPhase);
        }
        let operation_id =
            StableId::parse(record.commit_id.clone()).map_err(|_| PortableJournalError::Corrupt)?;
        self.begin_internal(
            &PortableRuntimeBeginV1 {
                operation_id,
                machine_instance_id: StableId::parse(record.machine_instance_id.clone())
                    .map_err(|_| PortableJournalError::Corrupt)?,
                binding_generation: record.binding_generation,
                expected_generation: record.expected_head_generation,
                chat_id: StableId::parse(record.chat_id.clone())
                    .map_err(|_| PortableJournalError::Corrupt)?,
                branch_id: StableId::parse(record.branch_id.clone())
                    .map_err(|_| PortableJournalError::Corrupt)?,
                commit_id: StableId::parse(record.commit_id.clone())
                    .map_err(|_| PortableJournalError::Corrupt)?,
                expected_head_hash: None,
                candidate_head_hash: "legacy.pending".to_owned(),
                checkpoint_hash: "legacy.none".to_owned(),
            },
            record.expected_head_generation,
        )
    }

    /// Finalizes the legacy record after a verified portable receipt is reread.
    pub fn link_head(
        &self,
        commit_id: &str,
        head_segment_hash: &str,
    ) -> Result<(), PortableJournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        let rows = connection.execute(
            "UPDATE portable_runtime_journal_v2
             SET phase = 'linked', candidate_head_hash = ?2,
                 verified_receipt_json = json_object('legacyHeadHash', ?2)
             WHERE commit_id = ?1 AND phase = 'pending'",
            params![commit_id, head_segment_hash],
        )?;
        if rows == 1 {
            Ok(())
        } else {
            Err(PortableJournalError::MissingOrFinalized)
        }
    }

    /// Reads compatibility recovery facts only; callers must compare every fence.
    pub fn get(
        &self,
        commit_id: &str,
    ) -> Result<Option<PortableJournalRecord>, PortableJournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        connection
            .query_row(
                "SELECT chat_id, branch_id, commit_id, machine_instance_id,
                        binding_generation, expected_head_generation,
                        CASE WHEN phase = 'linked' THEN candidate_head_hash ELSE NULL END,
                        phase
                 FROM portable_runtime_journal_v2 WHERE commit_id = ?1",
                [commit_id],
                |row| {
                    let phase: String = row.get(7)?;
                    Ok(PortableJournalRecord {
                        chat_id: row.get(0)?,
                        branch_id: row.get(1)?,
                        commit_id: row.get(2)?,
                        machine_instance_id: row.get(3)?,
                        binding_generation: checked_u64(row.get(4)?, 4)?,
                        expected_head_generation: checked_u64(row.get(5)?, 5)?,
                        head_segment_hash: row.get(6)?,
                        phase: PortableJournalPhase::parse(&phase)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    })
                },
            )
            .optional()
            .map_err(PortableJournalError::from)
    }

    fn begin_internal(
        &self,
        request: &PortableRuntimeBeginV1,
        expected_head_generation: u64,
    ) -> Result<(), PortableJournalError> {
        let encoded = serde_json::to_string(request)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        let transaction = connection.transaction()?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT json_object(
                    'operationId', operation_id,
                    'machineInstanceId', machine_instance_id,
                    'bindingGeneration', binding_generation,
                    'expectedGeneration', expected_head_generation,
                    'chatId', chat_id,
                    'branchId', branch_id,
                    'commitId', commit_id,
                    'expectedHeadHash', expected_head_hash,
                    'candidateHeadHash', candidate_head_hash,
                    'checkpointHash', checkpoint_hash)
                 FROM portable_runtime_journal_v2 WHERE operation_id = ?1",
                [request.operation_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing_value: serde_json::Value = serde_json::from_str(&existing)?;
            let request_value: serde_json::Value = serde_json::from_str(&encoded)?;
            if existing_value == request_value {
                transaction.commit()?;
                return Ok(());
            }
            return Err(PortableJournalError::IdentityConflict);
        }
        transaction.execute(
            "INSERT INTO portable_runtime_journal_v2
             (operation_id, commit_id, chat_id, branch_id, machine_instance_id,
              binding_generation, expected_head_generation, expected_head_hash,
              candidate_head_hash, checkpoint_hash, phase)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending')",
            params![
                request.operation_id.as_str(),
                request.commit_id.as_str(),
                request.chat_id.as_str(),
                request.branch_id.as_str(),
                request.machine_instance_id.as_str(),
                checked_i64(request.binding_generation)?,
                checked_i64(expected_head_generation)?,
                request.expected_head_hash.as_deref(),
                request.candidate_head_hash.as_str(),
                request.checkpoint_hash.as_str(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn finalize_internal(
        &self,
        request: &PortableRuntimeFinalizeV1,
    ) -> Result<(), PortableJournalError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        let transaction = connection.transaction()?;
        let begin = read_begin(&transaction, &request.operation_id)?
            .ok_or(PortableJournalError::MissingOrFinalized)?;
        let receipt = &request.verified_receipt;
        if receipt.operation_id != request.operation_id
            || receipt.commit_id != begin.commit_id
            || receipt.branch_id != begin.branch_id
            || receipt.previous_head_hash != begin.expected_head_hash
            || receipt.published_head_hash != begin.candidate_head_hash
            || receipt.checkpoint_hash != begin.checkpoint_hash
            || receipt.generation != begin.expected_generation.saturating_add(1)
        {
            return Err(PortableJournalError::FenceMismatch);
        }
        let receipt_json = serde_json::to_string(receipt)?;
        let existing: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT phase, verified_receipt_json FROM portable_runtime_journal_v2
                 WHERE operation_id = ?1",
                [request.operation_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((phase, Some(existing_receipt))) if phase == "linked" => {
                if serde_json::from_str::<serde_json::Value>(&existing_receipt)?
                    == serde_json::from_str::<serde_json::Value>(&receipt_json)?
                {
                    transaction.commit()?;
                    return Ok(());
                }
                return Err(PortableJournalError::IdentityConflict);
            }
            Some((phase, _)) if phase == "pending" => {}
            _ => return Err(PortableJournalError::MissingOrFinalized),
        }
        let rows = transaction.execute(
            "UPDATE portable_runtime_journal_v2
             SET phase = 'linked', verified_receipt_json = ?2
             WHERE operation_id = ?1 AND phase = 'pending'",
            params![request.operation_id.as_str(), receipt_json],
        )?;
        if rows != 1 {
            return Err(PortableJournalError::MissingOrFinalized);
        }
        transaction.commit()?;
        Ok(())
    }

    fn facts_internal(
        &self,
        operation_id: &StableId,
    ) -> Result<Option<PortableRuntimeFactsV1>, PortableJournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        let Some(begin) = read_begin(&connection, operation_id)? else {
            return Ok(None);
        };
        let (phase, receipt_json, quarantine_reason): (String, Option<String>, Option<String>) =
            connection.query_row(
                "SELECT phase, verified_receipt_json, quarantine_reason
                 FROM portable_runtime_journal_v2 WHERE operation_id = ?1",
                [operation_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        match phase.as_str() {
            "pending" => Ok(Some(PortableRuntimeFactsV1::Pending { begin })),
            "linked" => Ok(Some(PortableRuntimeFactsV1::HeadLinked {
                begin,
                receipt: serde_json::from_str(
                    receipt_json
                        .as_deref()
                        .ok_or(PortableJournalError::Corrupt)?,
                )?,
            })),
            "quarantined" => Ok(Some(PortableRuntimeFactsV1::Quarantined {
                begin,
                reason: quarantine_reason.ok_or(PortableJournalError::Corrupt)?,
            })),
            _ => Err(PortableJournalError::Corrupt),
        }
    }
}

impl PortableRuntimeJournalPort for PortableRuntimeJournal {
    fn begin(&self, request: &PortableRuntimeBeginV1) -> Result<(), HistoryPortErrorV1> {
        if request.operation_id != request.commit_id
            || request
                .expected_head_hash
                .as_deref()
                .is_some_and(|value| !valid_hash(value))
            || !valid_hash(&request.candidate_head_hash)
            || !valid_hash(&request.checkpoint_hash)
        {
            return Err(port_error(PortableJournalError::FenceMismatch));
        }
        self.begin_internal(request, request.expected_generation)
            .map_err(port_error)
    }

    fn finalize(&self, request: &PortableRuntimeFinalizeV1) -> Result<(), HistoryPortErrorV1> {
        self.finalize_internal(request).map_err(port_error)
    }

    fn facts(
        &self,
        operation_id: &StableId,
    ) -> Result<Option<PortableRuntimeFactsV1>, HistoryPortErrorV1> {
        self.facts_internal(operation_id).map_err(port_error)
    }

    fn quarantine(&self, operation_id: &StableId, reason: &str) -> Result<(), HistoryPortErrorV1> {
        if reason.is_empty() || reason.len() > 1024 {
            return Err(port_error(PortableJournalError::Corrupt));
        }
        let rows = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| port_error(PortableJournalError::Poisoned))?;
            connection
                .execute(
                    "UPDATE portable_runtime_journal_v2
                 SET phase = 'quarantined', quarantine_reason = ?2,
                     verified_receipt_json = NULL
                 WHERE operation_id = ?1 AND phase IN ('pending', 'linked')",
                    params![operation_id.as_str(), reason],
                )
                .map_err(|error| port_error(error.into()))?
        };
        if rows == 1 {
            Ok(())
        } else {
            let existing = self.facts_internal(operation_id).map_err(port_error)?;
            match existing {
                Some(PortableRuntimeFactsV1::Quarantined {
                    reason: existing, ..
                }) if existing == reason => Ok(()),
                _ => Err(port_error(PortableJournalError::MissingOrFinalized)),
            }
        }
    }
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn read_begin(
    connection: &Connection,
    operation_id: &StableId,
) -> Result<Option<PortableRuntimeBeginV1>, PortableJournalError> {
    connection
        .query_row(
            "SELECT operation_id, machine_instance_id, binding_generation,
                    expected_head_generation, chat_id, branch_id, commit_id,
                    expected_head_hash, candidate_head_hash, checkpoint_hash
             FROM portable_runtime_journal_v2 WHERE operation_id = ?1",
            [operation_id.as_str()],
            |row| {
                Ok(PortableRuntimeBeginV1 {
                    operation_id: parse_id(row.get(0)?)?,
                    machine_instance_id: parse_id(row.get(1)?)?,
                    binding_generation: checked_u64(row.get(2)?, 2)?,
                    expected_generation: checked_u64(row.get(3)?, 3)?,
                    chat_id: parse_id(row.get(4)?)?,
                    branch_id: parse_id(row.get(5)?)?,
                    commit_id: parse_id(row.get(6)?)?,
                    expected_head_hash: row.get(7)?,
                    candidate_head_hash: row.get(8)?,
                    checkpoint_hash: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(PortableJournalError::from)
}

fn parse_id(value: String) -> Result<StableId, rusqlite::Error> {
    StableId::parse(value).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn checked_i64(value: u64) -> Result<i64, PortableJournalError> {
    i64::try_from(value).map_err(|_| PortableJournalError::Corrupt)
}

fn checked_u64(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

fn port_error(error: PortableJournalError) -> HistoryPortErrorV1 {
    HistoryPortErrorV1 {
        code: match &error {
            PortableJournalError::IdentityConflict => "portable_journal_identity_conflict",
            PortableJournalError::FenceMismatch => "portable_journal_fence_mismatch",
            PortableJournalError::MissingOrFinalized => "portable_journal_missing_or_finalized",
            PortableJournalError::Poisoned => "portable_journal_unavailable",
            PortableJournalError::InvalidPhase | PortableJournalError::Corrupt => {
                "portable_journal_corrupt"
            }
            PortableJournalError::Sql(_) | PortableJournalError::Json(_) => "portable_journal_io",
        }
        .to_owned(),
        message: error.to_string(),
        retryable: matches!(
            &error,
            PortableJournalError::Poisoned | PortableJournalError::Sql(_)
        ),
        inspectable_read_only: true,
    }
}

#[derive(Debug, Error)]
pub enum PortableJournalError {
    #[error("portable journal record must begin pending")]
    InvalidPhase,
    #[error("portable journal record is missing or already finalized")]
    MissingOrFinalized,
    #[error("portable journal contains corrupt data")]
    Corrupt,
    #[error("portable journal operation identity conflicts with durable facts")]
    IdentityConflict,
    #[error("portable journal receipt does not match every publication fence")]
    FenceMismatch,
    #[error("portable journal lock is poisoned")]
    Poisoned,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
