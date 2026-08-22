//! Immutable diagnoses, workarounds, and evidence-availability tombstones.

use rusqlite::TransactionBehavior;
use serde::{Serialize, de::DeserializeOwned};

use super::{
    DiagnosisRecord, ErrorGroup, ErrorGroupStatus, EvidenceTombstone, LedgerAppendRequest,
    RepairEvidenceLedger, RepairLedgerError, WorkaroundRecord,
    common::{
        SqlValue, canonical_hash, ensure_version, immutable_exists, insert_immutable, load_group,
        persist_group, prior_operation, store_operation,
    },
    transition::append_transition,
    validation::{
        validate_diagnosis, validate_id, validate_redacted, validate_tombstone, validate_workaround,
    },
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    pub fn append_diagnosis(
        &self,
        request: &LedgerAppendRequest<DiagnosisRecord>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_diagnosis(&request.record)?;
        self.append_note(
            &request.operation_id,
            request.expected_ledger_version,
            &request.record.fingerprint,
            "diagnoses",
            "diagnosis_id",
            &request.record.diagnosis_id,
            request.record.recorded_at_epoch_ms,
            "diagnosis_appended",
            &request.record,
            redaction,
        )
    }

    pub fn append_workaround(
        &self,
        request: &LedgerAppendRequest<WorkaroundRecord>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_workaround(&request.record)?;
        self.append_note(
            &request.operation_id,
            request.expected_ledger_version,
            &request.record.fingerprint,
            "workarounds",
            "workaround_id",
            &request.record.workaround_id,
            request.record.recorded_at_epoch_ms,
            "workaround_appended",
            &request.record,
            redaction,
        )
    }

    /// Appends an unavailable/expired/corrupt supersession without rewriting
    /// any immutable evidence reference.
    pub fn append_evidence_tombstone(
        &self,
        operation_id: &str,
        tombstone: &EvidenceTombstone,
        redaction: &RedactionSet,
    ) -> Result<EvidenceTombstone, RepairLedgerError> {
        validate_id(operation_id)?;
        validate_tombstone(tombstone)?;
        validate_redacted(&(operation_id, tombstone), redaction)?;
        self.require_writable()?;
        let request_hash = canonical_hash(tombstone)?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        if immutable_exists(
            &transaction,
            "evidence_tombstones",
            "tombstone_id",
            &tombstone.tombstone_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
        }
        insert_immutable(
            &transaction,
            "evidence_tombstones",
            &[
                ("tombstone_id", SqlValue::Text(&tombstone.tombstone_id)),
                ("artifact_id", SqlValue::Text(&tombstone.artifact_id)),
                ("content_hash", SqlValue::Text(&tombstone.content_hash)),
                (
                    "recorded_at_epoch_ms",
                    SqlValue::Integer(tombstone.recorded_at_epoch_ms),
                ),
            ],
            tombstone,
        )?;
        store_operation(
            &transaction,
            operation_id,
            &request_hash,
            tombstone.recorded_at_epoch_ms,
            tombstone,
        )?;
        transaction.commit()?;
        Ok(tombstone.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn append_note<T: Serialize + DeserializeOwned>(
        &self,
        operation_id: &str,
        expected_version: u64,
        fingerprint: &str,
        table: &str,
        id_column: &str,
        record_id: &str,
        occurred_at_epoch_ms: u64,
        kind: &str,
        record: &T,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_id(operation_id)?;
        validate_redacted(&(operation_id, expected_version, record), redaction)?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(expected_version, record))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        if immutable_exists(&transaction, table, id_column, record_id)? {
            return Err(RepairLedgerError::IdentityConflict);
        }
        let mut group =
            load_group(&transaction, fingerprint)?.ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, expected_version)?;
        if !matches!(
            group.status,
            ErrorGroupStatus::Grouped
                | ErrorGroupStatus::Investigating
                | ErrorGroupStatus::RegressionReopened
        ) {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: group.status,
            });
        }
        group.ledger_version = group
            .ledger_version
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
        persist_group(&transaction, &group)?;
        insert_immutable(
            &transaction,
            table,
            &[
                (id_column, SqlValue::Text(record_id)),
                ("fingerprint", SqlValue::Text(fingerprint)),
                (
                    "recorded_at_epoch_ms",
                    SqlValue::Integer(occurred_at_epoch_ms),
                ),
            ],
            record,
        )?;
        append_transition(
            &transaction,
            fingerprint,
            Some(group.status),
            group.status,
            kind,
            occurred_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            operation_id,
            &request_hash,
            occurred_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }
}
