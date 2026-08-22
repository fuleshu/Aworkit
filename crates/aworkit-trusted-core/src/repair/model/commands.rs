//! Explicit user and lifecycle commands accepted by the repair orchestrator.

use aworkit_protocol::{ProcessGeneration, StableId};

use super::{
    bootstrap::{
        BootstrapAcceptedAdmissionV1, BootstrapDeadlinesV1, BootstrapResultV1,
        CoreQuiescenceFactsV1, RepairActivationBatonV1, RepairCandidateDecisionV1,
    },
    evidence::{
        ErrorOccurrenceV1, FocusedVerificationEvidenceV1, RepairCandidateV1,
        RepairInvestigationBudgetV1,
    },
};

/// Command that records evidence but does not start an investigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordRecurringFailureV1 {
    pub operation_id: StableId,
    pub group_id: StableId,
    pub expected_ledger_version: u64,
    pub occurrence: ErrorOccurrenceV1,
}

/// Explicit user command to start one bounded Management investigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartInvestigationV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub investigation_id: StableId,
    pub explicit_user_decision_id: StableId,
    pub group_id: StableId,
    pub management_chat_id: StableId,
    pub management_run_id: StableId,
    pub requested_capability_ids: Vec<StableId>,
    pub budget: RepairInvestigationBudgetV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterRepairCandidateV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub investigation_id: StableId,
    pub execution_receipt_id: StableId,
    pub expected_execution_receipt_hash: String,
    pub candidate: RepairCandidateV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryActivationCapabilityV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub group_id: StableId,
    pub candidate_id: StableId,
    pub expected_candidate_version: u64,
    pub expected_candidate_hash: String,
    pub now_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestManagedLocalEnrollmentV1 {
    pub operation_id: StableId,
    pub request_id: StableId,
    pub explicit_user_decision_id: StableId,
    pub expected_ledger_version: u64,
    pub group_id: StableId,
    pub candidate_id: StableId,
    pub expected_candidate_version: u64,
    pub expected_candidate_hash: String,
    pub expected_capability_report_id: StableId,
    pub expected_capability_digest: String,
    pub now_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectCandidateV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub group_id: StableId,
    pub decision: RepairCandidateDecisionV1,
}

/// Explicit decisive command. All expected fields fence stale UI decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivateAndRestartV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub group_id: StableId,
    pub activation_id: StableId,
    pub baton_id: StableId,
    pub explicit_user_decision_id: StableId,
    pub candidate_id: StableId,
    pub expected_candidate_version: u64,
    pub expected_candidate_hash: String,
    pub expected_capability_report_id: StableId,
    pub expected_capability_digest: String,
    pub current_process_generation: ProcessGeneration,
    pub deadlines: BootstrapDeadlinesV1,
    pub now_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteFocusedVerificationEvidenceV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub group_id: StableId,
    pub activation_id: StableId,
    pub current_process_generation: ProcessGeneration,
    pub evidence: FocusedVerificationEvidenceV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcileBootstrapResultV1 {
    pub operation_id: StableId,
    pub expected_ledger_version: u64,
    pub group_id: StableId,
    pub activation_id: StableId,
    pub current_process_generation: ProcessGeneration,
    pub now_epoch_ms: u64,
}

/// Result of the current-core activation handoff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationHandoffOutcomeV1 {
    Unsupported(BootstrapResultV1),
    ReadyForCoreExit {
        baton: RepairActivationBatonV1,
        admission: BootstrapAcceptedAdmissionV1,
        quiescence: CoreQuiescenceFactsV1,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapReconciliationOutcomeV1 {
    pub duplicate: bool,
    pub receipt: BootstrapResultV1,
    pub resume_dispatched: bool,
}
