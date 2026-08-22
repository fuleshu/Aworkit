//! Durable repair lifecycle events and phases.

use aworkit_protocol::ProcessGeneration;
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};

use super::{
    bootstrap::{
        AuthenticatedBootstrapResultV1, BootstrapAcceptedAdmissionV1, CoreQuiescenceFactsV1,
        EnrollmentPreparedV1, ManagedLocalEnrollmentRequestV1, ManagementCheckpointRefV1,
        PlatformCapabilityReportV1, RepairActivationBatonV1, RepairActivationDecisionV1,
        RepairCandidateDecisionV1,
    },
    evidence::{
        ErrorOccurrenceV1, FocusedVerificationEvidenceV1, RepairCandidateV1, RepairInvestigationV1,
    },
    investigation::AuthenticatedInvestigationExecutionReceiptV1,
};

/// One regression occurrence linked to the prior verified repair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairRegressionV1 {
    pub regression_id: StableId,
    pub occurrence_id: StableId,
    pub repaired_candidate_id: StableId,
    pub repaired_receipt_id: StableId,
}

/// Folded repair lifecycle state; no state implies automatic diagnosis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairPhaseV1 {
    Observed,
    Investigating,
    CandidateReady,
    EnrollmentPending,
    EnrollmentPrepared,
    CandidateRejected,
    ActivationPrepared,
    AwaitingBootstrapResult,
    VerificationSubmitted,
    Verified,
    RolledBack,
    ManualRecoveryRequired,
    Regression,
}

/// Events are the only durable inputs accepted by the aggregate fold.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepairEventV1 {
    FailureRecorded {
        occurrence: ErrorOccurrenceV1,
    },
    InvestigationStarted {
        investigation: RepairInvestigationV1,
    },
    CandidateRegistered {
        candidate: RepairCandidateV1,
        execution_receipt: AuthenticatedInvestigationExecutionReceiptV1,
    },
    CapabilityReported {
        queried_at_epoch_ms: u64,
        report: PlatformCapabilityReportV1,
    },
    EnrollmentRequested {
        requested_at_epoch_ms: u64,
        request: ManagedLocalEnrollmentRequestV1,
    },
    EnrollmentPrepared {
        prepared: EnrollmentPreparedV1,
    },
    CandidateDecided {
        decision: RepairCandidateDecisionV1,
    },
    ActivationPrepared {
        decision: RepairActivationDecisionV1,
        checkpoint: ManagementCheckpointRefV1,
        baton: RepairActivationBatonV1,
    },
    BootstrapAdmissionAccepted {
        admission: BootstrapAcceptedAdmissionV1,
    },
    CoreQuiesced {
        facts: CoreQuiescenceFactsV1,
    },
    FocusedVerificationSubmitted {
        activation_id: StableId,
        process_generation: ProcessGeneration,
        evidence: FocusedVerificationEvidenceV1,
    },
    BootstrapResultReconciled {
        reconciled_at_epoch_ms: u64,
        result: AuthenticatedBootstrapResultV1,
    },
    RegressionRecorded {
        regression: RepairRegressionV1,
    },
}

/// Ledger event with storage-assigned contiguous ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommittedRepairEventV1 {
    pub group_id: StableId,
    pub ledger_sequence: u64,
    pub operation_id: StableId,
    pub event: RepairEventV1,
}
