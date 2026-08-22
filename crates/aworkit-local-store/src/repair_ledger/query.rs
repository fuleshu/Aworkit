//! Bounded group and occurrence queries plus investigation entry.

use rusqlite::params;

use super::{
    ErrorGroup, ErrorGroupStatus, ErrorOccurrence, RepairEvidenceLedger, RepairLedgerError,
    common::{decode_verified, load_group},
    validation::{validate_id, validate_page},
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    pub fn group(&self, fingerprint: &str) -> Result<Option<ErrorGroup>, RepairLedgerError> {
        validate_id(fingerprint)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_group(&connection, fingerprint)
    }

    pub fn list_groups(
        &self,
        after_fingerprint: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ErrorGroup>, RepairLedgerError> {
        validate_page(limit)?;
        if let Some(cursor) = after_fingerprint {
            validate_id(cursor)?;
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT record_json, record_hash FROM error_groups
             WHERE fingerprint > COALESCE(?1, '') ORDER BY fingerprint LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after_fingerprint, i64::from(limit)], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(json, hash)| decode_verified(&json, &hash))
            .collect()
    }

    pub fn occurrences(
        &self,
        fingerprint: &str,
        after_occurrence_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ErrorOccurrence>, RepairLedgerError> {
        validate_id(fingerprint)?;
        validate_page(limit)?;
        if let Some(cursor) = after_occurrence_id {
            validate_id(cursor)?;
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT record_json, record_hash FROM error_occurrences
             WHERE fingerprint=?1 AND occurrence_id > COALESCE(?2, '')
             ORDER BY occurrence_id LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![fingerprint, after_occurrence_id, i64::from(limit)],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(json, hash)| decode_verified(&json, &hash))
            .collect()
    }

    pub fn begin_investigation(
        &self,
        operation_id: &str,
        fingerprint: &str,
        expected_ledger_version: u64,
        occurred_at_epoch_ms: u64,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        self.transition_group(
            operation_id,
            fingerprint,
            expected_ledger_version,
            &[
                ErrorGroupStatus::Grouped,
                ErrorGroupStatus::RegressionReopened,
                ErrorGroupStatus::Rejected,
            ],
            ErrorGroupStatus::Investigating,
            "investigation_started",
            occurred_at_epoch_ms,
            redaction,
        )
    }
}
