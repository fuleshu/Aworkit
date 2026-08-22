//! Explicit activation decision, bootstrap admission, and quiescence handoff.

use aworkit_protocol::ProcessGeneration;

use super::super::validation::{
    repair_activation_baton_hash_v1, total_deadline_ms, validate_bootstrap_deadlines,
    validate_checkpoint, validate_data_compatibility_for_activation,
    validate_repair_activation_baton,
};
use super::super::*;
use super::{
    error::RepairError,
    service::{
        RepairOrchestratorV1, active_candidate_exact, ensure_version, exact_report, port_error,
    },
};

impl RepairOrchestratorV1 {
    /// Creates and commits the checkpoint, decision, and baton before any
    /// helper admission or process-tree cleanup is attempted.
    pub fn activate_and_restart(
        &self,
        command: ActivateAndRestartV1,
    ) -> Result<ActivationHandoffOutcomeV1, RepairError> {
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            let baton = match existing.events.as_slice() {
                [
                    RepairEventV1::ActivationPrepared {
                        decision,
                        checkpoint: _,
                        baton,
                    },
                ] if decision.activation_id == command.activation_id
                    && decision.explicit_user_decision_id == command.explicit_user_decision_id
                    && decision.candidate_id == command.candidate_id
                    && decision.expected_candidate_version
                        == command.expected_candidate_version
                    && decision.expected_candidate_hash == command.expected_candidate_hash
                    && decision.expected_capability_report_id
                        == command.expected_capability_report_id
                    && decision.expected_capability_digest
                        == command.expected_capability_digest
                    && decision.decided_at_epoch_ms == command.now_epoch_ms
                    && baton.baton_id == command.baton_id
                    && baton.group_id == command.group_id
                    && baton.current_process_generation == command.current_process_generation
                    && baton.deadlines == command.deadlines =>
                {
                    baton.clone()
                }
                _ => return Err(RepairError::OperationConflict),
            };
            let aggregate = self.load_aggregate(&command.group_id)?;
            return self.continue_activation(aggregate, baton, command.now_epoch_ms);
        }
        let aggregate = self.load_aggregate(&command.group_id)?;
        ensure_version(&aggregate, command.expected_ledger_version)?;
        let prior_activation_was_unsupported =
            aggregate.bootstrap_result.as_ref().is_some_and(|result| {
                matches!(
                    result.receipt.result,
                    BootstrapResultKindV1::Unsupported { .. }
                )
            });
        if (aggregate.activation_baton.is_some() && !prior_activation_was_unsupported)
            || (aggregate.enrollment_request.is_some() && aggregate.enrollment_prepared.is_none())
        {
            return Err(RepairError::OperationAlreadyActive);
        }
        let candidate = active_candidate_exact(
            &aggregate,
            &command.candidate_id,
            command.expected_candidate_version,
            &command.expected_candidate_hash,
        )?
        .clone();
        validate_data_compatibility_for_activation(&candidate.disclosure.data_compatibility)
            .map_err(RepairError::InvalidContract)?;
        let report = exact_report(
            &aggregate,
            &command.expected_capability_report_id,
            &command.expected_capability_digest,
            command.now_epoch_ms,
        )?
        .clone();
        if report.eligibility != ActivationEligibilityV1::SupportedManagedLocal {
            return Err(RepairError::ActivationUnavailable(report.eligibility));
        }
        let enrollment_digest = match &report.build_origin {
            BuildOriginV1::ManagedLocal {
                enrollment_digest, ..
            } => enrollment_digest.clone(),
            _ => {
                return Err(RepairError::InvalidContract(
                    "Supported report has no managed-local origin",
                ));
            }
        };
        let previous_working_build =
            report
                .previous_working_build
                .clone()
                .ok_or(RepairError::InvalidContract(
                    "Supported report has no previous working build",
                ))?;
        if previous_working_build != candidate.disclosure.rollback_point {
            return Err(RepairError::InvalidContract(
                "Supported report changed the disclosed rollback point",
            ));
        }
        self.verify_activation_artifacts(&command.operation_id, &candidate, &report)?;
        let investigation = aggregate
            .investigation
            .as_ref()
            .ok_or(RepairError::InvestigationMismatch)?;
        if command.current_process_generation.0 == 0 {
            return Err(RepairError::InvalidContract(
                "current process generation must be nonzero",
            ));
        }
        validate_bootstrap_deadlines(&command.deadlines).map_err(RepairError::InvalidContract)?;
        let total_deadline = total_deadline_ms(&command.deadlines).ok_or(
            RepairError::InvalidContract("bootstrap deadline total overflowed"),
        )?;
        let expires_at_epoch_ms = command.now_epoch_ms.checked_add(total_deadline).ok_or(
            RepairError::InvalidContract("bootstrap deadline timestamp overflowed"),
        )?;
        let candidate_generation = command
            .current_process_generation
            .0
            .checked_add(1)
            .map(ProcessGeneration)
            .ok_or(RepairError::InvalidContract(
                "candidate process generation overflowed",
            ))?;
        let rollback_generation = command
            .current_process_generation
            .0
            .checked_add(2)
            .map(ProcessGeneration)
            .ok_or(RepairError::InvalidContract(
                "rollback process generation overflowed",
            ))?;
        let checkpoint = self
            .management
            .create_checkpoint(ManagementCheckpointRequestV1 {
                operation_id: command.operation_id.clone(),
                activation_id: command.activation_id.clone(),
                group_id: aggregate.group_id.clone(),
                candidate_id: candidate.candidate_id.clone(),
                candidate_version: candidate.candidate_version,
                management_chat_id: investigation.management_chat_id.clone(),
                management_run_id: investigation.management_run_id.clone(),
            })
            .map_err(|source| port_error("Management checkpoint", source))?;
        validate_checkpoint(&checkpoint).map_err(RepairError::InvalidContract)?;
        if checkpoint.chat_id != investigation.management_chat_id
            || checkpoint.run_id != investigation.management_run_id
        {
            return Err(RepairError::InvalidContract(
                "checkpoint did not preserve the Management Chat and Run",
            ));
        }
        let decision = RepairActivationDecisionV1 {
            activation_id: command.activation_id.clone(),
            explicit_user_decision_id: command.explicit_user_decision_id,
            candidate_id: candidate.candidate_id.clone(),
            expected_candidate_version: candidate.candidate_version,
            expected_candidate_hash: candidate.candidate_hash.clone(),
            expected_capability_report_id: report.report_id.clone(),
            expected_capability_digest: report.capability_digest.clone(),
            decided_at_epoch_ms: command.now_epoch_ms,
        };
        let mut baton = RepairActivationBatonV1 {
            schema_version: REPAIR_SCHEMA_VERSION_V1,
            baton_id: command.baton_id,
            activation_id: command.activation_id,
            group_id: aggregate.group_id.clone(),
            candidate_id: candidate.candidate_id.clone(),
            candidate_version: candidate.candidate_version,
            candidate_hash: candidate.candidate_hash.clone(),
            candidate_bundle: candidate.build_bundle.clone(),
            disclosure_hash: candidate.disclosure.disclosure_hash.clone(),
            provenance_hash: candidate.provenance.provenance_hash.clone(),
            enrollment_digest,
            capability_report_id: report.report_id,
            capability_generation: report.capability_generation,
            capability_digest: report.capability_digest,
            previous_working_build,
            management_checkpoint: checkpoint.clone(),
            verification_plan: candidate.disclosure.verification_plan.clone(),
            current_process_generation: command.current_process_generation,
            candidate_process_generation: candidate_generation,
            rollback_process_generation: rollback_generation,
            deadlines: command.deadlines,
            expires_at_epoch_ms,
            baton_hash: String::new(),
        };
        baton.baton_hash = repair_activation_baton_hash_v1(&baton)
            .map_err(|_| RepairError::InvalidContract("activation baton could not be sealed"))?;
        validate_repair_activation_baton(&baton).map_err(RepairError::InvalidContract)?;
        let after_baton = self.append_and_reload(
            &aggregate,
            command.operation_id,
            vec![RepairEventV1::ActivationPrepared {
                decision,
                checkpoint,
                baton: baton.clone(),
            }],
        )?;
        self.continue_activation(after_baton.aggregate, baton, command.now_epoch_ms)
    }
}
