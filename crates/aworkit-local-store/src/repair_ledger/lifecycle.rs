//! Shared optimistic transition primitive.

use rusqlite::TransactionBehavior;

use super::{
    ErrorGroup, ErrorGroupStatus, RepairEvidenceLedger, RepairLedgerError,
    common::{
        canonical_hash, ensure_version, load_group, persist_group, prior_operation, store_operation,
    },
    transition::append_transition,
    validation::{validate_id, validate_redacted},
};
use crate::RedactionSet;

impl RepairEvidenceLedger {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn transition_group(
        &self,
        operation_id: &str,
        fingerprint: &str,
        expected_ledger_version: u64,
        allowed_from: &[ErrorGroupStatus],
        to: ErrorGroupStatus,
        kind: &str,
        occurred_at_epoch_ms: u64,
        redaction: &RedactionSet,
    ) -> Result<ErrorGroup, RepairLedgerError> {
        validate_id(operation_id)?;
        validate_id(fingerprint)?;
        validate_redacted(
            &(
                operation_id,
                fingerprint,
                expected_ledger_version,
                allowed_from,
                to,
                kind,
                occurred_at_epoch_ms,
            ),
            redaction,
        )?;
        self.require_writable()?;
        let request_hash = canonical_hash(&(
            fingerprint,
            expected_ledger_version,
            allowed_from,
            to,
            kind,
            occurred_at_epoch_ms,
        ))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = prior_operation(&transaction, operation_id, &request_hash)? {
            transaction.commit()?;
            return Ok(prior);
        }
        let mut group =
            load_group(&transaction, fingerprint)?.ok_or(RepairLedgerError::UnknownGroup)?;
        ensure_version(&group, expected_ledger_version)?;
        let from = group.status;
        if !allowed_from.contains(&from) {
            return Err(RepairLedgerError::InvalidTransition { from, to });
        }
        group.ledger_version = group
            .ledger_version
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
        group.status = to;
        persist_group(&transaction, &group)?;
        append_transition(
            &transaction,
            fingerprint,
            Some(from),
            to,
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
