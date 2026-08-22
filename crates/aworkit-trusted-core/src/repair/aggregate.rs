//! Deterministic fold for the durable repair event stream.

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    ActivationEligibilityV1, AuthenticatedBootstrapResultV1,
    AuthenticatedInvestigationExecutionReceiptV1, BootstrapAcceptedAdmissionV1,
    BootstrapResultKindV1, BuildBundleRefV1, BuildOriginV1, CommittedRepairEventV1,
    CoreQuiescenceFactsV1, EnrollmentPreparedV1, ErrorOccurrenceV1, FocusedVerificationEvidenceV1,
    ManagedLocalEnrollmentRequestV1, ManagementCheckpointRefV1, PlatformCapabilityReportV1,
    RepairActivationBatonV1, RepairActivationDecisionV1, RepairCandidateDecisionV1,
    RepairCandidateV1, RepairEventV1, RepairInvestigationV1, RepairPhaseV1, RepairRegressionV1,
    validation::{
        repair_group_id_for_fingerprint_v1, total_deadline_ms, validate_admission,
        validate_authenticated_investigation_execution, validate_authenticated_result,
        validate_candidate, validate_candidate_decision, validate_capability_report_fresh,
        validate_capability_report_shape, validate_checkpoint, validate_enrollment_prepared,
        validate_enrollment_request, validate_focused_evidence_against_plan,
        validate_investigation, validate_occurrence, validate_quiescence_facts,
        validate_repair_activation_baton, validate_result_fresh,
    },
};

/// A replay failure means the durable stream is corrupt or violates policy.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RepairAggregateError {
    #[error("repair ledger sequence is not contiguous")]
    NonContiguousSequence,
    #[error("repair ledger event belongs to another group")]
    GroupMismatch,
    #[error("repair ledger event payload is invalid: {0}")]
    InvalidEvent(&'static str),
    #[error("repair ledger transition is illegal: {0}")]
    IllegalTransition(&'static str),
}

/// Core-owned projection rebuilt exclusively from committed repair facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairAggregateV1 {
    pub group_id: StableId,
    pub fingerprint: Option<String>,
    pub ledger_version: u64,
    pub phase: Option<RepairPhaseV1>,
    pub occurrences: Vec<ErrorOccurrenceV1>,
    pub investigation: Option<RepairInvestigationV1>,
    pub candidates: Vec<RepairCandidateV1>,
    pub candidate_execution_receipts: Vec<AuthenticatedInvestigationExecutionReceiptV1>,
    pub latest_capability_report: Option<PlatformCapabilityReportV1>,
    pub enrollment_request: Option<ManagedLocalEnrollmentRequestV1>,
    pub enrollment_prepared: Option<EnrollmentPreparedV1>,
    pub candidate_decision: Option<RepairCandidateDecisionV1>,
    pub activation_decision: Option<RepairActivationDecisionV1>,
    pub management_checkpoint: Option<ManagementCheckpointRefV1>,
    pub activation_baton: Option<RepairActivationBatonV1>,
    pub bootstrap_admission: Option<BootstrapAcceptedAdmissionV1>,
    pub quiescence: Option<CoreQuiescenceFactsV1>,
    pub focused_verification: Option<FocusedVerificationEvidenceV1>,
    pub bootstrap_result: Option<AuthenticatedBootstrapResultV1>,
    pub regressions: Vec<RepairRegressionV1>,
}

impl RepairAggregateV1 {
    /// Replays a complete group stream and rejects gaps or illegal state.
    pub fn rehydrate(
        group_id: StableId,
        events: &[CommittedRepairEventV1],
    ) -> Result<Self, RepairAggregateError> {
        let mut aggregate = Self::empty(group_id);
        for committed in events {
            let expected = aggregate.ledger_version.saturating_add(1);
            if committed.ledger_sequence != expected {
                return Err(RepairAggregateError::NonContiguousSequence);
            }
            if committed.group_id != aggregate.group_id {
                return Err(RepairAggregateError::GroupMismatch);
            }
            aggregate.apply(&committed.event)?;
            aggregate.ledger_version = committed.ledger_sequence;
        }
        Ok(aggregate)
    }

    #[must_use]
    pub fn empty(group_id: StableId) -> Self {
        Self {
            group_id,
            fingerprint: None,
            ledger_version: 0,
            phase: None,
            occurrences: Vec::new(),
            investigation: None,
            candidates: Vec::new(),
            candidate_execution_receipts: Vec::new(),
            latest_capability_report: None,
            enrollment_request: None,
            enrollment_prepared: None,
            candidate_decision: None,
            activation_decision: None,
            management_checkpoint: None,
            activation_baton: None,
            bootstrap_admission: None,
            quiescence: None,
            focused_verification: None,
            bootstrap_result: None,
            regressions: Vec::new(),
        }
    }

    #[must_use]
    pub fn active_candidate(&self) -> Option<&RepairCandidateV1> {
        self.candidates.last()
    }

    #[must_use]
    pub fn candidate_exact(
        &self,
        candidate_id: &StableId,
        version: u64,
    ) -> Option<&RepairCandidateV1> {
        self.candidates.iter().find(|candidate| {
            candidate.candidate_id == *candidate_id && candidate.candidate_version == version
        })
    }

    #[must_use]
    pub fn verified_build(&self) -> Option<(&RepairCandidateV1, &AuthenticatedBootstrapResultV1)> {
        let result = self.bootstrap_result.as_ref()?;
        if !matches!(
            result.receipt.result,
            BootstrapResultKindV1::ActivatedVerified { .. }
        ) {
            return None;
        }
        Some((self.active_candidate()?, result))
    }

    /// Applies a prospective atomic batch without mutating the durable fold.
    pub(crate) fn preview(&self, events: &[RepairEventV1]) -> Result<Self, RepairAggregateError> {
        let mut preview = self.clone();
        for event in events {
            preview.apply(event)?;
            preview.ledger_version = preview.ledger_version.saturating_add(1);
        }
        Ok(preview)
    }

    fn apply(&mut self, event: &RepairEventV1) -> Result<(), RepairAggregateError> {
        match event {
            RepairEventV1::FailureRecorded { occurrence } => self.record_failure(occurrence),
            RepairEventV1::InvestigationStarted { investigation } => {
                self.start_investigation(investigation)
            }
            RepairEventV1::CandidateRegistered {
                candidate,
                execution_receipt,
            } => self.register_candidate(candidate, execution_receipt),
            RepairEventV1::CapabilityReported {
                queried_at_epoch_ms,
                report,
            } => self.record_capability(*queried_at_epoch_ms, report),
            RepairEventV1::EnrollmentRequested {
                requested_at_epoch_ms,
                request,
            } => self.request_enrollment(*requested_at_epoch_ms, request),
            RepairEventV1::EnrollmentPrepared { prepared } => self.prepare_enrollment(prepared),
            RepairEventV1::CandidateDecided { decision } => self.decide_candidate(decision),
            RepairEventV1::ActivationPrepared {
                decision,
                checkpoint,
                baton,
            } => self.prepare_activation(decision, checkpoint, baton),
            RepairEventV1::BootstrapAdmissionAccepted { admission } => {
                self.accept_bootstrap_admission(admission)
            }
            RepairEventV1::CoreQuiesced { facts } => self.record_quiescence(facts),
            RepairEventV1::FocusedVerificationSubmitted {
                activation_id,
                process_generation,
                evidence,
            } => self.record_focused_verification(activation_id, *process_generation, evidence),
            RepairEventV1::BootstrapResultReconciled {
                reconciled_at_epoch_ms,
                result,
            } => self.reconcile_bootstrap_result(*reconciled_at_epoch_ms, result),
            RepairEventV1::RegressionRecorded { regression } => self.record_regression(regression),
        }
    }

    fn record_failure(
        &mut self,
        occurrence: &ErrorOccurrenceV1,
    ) -> Result<(), RepairAggregateError> {
        let preserve_active_phase = matches!(
            self.phase,
            Some(
                RepairPhaseV1::Investigating
                    | RepairPhaseV1::EnrollmentPending
                    | RepairPhaseV1::ActivationPrepared
                    | RepairPhaseV1::AwaitingBootstrapResult
                    | RepairPhaseV1::VerificationSubmitted
            )
        );
        validate_occurrence(occurrence).map_err(RepairAggregateError::InvalidEvent)?;
        let resolved_group =
            repair_group_id_for_fingerprint_v1(&occurrence.fingerprint).map_err(|_| {
                RepairAggregateError::InvalidEvent("invalid recurring-error fingerprint")
            })?;
        if resolved_group != self.group_id {
            return Err(RepairAggregateError::IllegalTransition(
                "recurring-error group is not the stable fingerprint resolution",
            ));
        }
        if self
            .occurrences
            .iter()
            .any(|seen| seen.occurrence_id == occurrence.occurrence_id)
        {
            return Err(RepairAggregateError::IllegalTransition(
                "duplicate occurrence id",
            ));
        }
        if let Some(fingerprint) = &self.fingerprint {
            if fingerprint != &occurrence.fingerprint {
                return Err(RepairAggregateError::IllegalTransition(
                    "fingerprint changed within group",
                ));
            }
        } else {
            self.fingerprint = Some(occurrence.fingerprint.clone());
        }
        self.occurrences.push(occurrence.clone());
        if !preserve_active_phase {
            self.phase = Some(RepairPhaseV1::Observed);
        }
        Ok(())
    }

    fn start_investigation(
        &mut self,
        investigation: &RepairInvestigationV1,
    ) -> Result<(), RepairAggregateError> {
        validate_investigation(investigation).map_err(RepairAggregateError::InvalidEvent)?;
        if self.fingerprint.is_none() || investigation.group_id != self.group_id {
            return Err(RepairAggregateError::IllegalTransition(
                "investigation has no matching recurring-error group",
            ));
        }
        if matches!(
            self.phase,
            Some(
                RepairPhaseV1::Investigating
                    | RepairPhaseV1::ActivationPrepared
                    | RepairPhaseV1::AwaitingBootstrapResult
                    | RepairPhaseV1::VerificationSubmitted
            )
        ) || (self.activation_baton.is_some() && self.bootstrap_result.is_none())
            || (self.enrollment_request.is_some() && self.enrollment_prepared.is_none())
        {
            return Err(RepairAggregateError::IllegalTransition(
                "another investigation or activation is active",
            ));
        }
        self.latest_capability_report = None;
        self.enrollment_request = None;
        self.enrollment_prepared = None;
        self.candidate_decision = None;
        self.activation_decision = None;
        self.management_checkpoint = None;
        self.activation_baton = None;
        self.bootstrap_admission = None;
        self.quiescence = None;
        self.focused_verification = None;
        self.bootstrap_result = None;
        self.investigation = Some(investigation.clone());
        self.phase = Some(RepairPhaseV1::Investigating);
        Ok(())
    }

    fn register_candidate(
        &mut self,
        candidate: &RepairCandidateV1,
        execution_receipt: &AuthenticatedInvestigationExecutionReceiptV1,
    ) -> Result<(), RepairAggregateError> {
        validate_candidate(candidate).map_err(RepairAggregateError::InvalidEvent)?;
        if self.enrollment_request.is_some() || self.activation_baton.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "candidate cannot replace an active or completed handoff",
            ));
        }
        let investigation =
            self.investigation
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "candidate has no explicit investigation",
                ))?;
        validate_authenticated_investigation_execution(execution_receipt, investigation, candidate)
            .map_err(RepairAggregateError::InvalidEvent)?;
        if candidate.group_id != self.group_id
            || candidate.built_under_authority_manifest_hash
                != investigation.authority.authority_manifest_hash
            || candidate
                .disclosure
                .verification_plan
                .checks
                .iter()
                .any(|check| {
                    !investigation
                        .authority
                        .capability_ids
                        .contains(&check.capability_id)
                })
        {
            return Err(RepairAggregateError::IllegalTransition(
                "candidate is not bound to this investigation authority",
            ));
        }
        let previous = self
            .candidates
            .iter()
            .filter(|stored| stored.candidate_id == candidate.candidate_id)
            .max_by_key(|stored| stored.candidate_version);
        match previous {
            Some(previous)
                if candidate.candidate_version != previous.candidate_version.saturating_add(1) =>
            {
                return Err(RepairAggregateError::IllegalTransition(
                    "candidate version is not contiguous",
                ));
            }
            None if candidate.candidate_version != 1 => {
                return Err(RepairAggregateError::IllegalTransition(
                    "first candidate version must be one",
                ));
            }
            _ => {}
        }
        self.candidates.push(candidate.clone());
        self.candidate_execution_receipts
            .push(execution_receipt.clone());
        self.latest_capability_report = None;
        self.enrollment_request = None;
        self.enrollment_prepared = None;
        self.candidate_decision = None;
        self.phase = Some(RepairPhaseV1::CandidateReady);
        Ok(())
    }

    fn record_capability(
        &mut self,
        queried_at_epoch_ms: u64,
        report: &PlatformCapabilityReportV1,
    ) -> Result<(), RepairAggregateError> {
        validate_capability_report_shape(report).map_err(RepairAggregateError::InvalidEvent)?;
        validate_capability_report_fresh(report, queried_at_epoch_ms)
            .map_err(RepairAggregateError::InvalidEvent)?;
        let prior_activation_was_unsupported =
            self.bootstrap_result.as_ref().is_some_and(|result| {
                matches!(
                    result.receipt.result,
                    BootstrapResultKindV1::Unsupported { .. }
                )
            });
        if self.activation_baton.is_some() && !prior_activation_was_unsupported {
            return Err(RepairAggregateError::IllegalTransition(
                "capability report cannot replace an active or completed handoff report",
            ));
        }
        if let Some(previous) = &self.latest_capability_report {
            if report.capability_generation < previous.capability_generation
                || (report.capability_generation == previous.capability_generation
                    && report != previous)
            {
                return Err(RepairAggregateError::IllegalTransition(
                    "capability generation regressed or changed without advancing",
                ));
            }
        }
        let candidate = self
            .active_candidate()
            .ok_or(RepairAggregateError::IllegalTransition(
                "capability report has no candidate",
            ))?;
        if report.candidate_id != candidate.candidate_id
            || report.candidate_version != candidate.candidate_version
            || report.candidate_hash != candidate.candidate_hash
        {
            return Err(RepairAggregateError::IllegalTransition(
                "capability report is bound to another candidate",
            ));
        }
        self.latest_capability_report = Some(report.clone());
        Ok(())
    }

    fn request_enrollment(
        &mut self,
        requested_at_epoch_ms: u64,
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<(), RepairAggregateError> {
        validate_enrollment_request(request).map_err(RepairAggregateError::InvalidEvent)?;
        let report = self.latest_capability_report.as_ref().ok_or(
            RepairAggregateError::IllegalTransition("enrollment has no capability report"),
        )?;
        validate_capability_report_fresh(report, requested_at_epoch_ms)
            .map_err(RepairAggregateError::InvalidEvent)?;
        let candidate = self
            .active_candidate()
            .ok_or(RepairAggregateError::IllegalTransition(
                "enrollment has no candidate",
            ))?;
        let projected_origin_matches = matches!(
            &report.build_origin,
            BuildOriginV1::SourceCheckout {
                projected_provenance_hash,
            } if projected_provenance_hash == &request.projected_provenance_hash
        );
        if report.eligibility != ActivationEligibilityV1::EnrollmentRequired
            || request.group_id != self.group_id
            || request.candidate_id != report.candidate_id
            || request.candidate_version != report.candidate_version
            || request.candidate_hash != report.candidate_hash
            || request.capability_report_id != report.report_id
            || request.capability_digest != report.capability_digest
            || request.projected_provenance_hash != candidate.provenance.provenance_hash
            || request.whole_bundle != candidate.build_bundle
            || !projected_origin_matches
        {
            return Err(RepairAggregateError::IllegalTransition(
                "enrollment request does not match an EnrollmentRequired report",
            ));
        }
        if self.activation_baton.is_some() || self.enrollment_request.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "enrollment or activation is already active",
            ));
        }
        self.enrollment_request = Some(request.clone());
        self.phase = Some(RepairPhaseV1::EnrollmentPending);
        Ok(())
    }

    fn prepare_enrollment(
        &mut self,
        prepared: &EnrollmentPreparedV1,
    ) -> Result<(), RepairAggregateError> {
        validate_enrollment_prepared(prepared).map_err(RepairAggregateError::InvalidEvent)?;
        let request =
            self.enrollment_request
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "enrollment was not requested",
                ))?;
        if prepared.request_id != request.request_id {
            return Err(RepairAggregateError::IllegalTransition(
                "enrollment response request id mismatch",
            ));
        }
        if self.enrollment_prepared.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "enrollment preparation is immutable",
            ));
        }
        self.enrollment_prepared = Some(prepared.clone());
        self.phase = Some(RepairPhaseV1::EnrollmentPrepared);
        Ok(())
    }

    fn decide_candidate(
        &mut self,
        decision: &RepairCandidateDecisionV1,
    ) -> Result<(), RepairAggregateError> {
        validate_candidate_decision(decision).map_err(RepairAggregateError::InvalidEvent)?;
        if self
            .candidate_exact(&decision.candidate_id, decision.candidate_version)
            .is_none()
        {
            return Err(RepairAggregateError::IllegalTransition(
                "candidate decision references an unknown version",
            ));
        }
        if self.activation_baton.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "candidate cannot be rejected after activation preparation",
            ));
        }
        self.candidate_decision = Some(decision.clone());
        self.phase = Some(RepairPhaseV1::CandidateRejected);
        Ok(())
    }

    fn prepare_activation(
        &mut self,
        decision: &RepairActivationDecisionV1,
        checkpoint: &ManagementCheckpointRefV1,
        baton: &RepairActivationBatonV1,
    ) -> Result<(), RepairAggregateError> {
        validate_checkpoint(checkpoint).map_err(RepairAggregateError::InvalidEvent)?;
        validate_repair_activation_baton(baton).map_err(RepairAggregateError::InvalidEvent)?;
        let prior_activation_was_unsupported =
            self.bootstrap_result.as_ref().is_some_and(|result| {
                matches!(
                    result.receipt.result,
                    BootstrapResultKindV1::Unsupported { .. }
                )
            });
        if (self.activation_baton.is_some() && !prior_activation_was_unsupported)
            || (self.enrollment_request.is_some() && self.enrollment_prepared.is_none())
        {
            return Err(RepairAggregateError::IllegalTransition(
                "enrollment or activation is already active",
            ));
        }
        let candidate = self
            .candidate_exact(&decision.candidate_id, decision.expected_candidate_version)
            .ok_or(RepairAggregateError::IllegalTransition(
                "activation candidate version is missing",
            ))?;
        let report = self.latest_capability_report.as_ref().ok_or(
            RepairAggregateError::IllegalTransition("activation capability is missing"),
        )?;
        let investigation =
            self.investigation
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "activation investigation is missing",
                ))?;
        let expected_expiry = total_deadline_ms(&baton.deadlines)
            .and_then(|deadline| decision.decided_at_epoch_ms.checked_add(deadline));
        let (managed_enrollment_digest, previous_working_build) = match &report.build_origin {
            BuildOriginV1::ManagedLocal {
                enrollment_digest, ..
            } => (
                Some(enrollment_digest),
                report.previous_working_build.as_ref(),
            ),
            _ => (None, report.previous_working_build.as_ref()),
        };
        if decision.expected_candidate_hash != candidate.candidate_hash
            || decision.expected_capability_report_id != report.report_id
            || decision.expected_capability_digest != report.capability_digest
            || report.eligibility != ActivationEligibilityV1::SupportedManagedLocal
            || decision.decided_at_epoch_ms < report.valid_from_epoch_ms
            || decision.decided_at_epoch_ms > report.expires_at_epoch_ms
            || checkpoint.chat_id != investigation.management_chat_id
            || checkpoint.run_id != investigation.management_run_id
            || baton.activation_id != decision.activation_id
            || baton.group_id != self.group_id
            || baton.candidate_id != candidate.candidate_id
            || baton.candidate_version != candidate.candidate_version
            || baton.candidate_hash != candidate.candidate_hash
            || baton.candidate_bundle != candidate.build_bundle
            || baton.disclosure_hash != candidate.disclosure.disclosure_hash
            || baton.provenance_hash != candidate.provenance.provenance_hash
            || managed_enrollment_digest != Some(&baton.enrollment_digest)
            || baton.capability_report_id != report.report_id
            || baton.capability_generation != report.capability_generation
            || baton.capability_digest != report.capability_digest
            || previous_working_build != Some(&baton.previous_working_build)
            || baton.previous_working_build != candidate.disclosure.rollback_point
            || baton.management_checkpoint != *checkpoint
            || baton.verification_plan != candidate.disclosure.verification_plan
            || expected_expiry != Some(baton.expires_at_epoch_ms)
        {
            return Err(RepairAggregateError::IllegalTransition(
                "activation decision, checkpoint, candidate, report, or baton mismatch",
            ));
        }
        self.activation_decision = Some(decision.clone());
        self.management_checkpoint = Some(checkpoint.clone());
        self.activation_baton = Some(baton.clone());
        self.bootstrap_admission = None;
        self.quiescence = None;
        self.focused_verification = None;
        self.bootstrap_result = None;
        self.phase = Some(RepairPhaseV1::ActivationPrepared);
        Ok(())
    }

    fn accept_bootstrap_admission(
        &mut self,
        admission: &BootstrapAcceptedAdmissionV1,
    ) -> Result<(), RepairAggregateError> {
        validate_admission(admission).map_err(RepairAggregateError::InvalidEvent)?;
        let baton =
            self.activation_baton
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "bootstrap admission has no baton",
                ))?;
        if admission.activation_id != baton.activation_id
            || admission.baton_hash != baton.baton_hash
            || admission.candidate_process_generation != baton.candidate_process_generation
            || admission.rollback_process_generation != baton.rollback_process_generation
            || self.bootstrap_admission.is_some()
        {
            return Err(RepairAggregateError::IllegalTransition(
                "bootstrap admission does not match the baton",
            ));
        }
        self.bootstrap_admission = Some(admission.clone());
        self.phase = Some(RepairPhaseV1::AwaitingBootstrapResult);
        Ok(())
    }

    fn record_quiescence(
        &mut self,
        facts: &CoreQuiescenceFactsV1,
    ) -> Result<(), RepairAggregateError> {
        validate_quiescence_facts(facts).map_err(RepairAggregateError::InvalidEvent)?;
        if facts.timed_out || facts.orphan_risk {
            return Err(RepairAggregateError::InvalidEvent(
                "unsafe quiescence facts cannot be committed",
            ));
        }
        let baton =
            self.activation_baton
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "quiescence has no activation baton",
                ))?;
        if self.bootstrap_admission.is_none()
            || self.quiescence.is_some()
            || facts.activation_id != baton.activation_id
            || facts.process_generation != baton.current_process_generation
        {
            return Err(RepairAggregateError::IllegalTransition(
                "quiescence happened before matching helper admission",
            ));
        }
        self.quiescence = Some(facts.clone());
        Ok(())
    }

    fn record_focused_verification(
        &mut self,
        activation_id: &StableId,
        process_generation: aworkit_protocol::ProcessGeneration,
        evidence: &FocusedVerificationEvidenceV1,
    ) -> Result<(), RepairAggregateError> {
        let baton =
            self.activation_baton
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "verification has no activation baton",
                ))?;
        validate_focused_evidence_against_plan(evidence, &baton.verification_plan)
            .map_err(RepairAggregateError::InvalidEvent)?;
        if self.bootstrap_result.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "verification cannot be submitted after a terminal bootstrap result",
            ));
        }
        if activation_id != &baton.activation_id
            || process_generation != baton.candidate_process_generation
            || evidence.plan_id != baton.verification_plan.plan_id
            || evidence.plan_hash != baton.verification_plan.plan_hash
            || self.bootstrap_admission.is_none()
            || self.quiescence.is_none()
        {
            return Err(RepairAggregateError::IllegalTransition(
                "verification does not match the baton plan",
            ));
        }
        if self.focused_verification.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "focused-verification evidence is immutable",
            ));
        }
        self.focused_verification = Some(evidence.clone());
        self.phase = Some(RepairPhaseV1::VerificationSubmitted);
        Ok(())
    }

    fn reconcile_bootstrap_result(
        &mut self,
        reconciled_at_epoch_ms: u64,
        result: &AuthenticatedBootstrapResultV1,
    ) -> Result<(), RepairAggregateError> {
        let baton =
            self.activation_baton
                .as_ref()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "bootstrap result has no activation baton",
                ))?;
        validate_authenticated_result(result, baton, self.bootstrap_admission.as_ref())
            .map_err(RepairAggregateError::InvalidEvent)?;
        validate_result_fresh(result, baton, reconciled_at_epoch_ms)
            .map_err(RepairAggregateError::InvalidEvent)?;
        if self.bootstrap_result.is_some() {
            return Err(RepairAggregateError::IllegalTransition(
                "a bootstrap result was already reconciled",
            ));
        }
        if !matches!(
            result.receipt.result,
            BootstrapResultKindV1::Unsupported { .. }
        ) && self.quiescence.is_none()
        {
            return Err(RepairAggregateError::IllegalTransition(
                "bootstrap result has no committed safe quiescence handoff",
            ));
        }
        if let BootstrapResultKindV1::ActivatedVerified {
            focused_verification,
        } = &result.receipt.result
            && self.focused_verification.as_ref() != Some(focused_verification)
        {
            return Err(RepairAggregateError::IllegalTransition(
                "activated receipt lacks the previously submitted focused verification",
            ));
        }
        self.phase = Some(match &result.receipt.result {
            BootstrapResultKindV1::Unsupported { .. } => RepairPhaseV1::CandidateReady,
            BootstrapResultKindV1::ActivatedVerified { .. } => RepairPhaseV1::Verified,
            BootstrapResultKindV1::RolledBack { .. } => RepairPhaseV1::RolledBack,
            BootstrapResultKindV1::ManualRecoveryRequired { .. } => {
                RepairPhaseV1::ManualRecoveryRequired
            }
        });
        self.bootstrap_result = Some(result.clone());
        Ok(())
    }

    fn record_regression(
        &mut self,
        regression: &RepairRegressionV1,
    ) -> Result<(), RepairAggregateError> {
        let (candidate, result) =
            self.verified_build()
                .ok_or(RepairAggregateError::IllegalTransition(
                    "regression has no verified repair",
                ))?;
        if regression.repaired_candidate_id != candidate.candidate_id
            || regression.repaired_receipt_id != result.receipt.receipt_id
            || !self
                .occurrences
                .iter()
                .any(|occurrence| occurrence.occurrence_id == regression.occurrence_id)
            || self
                .regressions
                .iter()
                .any(|seen| seen.regression_id == regression.regression_id)
        {
            return Err(RepairAggregateError::IllegalTransition(
                "regression references do not match verified evidence",
            ));
        }
        self.regressions.push(regression.clone());
        self.phase = Some(RepairPhaseV1::Regression);
        Ok(())
    }

    #[must_use]
    pub fn previous_working_build(&self) -> Option<&BuildBundleRefV1> {
        self.latest_capability_report
            .as_ref()
            .and_then(|report| report.previous_working_build.as_ref())
    }
}
