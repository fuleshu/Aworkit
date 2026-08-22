//! Activation eligibility and tamper-evident restart baton persistence.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{
    ActivateCandidateRequest, ActivationEligibility, ErrorGroupStatus, RepairEvidenceLedger,
    RepairLedgerError, RestartBaton,
    candidate::{all_candidate_evidence, ensure_active_candidate, load_candidate, load_disclosure},
    common::{
        SqlValue, canonical_hash, decode_verified, ensure_version, evidence_available,
        immutable_exists, insert_immutable, load_group, persist_group, prior_operation,
        store_operation,
    },
    transition::append_transition,
    validation::{validate_hash, validate_id, validate_redacted},
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    /// Re-verifies immutable hashes, complete evidence, disclosure, lifecycle,
    /// and any later evidence tombstones without changing state.
    pub fn activation_eligibility(
        &self,
        fingerprint: &str,
        candidate_id: &str,
        candidate_version: u64,
    ) -> Result<ActivationEligibility, RepairLedgerError> {
        validate_id(fingerprint)?;
        validate_id(candidate_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let group = load_group(&connection, fingerprint)?.ok_or(RepairLedgerError::UnknownGroup)?;
        let candidate = load_candidate(&connection, candidate_id, candidate_version)?
            .ok_or(RepairLedgerError::UnknownCandidate)?;
        eligibility(&connection, &group, &candidate)
    }

    /// Fsyncs the explicit activation decision and complete baton before the
    /// caller may quiesce the current application generation.
    #[allow(clippy::too_many_lines)]
    pub fn activate_and_restart(
        &self,
        request: &ActivateCandidateRequest,
        redaction: &RedactionSet,
    ) -> Result<RestartBaton, RepairLedgerError> {
        validate_id(&request.operation_id)?;
        validate_baton(&request.baton)?;
        validate_redacted(
            &(
                &request.operation_id,
                request.expected_ledger_version,
                request.expected_candidate_version,
                &request.baton,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(
            request.expected_ledger_version,
            request.expected_candidate_version,
            &request.baton,
        ))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, &request.operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        if immutable_exists(
            &transaction,
            "restart_batons",
            "baton_id",
            &request.baton.baton_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
        }
        let candidate = load_candidate(
            &transaction,
            &request.baton.candidate_id,
            request.baton.candidate_version,
        )?
        .ok_or(RepairLedgerError::UnknownCandidate)?;
        let mut group = load_group(&transaction, &request.baton.fingerprint)?
            .ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, request.expected_ledger_version)?;
        if candidate.candidate_version != request.expected_candidate_version {
            return Err(RepairLedgerError::CandidateVersionConflict);
        }
        ensure_active_candidate(&group, &candidate)?;
        let eligibility = eligibility(&transaction, &group, &candidate)?;
        if !eligibility.eligible {
            return Err(RepairLedgerError::Ineligible(eligibility.reasons));
        }
        let disclosure = load_disclosure(
            &transaction,
            &candidate.candidate_id,
            candidate.candidate_version,
        )?
        .ok_or_else(|| RepairLedgerError::Ineligible(vec!["missing_disclosure".to_owned()]))?;
        if request.baton.fingerprint != candidate.fingerprint
            || request.baton.candidate_hash != candidate.candidate_hash
            || request.baton.rollback_point_id != candidate.rollback_point.rollback_point_id
            || request.baton.previous_working_build_hash
                != candidate.rollback_point.previous_working_build.content_hash
            || request.baton.management_checkpoint_id != disclosure.management_checkpoint_id
        {
            return Err(RepairLedgerError::Integrity);
        }
        insert_immutable(
            &transaction,
            "restart_batons",
            &[
                ("baton_id", SqlValue::Text(&request.baton.baton_id)),
                ("fingerprint", SqlValue::Text(&request.baton.fingerprint)),
                ("candidate_id", SqlValue::Text(&request.baton.candidate_id)),
                (
                    "candidate_version",
                    SqlValue::Integer(request.baton.candidate_version),
                ),
            ],
            &request.baton,
        )?;
        let from = group.status;
        group.status = ErrorGroupStatus::ActivatedRestarting;
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
            "activation_decision_fsynced",
            request.baton.activated_at_epoch_ms,
        )?;
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.baton.activated_at_epoch_ms,
            &request.baton,
        )?;
        transaction.commit()?;
        // FULL synchronous already makes the commit durable. A full checkpoint
        // additionally produces a self-contained baton database before exit.
        connection.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        Ok(request.baton.clone())
    }

    pub fn restart_baton(&self, baton_id: &str) -> Result<Option<RestartBaton>, RepairLedgerError> {
        validate_id(baton_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_baton(&connection, baton_id)
    }
}

pub(super) fn load_baton(
    connection: &Connection,
    baton_id: &str,
) -> Result<Option<RestartBaton>, RepairLedgerError> {
    let stored = connection
        .query_row(
            "SELECT record_json, record_hash FROM restart_batons WHERE baton_id=?1",
            [baton_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    stored
        .map(|(json, hash)| decode_verified(&json, &hash))
        .transpose()
}

fn eligibility(
    connection: &Connection,
    group: &super::ErrorGroup,
    candidate: &super::RepairCandidate,
) -> Result<ActivationEligibility, RepairLedgerError> {
    let mut reasons = Vec::new();
    if group.status != ErrorGroupStatus::AwaitingActivation {
        reasons.push("group_not_awaiting_activation".to_owned());
    }
    if ensure_active_candidate(group, candidate).is_err() {
        reasons.push("candidate_not_active".to_owned());
    }
    if candidate.candidate_hash != candidate.candidate_build.content_hash {
        reasons.push("candidate_hash_mismatch".to_owned());
    }
    for evidence in all_candidate_evidence(candidate) {
        if !evidence_available(connection, evidence)? {
            reasons.push(format!("evidence_unavailable:{}", evidence.artifact_id));
        }
    }
    match load_disclosure(
        connection,
        &candidate.candidate_id,
        candidate.candidate_version,
    )? {
        None => reasons.push("missing_disclosure".to_owned()),
        Some(disclosure) => {
            if disclosure.management_checkpoint_id.is_empty() {
                reasons.push("missing_management_checkpoint".to_owned());
            }
        }
    }
    reasons.sort();
    reasons.dedup();
    Ok(ActivationEligibility {
        eligible: reasons.is_empty(),
        reasons,
    })
}

fn validate_baton(baton: &RestartBaton) -> Result<(), RepairLedgerError> {
    for id in [
        &baton.baton_id,
        &baton.fingerprint,
        &baton.candidate_id,
        &baton.rollback_point_id,
        &baton.management_checkpoint_id,
    ] {
        validate_id(id)?;
    }
    validate_hash(&baton.candidate_hash)?;
    validate_hash(&baton.previous_working_build_hash)?;
    validate_hash(&baton.activation_decision_hash)?;
    if baton.candidate_version == 0 {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}
