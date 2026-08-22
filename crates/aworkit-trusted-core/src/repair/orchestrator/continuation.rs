//! Durable activation continuation driven only by already committed outbox facts.

use super::super::validation::{
    validate_admission, validate_authenticated_result, validate_quiescence_facts,
    validate_result_fresh,
};
use super::super::*;
use super::{
    error::RepairError,
    service::{RepairOrchestratorV1, port_error},
};

impl RepairOrchestratorV1 {
    pub(super) fn continue_activation(
        &self,
        aggregate: RepairAggregateV1,
        baton: RepairActivationBatonV1,
        now_epoch_ms: u64,
    ) -> Result<ActivationHandoffOutcomeV1, RepairError> {
        if aggregate.activation_baton.as_ref() != Some(&baton) {
            return Err(RepairError::InvalidContract(
                "committed activation baton is no longer the active handoff",
            ));
        }
        if let Some(result) = aggregate.bootstrap_result.as_ref().filter(|result| {
            result.receipt.activation_id == baton.activation_id
                && matches!(
                    result.receipt.result,
                    BootstrapResultKindV1::Unsupported { .. }
                )
        }) {
            self.resume_unsupported(result, &baton)?;
            return Ok(ActivationHandoffOutcomeV1::Unsupported(
                result.receipt.clone(),
            ));
        }
        if aggregate.bootstrap_result.is_some() {
            return Err(RepairError::InvalidContract(
                "activation already has a terminal bootstrap result",
            ));
        }
        if let Some(admission) = aggregate.bootstrap_admission.clone() {
            return self.continue_after_admission(aggregate, baton, admission);
        }

        let candidate = aggregate
            .candidate_exact(&baton.candidate_id, baton.candidate_version)
            .ok_or(RepairError::CandidateMismatch)?;
        let report = aggregate
            .latest_capability_report
            .as_ref()
            .filter(|report| report.report_id == baton.capability_report_id)
            .ok_or(RepairError::InvalidContract(
                "activation capability report is missing during redrive",
            ))?;
        self.verify_activation_artifacts(&baton.activation_id, candidate, report)?;

        match self
            .bootstrap
            .admit_activation(baton.clone())
            .map_err(|source| port_error("bootstrap activation admission", source))?
        {
            BootstrapAdmissionV1::Unsupported(result) => {
                validate_authenticated_result(&result, &baton, None)
                    .map_err(RepairError::InvalidContract)?;
                validate_result_fresh(&result, &baton, now_epoch_ms)
                    .map_err(RepairError::InvalidContract)?;
                if !matches!(
                    result.receipt.result,
                    BootstrapResultKindV1::Unsupported { .. }
                ) {
                    return Err(RepairError::InvalidContract(
                        "non-Unsupported receipt used as admission rejection",
                    ));
                }
                let after_result = self.append_and_reload(
                    &aggregate,
                    result.receipt.receipt_id.clone(),
                    vec![RepairEventV1::BootstrapResultReconciled {
                        reconciled_at_epoch_ms: now_epoch_ms,
                        result: result.clone(),
                    }],
                )?;
                let committed = after_result.aggregate.bootstrap_result.as_ref().ok_or(
                    RepairError::InvalidContract("Unsupported receipt was not durably projected"),
                )?;
                if committed != &result {
                    return Err(RepairError::OperationConflict);
                }
                self.resume_unsupported(committed, &baton)?;
                Ok(ActivationHandoffOutcomeV1::Unsupported(result.receipt))
            }
            BootstrapAdmissionV1::Accepted(admission) => {
                validate_admission(&admission).map_err(RepairError::InvalidContract)?;
                if admission.activation_id != baton.activation_id
                    || admission.baton_hash != baton.baton_hash
                    || admission.candidate_process_generation != baton.candidate_process_generation
                    || admission.rollback_process_generation != baton.rollback_process_generation
                {
                    return Err(RepairError::InvalidContract(
                        "bootstrap admission does not match the committed baton",
                    ));
                }
                let after_admission = self.append_and_reload(
                    &aggregate,
                    admission.admission_id.clone(),
                    vec![RepairEventV1::BootstrapAdmissionAccepted {
                        admission: admission.clone(),
                    }],
                )?;
                self.continue_after_admission(after_admission.aggregate, baton, admission)
            }
        }
    }

    fn continue_after_admission(
        &self,
        aggregate: RepairAggregateV1,
        baton: RepairActivationBatonV1,
        admission: BootstrapAcceptedAdmissionV1,
    ) -> Result<ActivationHandoffOutcomeV1, RepairError> {
        if aggregate.activation_baton.as_ref() != Some(&baton)
            || aggregate.bootstrap_admission.as_ref() != Some(&admission)
            || aggregate.bootstrap_result.is_some()
        {
            return Err(RepairError::OperationConflict);
        }
        let quiescence = if let Some(existing) = aggregate.quiescence.clone() {
            existing
        } else {
            let facts = self
                .quiescence
                .quiesce_current_generation(CoreQuiescenceRequestV1 {
                    activation_id: baton.activation_id.clone(),
                    process_generation: baton.current_process_generation,
                    deadline_ms: baton.deadlines.cleanup_ms,
                })
                .map_err(|source| port_error("current-generation quiescence", source))?;
            validate_quiescence_facts(&facts).map_err(RepairError::InvalidContract)?;
            if facts.activation_id != baton.activation_id
                || facts.process_generation != baton.current_process_generation
            {
                return Err(RepairError::InvalidContract(
                    "quiescence facts do not match the admitted activation",
                ));
            }
            if facts.timed_out || facts.orphan_risk {
                return Err(RepairError::UnsafeQuiescence);
            }
            let appended = self.append_and_reload(
                &aggregate,
                facts.quiescence_id.clone(),
                vec![RepairEventV1::CoreQuiesced {
                    facts: facts.clone(),
                }],
            )?;
            if appended.aggregate.activation_baton.as_ref() != Some(&baton)
                || appended.aggregate.bootstrap_admission.as_ref() != Some(&admission)
                || appended.aggregate.quiescence.as_ref() != Some(&facts)
                || appended.aggregate.bootstrap_result.is_some()
            {
                return Err(RepairError::OperationConflict);
            }
            facts
        };
        if quiescence.timed_out || quiescence.orphan_risk {
            return Err(RepairError::UnsafeQuiescence);
        }
        self.bootstrap
            .record_core_quiescence(&admission.admission_id, quiescence.clone())
            .map_err(|source| port_error("bootstrap quiescence handoff", source))?;
        Ok(ActivationHandoffOutcomeV1::ReadyForCoreExit {
            baton,
            admission,
            quiescence,
        })
    }

    fn resume_unsupported(
        &self,
        result: &AuthenticatedBootstrapResultV1,
        baton: &RepairActivationBatonV1,
    ) -> Result<(), RepairError> {
        self.management
            .resume_same_chat(ManagementResumeRequestV1 {
                receipt_id: result.receipt.receipt_id.clone(),
                activation_id: result.receipt.activation_id.clone(),
                checkpoint: baton.management_checkpoint.clone(),
                recipient_process_generation: baton.current_process_generation,
            })
            .map_err(|source| port_error("Management same-Chat resume", source))
    }
}
