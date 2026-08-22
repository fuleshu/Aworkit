//! Stable failure grouping, immutable occurrences, and regression reopening.

use rusqlite::TransactionBehavior;
use sha2::{Digest, Sha256};

use super::{
    ErrorGroup, ErrorGroupStatus, OccurrenceReceipt, RecordOccurrenceRequest, RegressionRecord,
    RepairEvidenceLedger, RepairLedgerError,
    common::{
        SqlValue, canonical_hash, ensure_version, immutable_exists, insert_immutable, load_group,
        persist_group, prior_operation, store_operation,
    },
    transition::append_transition,
    validation::{validate_id, validate_occurrence, validate_redacted},
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    /// Appends one immutable occurrence and updates the compact fingerprint
    /// group under compare-and-swap. A post-terminal match creates a regression
    /// and reopens the group; it never starts an investigation automatically.
    #[allow(clippy::too_many_lines)]
    pub fn record_occurrence(
        &self,
        request: &RecordOccurrenceRequest,
        redaction: &RedactionSet,
    ) -> Result<OccurrenceReceipt, RepairLedgerError> {
        validate_id(&request.operation_id)?;
        validate_occurrence(&request.occurrence)?;
        validate_redacted(
            &(
                &request.operation_id,
                request.expected_ledger_version,
                &request.occurrence,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(request.expected_ledger_version, &request.occurrence))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = prior_operation(&transaction, &request.operation_id, &request_hash)?
        {
            transaction.commit()?;
            return Ok(receipt);
        }
        if immutable_exists(
            &transaction,
            "error_occurrences",
            "occurrence_id",
            &request.occurrence.occurrence_id,
        )? {
            return Err(RepairLedgerError::IdentityConflict);
        }

        let existing = load_group(&transaction, &request.occurrence.fingerprint)?;
        let (group, regression, transition_from) = match existing {
            None => {
                if request.expected_ledger_version.is_some() {
                    return Err(RepairLedgerError::VersionConflict {
                        expected: request.expected_ledger_version.unwrap_or_default(),
                        actual: 0,
                    });
                }
                (
                    ErrorGroup {
                        fingerprint: request.occurrence.fingerprint.clone(),
                        ledger_version: 1,
                        status: ErrorGroupStatus::Grouped,
                        occurrence_count: 1,
                        first_seen_epoch_ms: request.occurrence.observed_at_epoch_ms,
                        last_seen_epoch_ms: request.occurrence.observed_at_epoch_ms,
                        active_candidate_id: None,
                        active_candidate_version: None,
                    },
                    None,
                    None,
                )
            }
            Some(mut group) => {
                let expected =
                    request
                        .expected_ledger_version
                        .ok_or(RepairLedgerError::VersionConflict {
                            expected: 0,
                            actual: group.ledger_version,
                        })?;
                ensure_version(&group, expected)?;
                let prior = group.status;
                group.ledger_version = group
                    .ledger_version
                    .checked_add(1)
                    .ok_or(RepairLedgerError::NumericOverflow)?;
                group.occurrence_count = group
                    .occurrence_count
                    .checked_add(1)
                    .ok_or(RepairLedgerError::NumericOverflow)?;
                group.last_seen_epoch_ms = group
                    .last_seen_epoch_ms
                    .max(request.occurrence.observed_at_epoch_ms);
                let regression = matches!(
                    prior,
                    ErrorGroupStatus::Verified | ErrorGroupStatus::RolledBack
                )
                .then(|| RegressionRecord {
                    regression_id: derived_id("regression", &request.occurrence.occurrence_id),
                    fingerprint: group.fingerprint.clone(),
                    occurrence_id: request.occurrence.occurrence_id.clone(),
                    prior_status: prior,
                    prior_candidate_id: group.active_candidate_id.clone(),
                    recorded_at_epoch_ms: request.occurrence.observed_at_epoch_ms,
                });
                if regression.is_some() {
                    group.status = ErrorGroupStatus::RegressionReopened;
                }
                (group, regression, Some(prior))
            }
        };

        persist_group(&transaction, &group)?;
        insert_immutable(
            &transaction,
            "error_occurrences",
            &[
                (
                    "occurrence_id",
                    SqlValue::Text(&request.occurrence.occurrence_id),
                ),
                (
                    "fingerprint",
                    SqlValue::Text(&request.occurrence.fingerprint),
                ),
                (
                    "observed_at_epoch_ms",
                    SqlValue::Integer(request.occurrence.observed_at_epoch_ms),
                ),
            ],
            &request.occurrence,
        )?;
        if let Some(regression) = &regression {
            insert_immutable(
                &transaction,
                "regressions",
                &[
                    ("regression_id", SqlValue::Text(&regression.regression_id)),
                    ("fingerprint", SqlValue::Text(&regression.fingerprint)),
                    ("occurrence_id", SqlValue::Text(&regression.occurrence_id)),
                ],
                regression,
            )?;
        }
        append_transition(
            &transaction,
            &group.fingerprint,
            transition_from,
            group.status,
            if regression.is_some() {
                "regression"
            } else {
                "occurrence"
            },
            request.occurrence.observed_at_epoch_ms,
        )?;
        let receipt = OccurrenceReceipt { group, regression };
        store_operation(
            &transaction,
            &request.operation_id,
            &request_hash,
            request.occurrence.observed_at_epoch_ms,
            &receipt,
        )?;
        transaction.commit()?;
        Ok(receipt)
    }
}

fn derived_id(prefix: &str, source: &str) -> String {
    format!("{prefix}.{:x}", Sha256::digest(source.as_bytes()))
}
