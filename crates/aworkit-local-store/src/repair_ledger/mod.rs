//! Durable recurring-error grouping and user-gated repair evidence.
//!
//! The ledger is a storage adapter, not a repair agent: fingerprints, diagnoses,
//! candidates, activation decisions, verification, and rollback outcomes all
//! arrive from the trusted core. `SQLite` enforces optimistic versions while
//! immutable records and a hash-chained transition log preserve evidence.

mod activation;
mod candidate;
mod common;
mod core_events;
mod integrity;
mod lifecycle;
mod model;
mod notes;
mod occurrence;
mod outcomes;
mod query;
mod schema;
mod transition;
mod validation;

#[cfg(test)]
mod tests;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use rusqlite::Connection;

use crate::maintenance::MaintenanceGate;

pub use model::{
    ActivateCandidateRequest, ActivationEligibility, CandidateDisclosure, CandidateEvidence,
    CoreEventAppendBatchReceipt, CoreEventAppendBatchRequest, CoreEventAppendReceipt,
    CoreEventAppendRequest, CoreEventInput, CoreEventVersions, DiagnosisRecord,
    DiagnosticEvidenceReference, ErrorGroup, ErrorGroupStatus, ErrorOccurrence,
    EvidenceAvailability, EvidenceReference, EvidenceTombstone, LedgerAppendRequest,
    OccurrenceReceipt, PrepareCandidateRequest, RecordOccurrenceRequest, RegressionRecord,
    RejectionRecord, RepairCandidate, RepairIntegrityReport, RepairLedgerError, RepairLedgerMode,
    RepairTransition, RestartBaton, RollbackPoint, RollbackRecord, StoredCoreEvent,
    VerificationOutcome, VerificationRecord, VerificationStart, WorkaroundRecord,
};
use schema::{SCHEMA_VERSION, configure, ensure};

/// Dedicated compact `SQLite` repository and restart-baton vault.
#[derive(Clone)]
pub struct RepairEvidenceLedger {
    path: Arc<PathBuf>,
    gate: MaintenanceGate,
    connection: Arc<Mutex<Connection>>,
    mode: RepairLedgerMode,
}

impl RepairEvidenceLedger {
    pub fn for_store_root(root: impl AsRef<Path>) -> Result<Self, RepairLedgerError> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root.as_ref())?;
        Self::open(root.join("repair-evidence.sqlite"))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, RepairLedgerError> {
        let path = absolute_file(path.as_ref())?;
        let root = path
            .parent()
            .ok_or(RepairLedgerError::InvalidRecord)?
            .to_path_buf();
        fs::create_dir_all(&root)?;
        let gate = MaintenanceGate::for_root(&root)?;
        let _lease = gate.shared()?;
        let connection = Connection::open(&path)?;
        configure(&connection)?;
        let found: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let mode = if found > SCHEMA_VERSION {
            connection.execute_batch("PRAGMA query_only=ON;")?;
            RepairLedgerMode::InspectableReadOnly {
                found_schema: u32::try_from(found).unwrap_or(u32::MAX),
            }
        } else {
            ensure(&connection)?;
            RepairLedgerMode::ReadWrite
        };
        Ok(Self {
            path: Arc::new(path),
            gate,
            connection: Arc::new(Mutex::new(connection)),
            mode,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.path.as_ref()
    }

    #[must_use]
    pub fn mode(&self) -> RepairLedgerMode {
        self.mode
    }

    pub(super) fn require_writable(&self) -> Result<(), RepairLedgerError> {
        match self.mode {
            RepairLedgerMode::ReadWrite => Ok(()),
            RepairLedgerMode::InspectableReadOnly { found_schema } => {
                Err(RepairLedgerError::ForwardSchema { found_schema })
            }
        }
    }

    pub(super) fn lock(&self) -> Result<MutexGuard<'_, Connection>, RepairLedgerError> {
        self.connection
            .lock()
            .map_err(|_| RepairLedgerError::Poisoned)
    }
}

fn absolute_file(path: &Path) -> Result<PathBuf, RepairLedgerError> {
    let parent = path.parent().ok_or(RepairLedgerError::InvalidRecord)?;
    fs::create_dir_all(parent)?;
    let parent = fs::canonicalize(parent)?;
    let name = path.file_name().ok_or(RepairLedgerError::InvalidRecord)?;
    Ok(parent.join(name))
}
