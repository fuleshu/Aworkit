//! Ports that keep repair policy in core and mechanics outside it.

use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    AuthenticatedBootstrapResultV1, AuthenticatedInvestigationExecutionReceiptV1,
    BootstrapAdmissionV1, CommittedRepairEventV1, CoreQuiescenceFactsV1, EnrollmentPreparedV1,
    FocusedVerificationEvidenceV1, ManagedLocalEnrollmentRequestV1, ManagementCheckpointRefV1,
    PlatformCapabilityReportV1, RepairActivationBatonV1, RepairArtifactRefV1, RepairCandidateV1,
    RepairEventV1, RepairInvestigationV1,
};

/// Process-neutral boundary failure. Messages must already be safe to expose.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[error("{code}: {message}")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairPortErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Atomic compare-and-swap append to one repair-group stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairLedgerAppendV1 {
    pub operation_id: StableId,
    pub group_id: StableId,
    pub expected_ledger_version: u64,
    pub events: Vec<RepairEventV1>,
}

/// Append acknowledgement. Duplicate operations must name the same payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairLedgerAppendOutcomeV1 {
    pub ledger_version: u64,
    pub duplicate: bool,
}

/// Previously committed payload for one idempotency key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairLedgerOperationV1 {
    pub operation_id: StableId,
    pub group_id: StableId,
    pub events: Vec<RepairEventV1>,
}

/// Durable append-only recurring-error and repair evidence boundary.
///
/// Implementations must return events in contiguous sequence order, apply a
/// batch atomically, and deduplicate by operation ID plus canonical payload.
pub trait RepairLedgerPortV1: Send + Sync {
    fn load_group(
        &self,
        group_id: &StableId,
    ) -> Result<Vec<CommittedRepairEventV1>, RepairPortErrorV1>;

    fn append(
        &self,
        request: RepairLedgerAppendV1,
    ) -> Result<RepairLedgerAppendOutcomeV1, RepairPortErrorV1>;

    /// Looks up an idempotency key before CAS or transition validation.
    ///
    /// The default implementation scans the immutable group stream so existing
    /// adapters remain correct even when they do not have a dedicated index.
    fn load_operation(
        &self,
        group_id: &StableId,
        operation_id: &StableId,
    ) -> Result<Option<RepairLedgerOperationV1>, RepairPortErrorV1> {
        let events = self.load_group(group_id)?;
        let matching = events
            .into_iter()
            .filter(|event| event.operation_id == *operation_id)
            .map(|event| event.event)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            Ok(None)
        } else {
            Ok(Some(RepairLedgerOperationV1 {
                operation_id: operation_id.clone(),
                group_id: group_id.clone(),
                events: matching,
            }))
        }
    }
}

/// Dispatch produced only after the explicit investigation event is durable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairInvestigationDispatchV1 {
    pub operation_id: StableId,
    pub investigation: RepairInvestigationV1,
}

/// Exact protected execution receipt requested during candidate registration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvestigationExecutionReceiptQueryV1 {
    pub operation_id: StableId,
    pub receipt_id: StableId,
    pub investigation_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub candidate_hash: String,
}

/// Existing worker/capability infrastructure implements this idempotent seam.
pub trait RepairInvestigationPortV1: Send + Sync {
    fn dispatch(&self, request: RepairInvestigationDispatchV1) -> Result<(), RepairPortErrorV1>;

    fn read_execution_receipt(
        &self,
        query: InvestigationExecutionReceiptQueryV1,
    ) -> Result<AuthenticatedInvestigationExecutionReceiptV1, RepairPortErrorV1>;
}

/// Why trusted core is re-reading a referenced artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairArtifactVerificationPurposeV1 {
    ErrorOccurrence,
    CandidateRegistration,
    Enrollment,
    Activation,
    FocusedVerification,
}

/// Process-neutral exact-read request to the artifact repository boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairArtifactVerificationRequestV1 {
    pub operation_id: StableId,
    pub purpose: RepairArtifactVerificationPurposeV1,
    pub artifact: RepairArtifactRefV1,
}

/// Closed artifact readiness result; ambiguity never becomes Ready.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepairArtifactReadinessV1 {
    Ready {
        artifact_id: StableId,
        observed_content_hash: String,
        observed_byte_size: u64,
    },
    Missing {
        artifact_id: StableId,
    },
    Unavailable {
        artifact_id: StableId,
        reason: String,
    },
}

/// Exact-read seam implemented by the local artifact repository adapter.
pub trait RepairArtifactIntegrityPortV1: Send + Sync {
    fn verify_ready(
        &self,
        request: RepairArtifactVerificationRequestV1,
    ) -> Result<RepairArtifactReadinessV1, RepairPortErrorV1>;
}

/// Candidate-bound eligibility query sent over authenticated helper IPC.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivationCapabilityQueryV1 {
    pub operation_id: StableId,
    pub group_id: StableId,
    pub candidate: RepairCandidateV1,
    pub now_epoch_ms: u64,
}

/// Read query targeted to one activation and receiving process generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapResultQueryV1 {
    pub operation_id: StableId,
    pub activation_id: StableId,
    pub recipient_process_generation: ProcessGeneration,
}

/// Typed bootstrap protocol; it never grants core authority or commits events.
///
/// Every mutating call is idempotent by its contained stable ID. Admission must
/// return Unsupported before the caller quiesces whenever guarantees drift.
pub trait RepairBootstrapPortV1: Send + Sync {
    fn query_activation_capability(
        &self,
        query: ActivationCapabilityQueryV1,
    ) -> Result<PlatformCapabilityReportV1, RepairPortErrorV1>;

    fn prepare_managed_local_enrollment(
        &self,
        request: ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPreparedV1, RepairPortErrorV1>;

    /// Implementations own the trusted clock and must re-check the baton's
    /// absolute expiry and phase deadlines on every redrive. An expired baton
    /// must fail closed and must never return `Accepted`.
    fn admit_activation(
        &self,
        baton: RepairActivationBatonV1,
    ) -> Result<BootstrapAdmissionV1, RepairPortErrorV1>;

    fn record_core_quiescence(
        &self,
        admission_id: &StableId,
        facts: CoreQuiescenceFactsV1,
    ) -> Result<(), RepairPortErrorV1>;

    fn submit_focused_verification(
        &self,
        activation_id: &StableId,
        evidence: FocusedVerificationEvidenceV1,
    ) -> Result<(), RepairPortErrorV1>;

    fn read_result(
        &self,
        query: BootstrapResultQueryV1,
    ) -> Result<Option<AuthenticatedBootstrapResultV1>, RepairPortErrorV1>;
}

/// Request to freeze the same Management Chat before helper handoff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementCheckpointRequestV1 {
    pub operation_id: StableId,
    pub activation_id: StableId,
    pub group_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub management_chat_id: StableId,
    pub management_run_id: StableId,
}

/// Idempotent same-Chat resume request issued only after receipt commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementResumeRequestV1 {
    pub receipt_id: StableId,
    pub activation_id: StableId,
    pub checkpoint: ManagementCheckpointRefV1,
    pub recipient_process_generation: ProcessGeneration,
}

/// Durable checkpoint and recovery seam for the Management Run.
pub trait ManagementCheckpointPortV1: Send + Sync {
    /// Idempotent by operation and activation ID; uncertain callers redrive it.
    fn create_checkpoint(
        &self,
        request: ManagementCheckpointRequestV1,
    ) -> Result<ManagementCheckpointRefV1, RepairPortErrorV1>;

    /// Implementations deduplicate by receipt ID so recovery can retry safely.
    fn resume_same_chat(&self, request: ManagementResumeRequestV1)
    -> Result<(), RepairPortErrorV1>;
}

/// Current-generation cleanup request made only after helper admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoreQuiescenceRequestV1 {
    pub activation_id: StableId,
    pub process_generation: ProcessGeneration,
    pub deadline_ms: u64,
}

/// Existing supervision supplies facts; it never switches slots or relaunches.
pub trait CoreQuiescencePortV1: Send + Sync {
    /// Idempotent for the exact activation and process generation. The
    /// implementation must enforce `deadline_ms`; timeout is returned as facts
    /// with `timed_out = true`, which core rejects before helper handoff.
    fn quiesce_current_generation(
        &self,
        request: CoreQuiescenceRequestV1,
    ) -> Result<CoreQuiescenceFactsV1, RepairPortErrorV1>;
}
