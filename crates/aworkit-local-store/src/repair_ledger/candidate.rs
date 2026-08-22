//! Complete immutable repair candidates, disclosure, and rejection.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    CandidateDisclosure, ErrorGroup, ErrorGroupStatus, EvidenceAvailability, LedgerAppendRequest,
    PrepareCandidateRequest, RejectionRecord, RepairCandidate, RepairEvidenceLedger,
    RepairLedgerError,
    common::{
        SqlValue, canonical_hash, decode_verified, ensure_version, immutable_exists,
        insert_immutable, load_group, persist_group, prior_operation, store_operation, to_i64,
    },
    transition::append_transition,
    validation::{validate_evidence, validate_hash, validate_id, validate_redacted, validate_text},
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    #[allow(clippy::too_many_lines)]
    pub fn prepare_candidate(
        &self,
        request: &PrepareCandidateRequest,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_id(&request.operation_id)?;
        validate_candidate(&request.candidate)?;
        validate_redacted(
            &(
                &request.operation_id,
                request.expected_ledger_version,
                request.expected_candidate_version,
                &request.candidate,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(
            request.expected_ledger_version,
            request.expected_candidate_version,
            &request.candidate,
        ))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, &request.operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        let mut group = load_group(&transaction, &request.candidate.fingerprint)?
            .ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, request.expected_ledger_version)?;
        if group.status != ErrorGroupStatus::Investigating {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: ErrorGroupStatus::CandidatePrepared,
            });
        }
        let latest: Option<i64> = transaction
            .query_row(
                "SELECT MAX(candidate_version) FROM repair_candidates WHERE candidate_id=?1",
                [&request.candidate.candidate_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        match (latest, request.expected_candidate_version) {
            (None, None) if request.candidate.candidate_version == 1 => {}
            (Some(latest), Some(expected))
                if u64::try_from(latest).map_err(|_| RepairLedgerError::Corrupt)? == expected
                    && request.candidate.candidate_version == expected.saturating_add(1) => {}
            _ => return Err(RepairLedgerError::CandidateVersionConflict),
        }
        insert_immutable(
            &transaction,
            "repair_candidates",
            &[
                (
                    "candidate_id",
                    SqlValue::Text(&request.candidate.candidate_id),
                ),
                (
                    "candidate_version",
                    SqlValue::Integer(request.candidate.candidate_version),
                ),
                (
                    "fingerprint",
                    SqlValue::Text(&request.candidate.fingerprint),
                ),
                (
                    "candidate_hash",
                    SqlValue::Text(&request.candidate.candidate_hash),
                ),
                (
                    "prepared_at_epoch_ms",
                    SqlValue::Integer(request.candidate.prepared_at_epoch_ms),
                ),
            ],
            &request.candidate,
        )?;
        let from = group.status;
        group.status = ErrorGroupStatus::CandidatePrepared;
        group.ledger_version = group
            .ledger_version
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
        group.active_candidate_id = Some(request.candidate.candidate_id.clone());
        group.active_candidate_version = Some(request.candidate.candidate_version);
        persist_group(&transaction, &group)?;
        append_transition(
            &transaction,
            &group.fingerprint,
            Some(from),
            group.status,
            "candidate_prepared",
            request.candidate.prepared_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.candidate.prepared_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }

    pub fn disclose_candidate(
        &self,
        request: &LedgerAppendRequest<CandidateDisclosure>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_disclosure(&request.record)?;
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
        if immutable_exists(
            &transaction,
            "candidate_disclosures",
            "disclosure_id",
            &request.record.disclosure_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
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
        if group.status != ErrorGroupStatus::CandidatePrepared {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: ErrorGroupStatus::AwaitingActivation,
            });
        }
        insert_immutable(
            &transaction,
            "candidate_disclosures",
            &[
                (
                    "disclosure_id",
                    SqlValue::Text(&request.record.disclosure_id),
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
        group.status = ErrorGroupStatus::AwaitingActivation;
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
            "candidate_disclosed",
            request.record.disclosed_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.record.disclosed_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }

    pub fn reject_candidate(
        &self,
        request: &LedgerAppendRequest<RejectionRecord>,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_rejection(&request.record)?;
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
        let mut group = load_group(&transaction, &candidate.fingerprint)?
            .ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, request.expected_ledger_version)?;
        ensure_active_candidate(&group, &candidate)?;
        if group.status != ErrorGroupStatus::AwaitingActivation {
            return Err(RepairLedgerError::InvalidTransition {
                from: group.status,
                to: ErrorGroupStatus::Rejected,
            });
        }
        insert_immutable(
            &transaction,
            "candidate_rejections",
            &[
                ("rejection_id", SqlValue::Text(&request.record.rejection_id)),
                ("candidate_id", SqlValue::Text(&request.record.candidate_id)),
                (
                    "candidate_version",
                    SqlValue::Integer(request.record.candidate_version),
                ),
            ],
            &request.record,
        )?;
        let from = group.status;
        group.status = ErrorGroupStatus::Rejected;
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
            "candidate_rejected",
            request.record.rejected_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.record.rejected_at_epoch_ms,
            &group,
        )?;
        transaction.commit()?;
        Ok(group)
    }

    pub fn candidate(
        &self,
        candidate_id: &str,
        candidate_version: u64,
    ) -> Result<Option<RepairCandidate>, RepairLedgerError> {
        validate_id(candidate_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_candidate(&connection, candidate_id, candidate_version)
    }
}

pub(super) fn load_candidate(
    connection: &Connection,
    candidate_id: &str,
    candidate_version: u64,
) -> Result<Option<RepairCandidate>, RepairLedgerError> {
    let stored = connection
        .query_row(
            "SELECT record_json, record_hash FROM repair_candidates
             WHERE candidate_id=?1 AND candidate_version=?2",
            params![candidate_id, to_i64(candidate_version)?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    stored
        .map(|(json, hash)| decode_verified(&json, &hash))
        .transpose()
}

pub(super) fn load_disclosure(
    connection: &Connection,
    candidate_id: &str,
    candidate_version: u64,
) -> Result<Option<CandidateDisclosure>, RepairLedgerError> {
    let stored = connection
        .query_row(
            "SELECT record_json, record_hash FROM candidate_disclosures
             WHERE candidate_id=?1 AND candidate_version=?2",
            params![candidate_id, to_i64(candidate_version)?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    stored
        .map(|(json, hash)| decode_verified(&json, &hash))
        .transpose()
}

pub(super) fn ensure_active_candidate(
    group: &ErrorGroup,
    candidate: &RepairCandidate,
) -> Result<(), RepairLedgerError> {
    if group.active_candidate_id.as_deref() != Some(candidate.candidate_id.as_str())
        || group.active_candidate_version != Some(candidate.candidate_version)
    {
        return Err(RepairLedgerError::CandidateVersionConflict);
    }
    Ok(())
}

pub(super) fn all_candidate_evidence(
    candidate: &RepairCandidate,
) -> [&super::EvidenceReference; 9] {
    [
        &candidate.candidate_build,
        &candidate.evidence.diff,
        &candidate.evidence.tests,
        &candidate.evidence.benchmarks,
        &candidate.evidence.consequences,
        &candidate.evidence.removal_plan,
        &candidate.evidence.authority_broadening,
        &candidate.evidence.uncertainties,
        &candidate.rollback_point.previous_working_build,
    ]
}

fn validate_candidate(candidate: &RepairCandidate) -> Result<(), RepairLedgerError> {
    validate_id(&candidate.candidate_id)?;
    validate_id(&candidate.fingerprint)?;
    validate_hash(&candidate.candidate_hash)?;
    if candidate.candidate_version == 0
        || candidate.candidate_hash != candidate.candidate_build.content_hash
    {
        return Err(RepairLedgerError::InvalidRecord);
    }
    validate_id(&candidate.rollback_point.rollback_point_id)?;
    for evidence in all_candidate_evidence(candidate) {
        validate_evidence(evidence)?;
        if evidence.availability != EvidenceAvailability::Available {
            return Err(RepairLedgerError::InvalidRecord);
        }
    }
    Ok(())
}

fn validate_disclosure(disclosure: &CandidateDisclosure) -> Result<(), RepairLedgerError> {
    validate_id(&disclosure.disclosure_id)?;
    validate_id(&disclosure.candidate_id)?;
    validate_id(&disclosure.management_checkpoint_id)?;
    validate_hash(&disclosure.disclosure_hash)?;
    if disclosure.candidate_version == 0 {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}

fn validate_rejection(rejection: &RejectionRecord) -> Result<(), RepairLedgerError> {
    validate_id(&rejection.rejection_id)?;
    validate_id(&rejection.candidate_id)?;
    validate_text(&rejection.reason)?;
    if rejection.candidate_version == 0 {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}
