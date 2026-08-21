//! Strictly noncanonical portable-Run operational journal.
//!
//! This SQLite journal has no semantic events. It only fences a matching local
//! runtime around an immutable portable branch-head publication. Losing or
//! mismatching it therefore quarantines continuation instead of replaying work.

use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use thiserror::Error;

/// Durably visible phase of a two-store portable commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortableJournalPhase {
    PendingPortableCommit,
    HeadLinked,
}
impl PortableJournalPhase {
    fn parse(value: &str) -> Result<Self, PortableJournalError> {
        match value {
            "pending" => Ok(Self::PendingPortableCommit),
            "linked" => Ok(Self::HeadLinked),
            _ => Err(PortableJournalError::Corrupt),
        }
    }
}

/// Exact local binding facts that must all agree before same-installation recovery.
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
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS portable_runtime_journal (commit_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL, machine_instance_id TEXT NOT NULL, binding_generation INTEGER NOT NULL, expected_head_generation INTEGER NOT NULL, head_segment_hash TEXT, phase TEXT NOT NULL) STRICT;")?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }
    /// Persists the fence before portable object publication starts.
    pub fn record_pending(
        &self,
        record: &PortableJournalRecord,
    ) -> Result<(), PortableJournalError> {
        if record.phase != PortableJournalPhase::PendingPortableCommit {
            return Err(PortableJournalError::InvalidPhase);
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        connection.execute("INSERT INTO portable_runtime_journal (commit_id, chat_id, branch_id, machine_instance_id, binding_generation, expected_head_generation, head_segment_hash, phase) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 'pending')", params![record.commit_id, record.chat_id, record.branch_id, record.machine_instance_id, i64::try_from(record.binding_generation).map_err(|_| PortableJournalError::Corrupt)?, i64::try_from(record.expected_head_generation).map_err(|_| PortableJournalError::Corrupt)?])?;
        Ok(())
    }
    /// Finalizes only after the portable coordinator's verified receipt is reread.
    pub fn link_head(
        &self,
        commit_id: &str,
        head_segment_hash: &str,
    ) -> Result<(), PortableJournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        let rows = connection.execute("UPDATE portable_runtime_journal SET phase = 'linked', head_segment_hash = ?2 WHERE commit_id = ?1 AND phase = 'pending'", params![commit_id, head_segment_hash])?;
        if rows == 1 {
            Ok(())
        } else {
            Err(PortableJournalError::MissingOrFinalized)
        }
    }
    /// Reads recovery facts only; callers must compare every runtime fence.
    pub fn get(
        &self,
        commit_id: &str,
    ) -> Result<Option<PortableJournalRecord>, PortableJournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PortableJournalError::Poisoned)?;
        connection.query_row("SELECT chat_id, branch_id, commit_id, machine_instance_id, binding_generation, expected_head_generation, head_segment_hash, phase FROM portable_runtime_journal WHERE commit_id = ?1", [commit_id], |row| { let phase: String = row.get(7)?; Ok(PortableJournalRecord { chat_id: row.get(0)?, branch_id: row.get(1)?, commit_id: row.get(2)?, machine_instance_id: row.get(3)?, binding_generation: u64::try_from(row.get::<_, i64>(4)?).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 0))?, expected_head_generation: u64::try_from(row.get::<_, i64>(5)?).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, 0))?, head_segment_hash: row.get(6)?, phase: PortableJournalPhase::parse(&phase).map_err(|_| rusqlite::Error::InvalidQuery)?, }) }).optional().map_err(PortableJournalError::from)
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
    #[error("portable journal lock is poisoned")]
    Poisoned,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}
