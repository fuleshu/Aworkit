//! Restart verification, rollback, and terminal evidence.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{
    ErrorGroup, ErrorGroupStatus, LedgerAppendRequest, RepairEvidenceLedger, RepairLedgerError,
    RollbackRecord, VerificationOutcome, VerificationRecord, VerificationStart,
    candidate::{all_candidate_evidence, ensure_active_candidate, load_candidate},
    common::{
        SqlValue, canonical_hash, decode_verified, ensure_version, evidence_available,
        immutable_exists, insert_immutable, load_group, persist_group, prior_operation,
        store_operation,
    },
    transition::append_transition,
    validation::{validate_evidence, validate_hash, validate_id, validate_redacted, validate_text},
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    pub fn begin_verification(
        &self,
        request: &LedgerAppendRequest<VerificationStart>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_verification_start(&request.record)?;
        validate_id(&request.operation_id)?;
        validate_redacted(
            &(
                &request.operation_id,
                request.expected_ledger_version,
                &request.record,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(request.expected_ledger_version, &request.record))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, &request.operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        let candidate = load_candidate(
            &transaction,
            &request.record.candidate_id,
            request.record.candidate_version,
        )?
        .ok_or(RepairLedgerError::UnknownCandidate)?;
        if request.record.started_build_hash != candidate.candidate_hash {
            return Err(RepairLedgerError::Integrity);
        }
        let mut group = load_group(&transaction, &candidate.fingerprint)?
            .ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, request.expected_ledger_version)?;
        ensure_active_candidate(&group, &candidate)?;
        if group.status != ErrorGroupStatus::ActivatedRestarting {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: ErrorGroupStatus::Verifying,
            });
        }
        if immutable_exists(
            &transaction,
            "verification_starts",
            "verification_id",
            &request.record.verification_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
        }
        insert_immutable(
            &transaction,
            "verification_starts",
            &[
                (
                    "verification_id",
                    SqlValue::Text(&request.record.verification_id),
                ),
                ("candidate_id", SqlValue::Text(&request.record.candidate_id)),
                (
                    "candidate_version",
                    SqlValue::Integer(request.record.candidate_version),
                ),
            ],
            &request.record,
        )?;
        let from = group.status;
        group.status = ErrorGroupStatus::Verifying;
        group.ledger_version = group
            .ledger_version
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
        persist_group(&transaction, &group)?;
        append_transition(
            &transaction,
            &group.fingerprint,
            Some(from),
            group.status,
            "verification_started",
            request.record.started_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.record.started_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }

    /// A passed result closes the group as verified. A failed result is still
    /// appended immutably but remains `Verifying` until rollback evidence is
    /// durably appended; storage never claims rollback happened implicitly.
    #[allow(clippy::too_many_lines)]
    pub fn complete_verification(
        &self,
        request: &LedgerAppendRequest<VerificationRecord>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_verification(&request.record)?;
        validate_id(&request.operation_id)?;
        validate_redacted(
            &(
                &request.operation_id,
                request.expected_ledger_version,
                &request.record,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(request.expected_ledger_version, &request.record))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, &request.operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        let start = load_verification_start(&transaction, &request.record.verification_id)?
            .ok_or(RepairLedgerError::InvalidRecord)?;
        if start.candidate_id != request.record.candidate_id
            || start.candidate_version != request.record.candidate_version
            || start.started_build_hash != request.record.started_build_hash
        {
            return Err(RepairLedgerError::Integrity);
        }
        let candidate = load_candidate(
            &transaction,
            &request.record.candidate_id,
            request.record.candidate_version,
        )?
        .ok_or(RepairLedgerError::UnknownCandidate)?;
        let mut group = load_group(&transaction, &candidate.fingerprint)?
            .ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, request.expected_ledger_version)?;
        ensure_active_candidate(&group, &candidate)?;
        if group.status != ErrorGroupStatus::Verifying {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: ErrorGroupStatus::Verified,
            });
        }
        if matches!(request.record.outcome, VerificationOutcome::Passed)
            && (!request.record.identity_matched
                || request.record.started_build_hash != candidate.candidate_hash)
        {
            return Err(RepairLedgerError::Integrity);
        }
        let mut unavailable = Vec::new();
        if !evidence_available(&transaction, &request.record.evidence)? {
            unavailable.push(format!(
                "verification_evidence_unavailable:{}",
                request.record.evidence.artifact_id
            ));
        }
        if matches!(request.record.outcome, VerificationOutcome::Passed) {
            for evidence in all_candidate_evidence(&candidate) {
                if !evidence_available(&transaction, evidence)? {
                    unavailable.push(format!(
                        "candidate_evidence_unavailable:{}",
                        evidence.artifact_id
                    ));
                }
            }
        }
        unavailable.sort();
        unavailable.dedup();
        if !unavailable.is_empty() {
            return Err(RepairLedgerError::Ineligible(unavailable));
        }
        if immutable_exists(
            &transaction,
            "verifications",
            "verification_id",
            &request.record.verification_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
        }
        insert_immutable(
            &transaction,
            "verifications",
            &[
                (
                    "verification_id",
                    SqlValue::Text(&request.record.verification_id),
                ),
                ("candidate_id", SqlValue::Text(&request.record.candidate_id)),
                (
                    "candidate_version",
                    SqlValue::Integer(request.record.candidate_version),
                ),
            ],
            &request.record,
        )?;
        let from = group.status;
        if matches!(request.record.outcome, VerificationOutcome::Passed) {
            group.status = ErrorGroupStatus::Verified;
        }
        group.ledger_version = group
            .ledger_version
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
        persist_group(&transaction, &group)?;
        append_transition(
            &transaction,
            &group.fingerprint,
            Some(from),
            group.status,
            if matches!(request.record.outcome, VerificationOutcome::Passed) {
                "verification_passed"
            } else {
                "verification_failed_rollback_required"
            },
            request.record.completed_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.record.completed_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }

    #[allow(clippy::too_many_lines)]
    pub fn record_rollback(
        &self,
        request: &LedgerAppendRequest<RollbackRecord>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_rollback(&request.record)?;
        validate_id(&request.operation_id)?;
        validate_redacted(
            &(
                &request.operation_id,
                request.expected_ledger_version,
                &request.record,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(request.expected_ledger_version, &request.record))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, &request.operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        let candidate = load_candidate(
            &transaction,
            &request.record.candidate_id,
            request.record.candidate_version,
        )?
        .ok_or(RepairLedgerError::UnknownCandidate)?;
        if request.record.restored_build_hash
            != candidate.rollback_point.previous_working_build.content_hash
        {
            return Err(RepairLedgerError::Integrity);
        }
        let mut unavailable = Vec::new();
        if !evidence_available(&transaction, &request.record.evidence)? {
            unavailable.push(format!(
                "rollback_evidence_unavailable:{}",
                request.record.evidence.artifact_id
            ));
        }
        if !evidence_available(
            &transaction,
            &candidate.rollback_point.previous_working_build,
        )? {
            unavailable.push(format!(
                "rollback_build_unavailable:{}",
                candidate.rollback_point.previous_working_build.artifact_id
            ));
        }
        unavailable.sort();
        unavailable.dedup();
        if !unavailable.is_empty() {
            return Err(RepairLedgerError::Ineligible(unavailable));
        }
        let mut group = load_group(&transaction, &candidate.fingerprint)?
            .ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, request.expected_ledger_version)?;
        ensure_active_candidate(&group, &candidate)?;
        if !matches!(
            group.status,
            ErrorGroupStatus::ActivatedRestarting | ErrorGroupStatus::Verifying
        ) {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: ErrorGroupStatus::RolledBack,
            });
        }
        if immutable_exists(
            &transaction,
            "rollbacks",
            "rollback_id",
            &request.record.rollback_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
        }
        insert_immutable(
            &transaction,
            "rollbacks",
            &[
                ("rollback_id", SqlValue::Text(&request.record.rollback_id)),
                ("candidate_id", SqlValue::Text(&request.record.candidate_id)),
                (
                    "candidate_version",
                    SqlValue::Integer(request.record.candidate_version),
                ),
            ],
            &request.record,
        )?;
        let from = group.status;
        group.status = ErrorGroupStatus::RolledBack;
        group.ledger_version = group
            .ledger_version
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
        persist_group(&transaction, &group)?;
        append_transition(
            &transaction,
            &group.fingerprint,
            Some(from),
            group.status,
            if request.record.manual_recovery_required {
                "rollback_manual_recovery_required"
            } else {
                "rollback_completed"
            },
            request.record.completed_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.record.completed_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }

    pub fn verification(
        &self,
        verification_id: &str,
    ) -> Result<Option<VerificationRecord>, RepairLedgerError> {
        validate_id(verification_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_verified_record(
            &connection,
            "verifications",
            "verification_id",
            verification_id,
        )
    }

    pub fn rollback(&self, rollback_id: &str) -> Result<Option<RollbackRecord>, RepairLedgerError> {
        validate_id(rollback_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_verified_record(&connection, "rollbacks", "rollback_id", rollback_id)
    }
}

fn load_verification_start(
    connection: &Connection,
    verification_id: &str,
) -> Result<Option<VerificationStart>, RepairLedgerError> {
    load_verified_record(
        connection,
        "verification_starts",
        "verification_id",
        verification_id,
    )
}

fn load_verified_record<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    table: &str,
    id_column: &str,
    id: &str,
) -> Result<Option<T>, RepairLedgerError> {
    let sql = format!("SELECT record_json, record_hash FROM {table} WHERE {id_column}=?1");
    let stored = connection
        .query_row(&sql, [id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?;
    stored
        .map(|(json, hash)| decode_verified(&json, &hash))
        .transpose()
}

fn validate_verification_start(start: &VerificationStart) -> Result<(), RepairLedgerError> {
    validate_id(&start.verification_id)?;
    validate_id(&start.candidate_id)?;
    validate_hash(&start.started_build_hash)?;
    if start.candidate_version == 0 {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}

fn validate_verification(record: &VerificationRecord) -> Result<(), RepairLedgerError> {
    validate_id(&record.verification_id)?;
    validate_id(&record.candidate_id)?;
    validate_hash(&record.started_build_hash)?;
    validate_evidence(&record.evidence)?;
    if record.candidate_version == 0 {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}

fn validate_rollback(record: &RollbackRecord) -> Result<(), RepairLedgerError> {
    validate_id(&record.rollback_id)?;
    validate_id(&record.candidate_id)?;
    validate_hash(&record.restored_build_hash)?;
    validate_text(&record.reason)?;
    validate_evidence(&record.evidence)?;
    if record.candidate_version == 0 {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}
