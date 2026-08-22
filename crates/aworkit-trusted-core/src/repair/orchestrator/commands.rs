//! Recurring-error, investigation, candidate, capability, and decision commands.

use crate::AuthorityManifestV1;

use super::super::validation::{
    freeze_investigation_authority, repair_group_id_for_fingerprint_v1,
    validate_authenticated_investigation_execution, validate_candidate,
    validate_capability_report_fresh, validate_enrollment_prepared, validate_investigation,
    validate_occurrence,
};
use super::super::*;
use super::{
    error::RepairError,
    service::{
        RepairOrchestratorV1, active_candidate_exact, derived_id, ensure_version, exact_report,
        port_error,
    },
};

impl RepairOrchestratorV1 {
    /// Records a failure occurrence only. A verified recurrence is atomically
    /// marked as a regression but never starts another investigation.
    pub fn record_recurring_failure(
        &self,
        command: RecordRecurringFailureV1,
    ) -> Result<RepairAggregateV1, RepairError> {
        let resolved_group = repair_group_id_for_fingerprint_v1(&command.occurrence.fingerprint)
            .map_err(|_| RepairError::InvalidContract("invalid recurring-error fingerprint"))?;
        if resolved_group != command.group_id {
            return Err(RepairError::InvalidContract(
                "group id is not the stable fingerprint resolution",
            ));
        }
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            let exact = matches!(
                existing.events.as_slice(),
                [RepairEventV1::FailureRecorded { occurrence }]
                    if occurrence == &command.occurrence
            ) || matches!(
                existing.events.as_slice(),
                [
                    RepairEventV1::FailureRecorded { occurrence },
                    RepairEventV1::RegressionRecorded { regression },
                ] if occurrence == &command.occurrence
                    && regression.occurrence_id == command.occurrence.occurrence_id
            );
            if !exact {
                return Err(RepairError::OperationConflict);
            }
            return self.load_aggregate(&command.group_id);
        }
        validate_occurrence(&command.occurrence).map_err(RepairError::InvalidContract)?;
        self.verify_occurrence_artifacts(&command.operation_id, &command.occurrence)?;
        let aggregate = self.load_aggregate(&command.group_id)?;
        let verified = aggregate.verified_build().map(|(candidate, result)| {
            (
                candidate.candidate_id.clone(),
                result.receipt.receipt_id.clone(),
            )
        });
        let mut events = vec![RepairEventV1::FailureRecorded {
            occurrence: command.occurrence.clone(),
        }];
        if let Some((candidate_id, receipt_id)) = verified {
            events.push(RepairEventV1::RegressionRecorded {
                regression: RepairRegressionV1 {
                    regression_id: derived_id("regression", &command.occurrence.occurrence_id)?,
                    occurrence_id: command.occurrence.occurrence_id,
                    repaired_candidate_id: candidate_id,
                    repaired_receipt_id: receipt_id,
                },
            });
        }
        ensure_version(&aggregate, command.expected_ledger_version)?;
        self.append_and_reload(&aggregate, command.operation_id, events)
            .map(|result| result.aggregate)
    }

    /// Freezes an authority subset, commits the explicit user decision, then
    /// dispatches the bounded investigation through existing Run machinery.
    pub fn start_bounded_investigation(
        &self,
        command: StartInvestigationV1,
        frozen_authority: &AuthorityManifestV1,
    ) -> Result<RepairInvestigationV1, RepairError> {
        let authority =
            freeze_investigation_authority(frozen_authority, &command.requested_capability_ids)
                .map_err(RepairError::InvalidContract)?;
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            let investigation = match existing.events.as_slice() {
                [RepairEventV1::InvestigationStarted { investigation }]
                    if investigation.investigation_id == command.investigation_id
                        && investigation.explicit_user_decision_id
                            == command.explicit_user_decision_id
                        && investigation.group_id == command.group_id
                        && investigation.management_chat_id == command.management_chat_id
                        && investigation.management_run_id == command.management_run_id
                        && investigation.budget == command.budget
                        && investigation.authority == authority =>
                {
                    investigation.clone()
                }
                _ => return Err(RepairError::OperationConflict),
            };
            let aggregate = self.load_aggregate(&command.group_id)?;
            if aggregate.investigation.as_ref() != Some(&investigation) {
                return Err(RepairError::OperationConflict);
            }
            if aggregate.phase != Some(RepairPhaseV1::Investigating) {
                return Ok(investigation);
            }
            self.investigations
                .dispatch(RepairInvestigationDispatchV1 {
                    operation_id: command.operation_id,
                    investigation: investigation.clone(),
                })
                .map_err(|source| port_error("repair investigation dispatch", source))?;
            return Ok(investigation);
        }
        let investigation = RepairInvestigationV1 {
            investigation_id: command.investigation_id.clone(),
            explicit_user_decision_id: command.explicit_user_decision_id.clone(),
            group_id: command.group_id.clone(),
            management_chat_id: command.management_chat_id.clone(),
            management_run_id: command.management_run_id.clone(),
            authority,
            budget: command.budget.clone(),
        };
        let events = vec![RepairEventV1::InvestigationStarted {
            investigation: investigation.clone(),
        }];
        validate_investigation(&investigation).map_err(RepairError::InvalidContract)?;
        let aggregate = self.load_aggregate(&command.group_id)?;
        ensure_version(&aggregate, command.expected_ledger_version)?;
        if aggregate.fingerprint.is_none() {
            return Err(RepairError::GroupMissing);
        }
        let appended = self.append_and_reload(&aggregate, command.operation_id.clone(), events)?;
        if appended.aggregate.investigation.as_ref() != Some(&investigation)
            || appended.aggregate.phase != Some(RepairPhaseV1::Investigating)
        {
            return Err(RepairError::OperationConflict);
        }
        self.investigations
            .dispatch(RepairInvestigationDispatchV1 {
                operation_id: command.operation_id,
                investigation: investigation.clone(),
            })
            .map_err(|source| port_error("repair investigation dispatch", source))?;
        Ok(investigation)
    }

    /// Registers evidence produced outside core after validating complete
    /// disclosure and its frozen-authority binding.
    pub fn register_candidate(
        &self,
        command: RegisterRepairCandidateV1,
    ) -> Result<RepairAggregateV1, RepairError> {
        if let Some(existing) =
            self.load_operation(&command.candidate.group_id, &command.operation_id)?
        {
            let exact = matches!(
                existing.events.as_slice(),
                [RepairEventV1::CandidateRegistered {
                    candidate,
                    execution_receipt,
                }] if candidate == &command.candidate
                    && execution_receipt.receipt.investigation_id
                        == command.investigation_id
                    && execution_receipt.receipt.receipt_id == command.execution_receipt_id
                    && execution_receipt.receipt.receipt_hash
                        == command.expected_execution_receipt_hash
            );
            if !exact {
                return Err(RepairError::OperationConflict);
            }
            return self.load_aggregate(&command.candidate.group_id);
        }
        let aggregate = self.load_aggregate(&command.candidate.group_id)?;
        ensure_version(&aggregate, command.expected_ledger_version)?;
        if aggregate.enrollment_request.is_some() || aggregate.activation_baton.is_some() {
            return Err(RepairError::OperationAlreadyActive);
        }
        let investigation = aggregate
            .investigation
            .as_ref()
            .filter(|active| active.investigation_id == command.investigation_id)
            .ok_or(RepairError::InvestigationMismatch)?;
        validate_candidate(&command.candidate).map_err(RepairError::InvalidContract)?;
        let execution_receipt = self
            .investigations
            .read_execution_receipt(InvestigationExecutionReceiptQueryV1 {
                operation_id: command.operation_id.clone(),
                receipt_id: command.execution_receipt_id.clone(),
                investigation_id: command.investigation_id,
                candidate_id: command.candidate.candidate_id.clone(),
                candidate_version: command.candidate.candidate_version,
                candidate_hash: command.candidate.candidate_hash.clone(),
            })
            .map_err(|source| port_error("investigation execution receipt read", source))?;
        if execution_receipt.receipt.receipt_id != command.execution_receipt_id
            || execution_receipt.receipt.receipt_hash != command.expected_execution_receipt_hash
        {
            return Err(RepairError::InvestigationMismatch);
        }
        validate_authenticated_investigation_execution(
            &execution_receipt,
            investigation,
            &command.candidate,
        )
        .map_err(RepairError::InvalidContract)?;
        self.verify_candidate_artifacts(
            &command.operation_id,
            RepairArtifactVerificationPurposeV1::CandidateRegistration,
            &command.candidate,
        )?;
        self.append_and_reload(
            &aggregate,
            command.operation_id,
            vec![RepairEventV1::CandidateRegistered {
                candidate: command.candidate,
                execution_receipt,
            }],
        )
        .map(|result| result.aggregate)
    }

    /// Queries the helper and commits its exact supported/degraded report.
    pub fn query_activation_capability(
        &self,
        command: QueryActivationCapabilityV1,
    ) -> Result<PlatformCapabilityReportV1, RepairError> {
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            let report = match existing.events.as_slice() {
                [
                    RepairEventV1::CapabilityReported {
                        queried_at_epoch_ms,
                        report,
                    },
                ] if report.candidate_id == command.candidate_id
                    && report.candidate_version == command.expected_candidate_version
                    && report.candidate_hash == command.expected_candidate_hash
                    && *queried_at_epoch_ms == command.now_epoch_ms =>
                {
                    report.clone()
                }
                _ => return Err(RepairError::OperationConflict),
            };
            return Ok(report);
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
        if aggregate.activation_baton.is_some() && !prior_activation_was_unsupported {
            return Err(RepairError::OperationAlreadyActive);
        }
        let candidate = active_candidate_exact(
            &aggregate,
            &command.candidate_id,
            command.expected_candidate_version,
            &command.expected_candidate_hash,
        )?
        .clone();
        let report = self
            .bootstrap
            .query_activation_capability(ActivationCapabilityQueryV1 {
                operation_id: command.operation_id.clone(),
                group_id: command.group_id,
                candidate,
                now_epoch_ms: command.now_epoch_ms,
            })
            .map_err(|source| port_error("bootstrap capability query", source))?;
        validate_capability_report_fresh(&report, command.now_epoch_ms)
            .map_err(RepairError::InvalidContract)?;
        if report.candidate_id != command.candidate_id
            || report.candidate_version != command.expected_candidate_version
            || report.candidate_hash != command.expected_candidate_hash
        {
            return Err(RepairError::CandidateMismatch);
        }
        let appended = self.append_and_reload(
            &aggregate,
            command.operation_id,
            vec![RepairEventV1::CapabilityReported {
                queried_at_epoch_ms: command.now_epoch_ms,
                report: report.clone(),
            }],
        )?;
        if appended.aggregate.latest_capability_report.as_ref() != Some(&report) {
            return Err(RepairError::OperationConflict);
        }
        Ok(report)
    }

    /// Commits the explicit enrollment request before invoking the helper, then
    /// commits preparation evidence without treating it as activation.
    pub fn request_managed_local_enrollment(
        &self,
        command: RequestManagedLocalEnrollmentV1,
    ) -> Result<EnrollmentPreparedV1, RepairError> {
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            let request = match existing.events.as_slice() {
                [
                    RepairEventV1::EnrollmentRequested {
                        requested_at_epoch_ms,
                        request,
                    },
                ] if request.request_id == command.request_id
                    && request.explicit_user_decision_id == command.explicit_user_decision_id
                    && request.group_id == command.group_id
                    && request.candidate_id == command.candidate_id
                    && request.candidate_version == command.expected_candidate_version
                    && request.candidate_hash == command.expected_candidate_hash
                    && request.capability_report_id == command.expected_capability_report_id
                    && request.capability_digest == command.expected_capability_digest
                    && *requested_at_epoch_ms == command.now_epoch_ms =>
                {
                    request.clone()
                }
                _ => return Err(RepairError::OperationConflict),
            };
            let aggregate = self.load_aggregate(&command.group_id)?;
            return self.continue_enrollment(aggregate, request, &command.operation_id);
        }
        let aggregate = self.load_aggregate(&command.group_id)?;
        ensure_version(&aggregate, command.expected_ledger_version)?;
        let candidate = active_candidate_exact(
            &aggregate,
            &command.candidate_id,
            command.expected_candidate_version,
            &command.expected_candidate_hash,
        )?;
        let report = exact_report(
            &aggregate,
            &command.expected_capability_report_id,
            &command.expected_capability_digest,
            command.now_epoch_ms,
        )?;
        if report.eligibility != ActivationEligibilityV1::EnrollmentRequired {
            return Err(RepairError::ActivationUnavailable(report.eligibility));
        }
        let projected_provenance_hash = match &report.build_origin {
            BuildOriginV1::SourceCheckout {
                projected_provenance_hash,
            } if projected_provenance_hash == &candidate.provenance.provenance_hash => {
                projected_provenance_hash.clone()
            }
            _ => {
                return Err(RepairError::InvalidContract(
                    "enrollment provenance does not match the candidate",
                ));
            }
        };
        let request = ManagedLocalEnrollmentRequestV1 {
            request_id: command.request_id,
            explicit_user_decision_id: command.explicit_user_decision_id,
            group_id: aggregate.group_id.clone(),
            candidate_id: candidate.candidate_id.clone(),
            candidate_version: candidate.candidate_version,
            candidate_hash: candidate.candidate_hash.clone(),
            projected_provenance_hash,
            whole_bundle: candidate.build_bundle.clone(),
            capability_report_id: report.report_id.clone(),
            capability_digest: report.capability_digest.clone(),
        };
        self.verify_enrollment_artifacts(&command.operation_id, &request)?;
        let operation_id = command.operation_id.clone();
        let after_request = self.append_and_reload(
            &aggregate,
            command.operation_id,
            vec![RepairEventV1::EnrollmentRequested {
                requested_at_epoch_ms: command.now_epoch_ms,
                request: request.clone(),
            }],
        )?;
        self.continue_enrollment(after_request.aggregate, request, &operation_id)
    }

    fn continue_enrollment(
        &self,
        aggregate: RepairAggregateV1,
        request: ManagedLocalEnrollmentRequestV1,
        operation_id: &aworkit_protocol::StableId,
    ) -> Result<EnrollmentPreparedV1, RepairError> {
        if aggregate.enrollment_request.as_ref() != Some(&request) {
            return Err(RepairError::OperationConflict);
        }
        if let Some(prepared) = aggregate
            .enrollment_prepared
            .as_ref()
            .filter(|prepared| prepared.request_id == request.request_id)
        {
            return Ok(prepared.clone());
        }
        if aggregate.phase != Some(RepairPhaseV1::EnrollmentPending) {
            return Err(RepairError::OperationConflict);
        }
        self.verify_enrollment_artifacts(operation_id, &request)?;
        let prepared = self
            .bootstrap
            .prepare_managed_local_enrollment(request.clone())
            .map_err(|source| port_error("bootstrap enrollment preparation", source))?;
        validate_enrollment_prepared(&prepared).map_err(RepairError::InvalidContract)?;
        if prepared.request_id != request.request_id {
            return Err(RepairError::InvalidContract(
                "enrollment preparation request id mismatch",
            ));
        }
        let prepared_id = prepared.preparation_id.clone();
        let appended = self.append_and_reload(
            &aggregate,
            prepared_id,
            vec![RepairEventV1::EnrollmentPrepared {
                prepared: prepared.clone(),
            }],
        )?;
        if appended.aggregate.enrollment_request.as_ref() != Some(&request)
            || appended.aggregate.enrollment_prepared.as_ref() != Some(&prepared)
        {
            return Err(RepairError::OperationConflict);
        }
        Ok(prepared)
    }

    /// Rejects or defers a candidate with no checkpoint, baton, or helper call.
    pub fn reject_or_defer_candidate(
        &self,
        command: RejectCandidateV1,
    ) -> Result<RepairAggregateV1, RepairError> {
        let events = vec![RepairEventV1::CandidateDecided {
            decision: command.decision.clone(),
        }];
        if let Some(existing) = self.load_operation(&command.group_id, &command.operation_id)? {
            if existing.events != events {
                return Err(RepairError::OperationConflict);
            }
            return self.load_aggregate(&command.group_id);
        }
        let aggregate = self.load_aggregate(&command.group_id)?;
        ensure_version(&aggregate, command.expected_ledger_version)?;
        active_candidate_exact(
            &aggregate,
            &command.decision.candidate_id,
            command.decision.candidate_version,
            &aggregate
                .candidate_exact(
                    &command.decision.candidate_id,
                    command.decision.candidate_version,
                )
                .ok_or(RepairError::CandidateMismatch)?
                .candidate_hash,
        )?;
        if !matches!(
            command.decision.disposition,
            RepairCandidateDispositionV1::Rejected | RepairCandidateDispositionV1::Deferred
        ) {
            return Err(RepairError::InvalidContract(
                "candidate decision is not reject or defer",
            ));
        }
        self.append_and_reload(&aggregate, command.operation_id, events)
            .map(|result| result.aggregate)
    }
}
