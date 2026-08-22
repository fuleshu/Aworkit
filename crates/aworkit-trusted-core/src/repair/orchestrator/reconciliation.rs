//! Focused verification, protected receipt import, and same-Chat resume.

use super::super::validation::{
    validate_authenticated_result, validate_focused_evidence_against_plan, validate_result_fresh,
};
use super::super::*;
use super::{
    error::RepairError,
    service::{RepairOrchestratorV1, ensure_version, port_error},
};

impl RepairOrchestratorV1 {
    /// Commits plan-bound evidence before sending it to the helper for receipt
    /// sealing. This call does not resume the Management Chat.
    pub fn complete_focused_verification_evidence(
        &self,
        command: CompleteFocusedVerificationEvidenceV1,
    ) -> Result<RepairAggregateV1, RepairError> {
        let events = vec![RepairEventV1::FocusedVerificationSubmitted {
            activation_id: command.activation_id.clone(),
            process_generation: command.current_process_generation,
            evidence: command.evidence.clone(),
        }];
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            if existing.events != events {
                return Err(RepairError::OperationConflict);
            }
            let aggregate = self.load_aggregate(&command.group_id)?;
            if aggregate.focused_verification.as_ref() != Some(&command.evidence) {
                return Err(RepairError::OperationConflict);
            }
            if aggregate.bootstrap_result.is_some() {
                return Ok(aggregate);
            }
            self.verify_focused_evidence_artifacts(&command.operation_id, &command.evidence)?;
            self.bootstrap
                .submit_focused_verification(&command.activation_id, command.evidence)
                .map_err(|source| port_error("bootstrap focused verification", source))?;
            return Ok(aggregate);
        }
        let aggregate = self.load_aggregate(&command.group_id)?;
        ensure_version(&aggregate, command.expected_ledger_version)?;
        let baton = aggregate
            .activation_baton
            .as_ref()
            .ok_or(RepairError::InvalidContract("activation baton is missing"))?;
        if baton.activation_id != command.activation_id
            || baton.candidate_process_generation != command.current_process_generation
            || aggregate.bootstrap_admission.is_none()
            || aggregate.bootstrap_result.is_some()
        {
            return Err(RepairError::InvalidContract(
                "focused verification arrived from an unexpected generation or terminal activation",
            ));
        }
        validate_focused_evidence_against_plan(&command.evidence, &baton.verification_plan)
            .map_err(RepairError::InvalidContract)?;
        self.verify_focused_evidence_artifacts(&command.operation_id, &command.evidence)?;
        let appended = self.append_and_reload(&aggregate, command.operation_id, events)?;
        self.verify_focused_evidence_artifacts(&command.activation_id, &command.evidence)?;
        if appended.aggregate.focused_verification.as_ref() != Some(&command.evidence)
            || appended.aggregate.bootstrap_result.is_some()
        {
            return Err(RepairError::OperationConflict);
        }
        self.bootstrap
            .submit_focused_verification(&command.activation_id, command.evidence)
            .map_err(|source| port_error("bootstrap focused verification", source))?;
        Ok(appended.aggregate)
    }

    /// Imports an authenticated expected-generation receipt, commits it exactly
    /// once, and only then requests idempotent same-Chat resume.
    pub fn reconcile_bootstrap_result(
        &self,
        command: ReconcileBootstrapResultV1,
    ) -> Result<BootstrapReconciliationOutcomeV1, RepairError> {
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            let result = match existing.events.as_slice() {
                [
                    RepairEventV1::BootstrapResultReconciled {
                        reconciled_at_epoch_ms,
                        result,
                    },
                ] if result.receipt.activation_id == command.activation_id
                    && result.receipt.recipient_process_generation
                        == command.current_process_generation
                    && *reconciled_at_epoch_ms == command.now_epoch_ms =>
                {
                    result.clone()
                }
                _ => return Err(RepairError::OperationConflict),
            };
            let aggregate = self.load_aggregate(&command.group_id)?;
            if aggregate.bootstrap_result.as_ref() != Some(&result) {
                return Err(RepairError::OperationConflict);
            }
            let resume_dispatched = self.resume_committed_result(&result, &aggregate)?;
            return Ok(BootstrapReconciliationOutcomeV1 {
                duplicate: true,
                receipt: result.receipt,
                resume_dispatched,
            });
        }
        let aggregate = self.load_aggregate(&command.group_id)?;
        if let Some(existing) = &aggregate.bootstrap_result
            && existing.receipt.activation_id == command.activation_id
            && existing.receipt.recipient_process_generation == command.current_process_generation
        {
            let resume_dispatched = self.resume_committed_result(existing, &aggregate)?;
            return Ok(BootstrapReconciliationOutcomeV1 {
                duplicate: true,
                receipt: existing.receipt.clone(),
                resume_dispatched,
            });
        }
        ensure_version(&aggregate, command.expected_ledger_version)?;
        let baton = aggregate
            .activation_baton
            .as_ref()
            .ok_or(RepairError::InvalidContract("activation baton is missing"))?
            .clone();
        if baton.activation_id != command.activation_id
            || (command.current_process_generation != baton.candidate_process_generation
                && command.current_process_generation != baton.rollback_process_generation)
        {
            return Err(RepairError::InvalidContract(
                "receipt reconciliation generation is unexpected",
            ));
        }
        let result = self
            .bootstrap
            .read_result(BootstrapResultQueryV1 {
                operation_id: command.operation_id.clone(),
                activation_id: command.activation_id,
                recipient_process_generation: command.current_process_generation,
            })
            .map_err(|source| port_error("bootstrap result read", source))?
            .ok_or(RepairError::BootstrapResultMissing)?;
        validate_authenticated_result(&result, &baton, aggregate.bootstrap_admission.as_ref())
            .map_err(RepairError::InvalidContract)?;
        validate_result_fresh(&result, &baton, command.now_epoch_ms)
            .map_err(RepairError::InvalidContract)?;
        if !matches!(
            result.receipt.result,
            BootstrapResultKindV1::Unsupported { .. }
        ) && aggregate.quiescence.is_none()
        {
            return Err(RepairError::InvalidContract(
                "bootstrap result has no committed safe quiescence handoff",
            ));
        }
        if result.receipt.recipient_process_generation != command.current_process_generation {
            return Err(RepairError::InvalidContract(
                "receipt is targeted to another process generation",
            ));
        }
        self.verify_result_artifacts(&command.operation_id, &result.receipt.result)?;
        let appended = self.append_and_reload(
            &aggregate,
            command.operation_id,
            vec![RepairEventV1::BootstrapResultReconciled {
                reconciled_at_epoch_ms: command.now_epoch_ms,
                result: result.clone(),
            }],
        )?;
        let resume_dispatched = self.resume_committed_result(&result, &appended.aggregate)?;
        Ok(BootstrapReconciliationOutcomeV1 {
            duplicate: appended.duplicate,
            receipt: result.receipt,
            resume_dispatched,
        })
    }

    fn resume_committed_result(
        &self,
        result: &AuthenticatedBootstrapResultV1,
        aggregate: &RepairAggregateV1,
    ) -> Result<bool, RepairError> {
        let baton = aggregate
            .activation_baton
            .as_ref()
            .ok_or(RepairError::InvalidContract(
                "committed bootstrap result has no activation baton",
            ))?;
        let checkpoint =
            aggregate
                .management_checkpoint
                .clone()
                .ok_or(RepairError::InvalidContract(
                    "committed bootstrap result has no Management checkpoint",
                ))?;
        if aggregate.bootstrap_result.as_ref() != Some(result)
            || result.receipt.activation_id != baton.activation_id
            || result.receipt.baton_hash != baton.baton_hash
            || result.receipt.management_checkpoint_id != checkpoint.checkpoint_id
        {
            return Err(RepairError::OperationConflict);
        }
        self.management
            .resume_same_chat(ManagementResumeRequestV1 {
                receipt_id: result.receipt.receipt_id.clone(),
                activation_id: result.receipt.activation_id.clone(),
                checkpoint,
                recipient_process_generation: result.receipt.recipient_process_generation,
            })
            .map_err(|source| port_error("Management same-Chat resume", source))?;
        Ok(true)
    }
}
