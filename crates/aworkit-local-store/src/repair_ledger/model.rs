//! Public recurring-error and repair evidence records.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Core-normalized group lifecycle. Storage validates transitions but never
/// diagnoses a failure or decides to activate a repair.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorGroupStatus {
    Grouped,
    Investigating,
    CandidatePrepared,
    AwaitingActivation,
    Rejected,
    ActivatedRestarting,
    Verifying,
    Verified,
    RolledBack,
    RegressionReopened,
}

impl ErrorGroupStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Grouped => "grouped",
            Self::Investigating => "investigating",
            Self::CandidatePrepared => "candidate_prepared",
            Self::AwaitingActivation => "awaiting_activation",
            Self::Rejected => "rejected",
            Self::ActivatedRestarting => "activated_restarting",
            Self::Verifying => "verifying",
            Self::Verified => "verified",
            Self::RolledBack => "rolled_back",
            Self::RegressionReopened => "regression_reopened",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, RepairLedgerError> {
        match value {
            "grouped" => Ok(Self::Grouped),
            "investigating" => Ok(Self::Investigating),
            "candidate_prepared" => Ok(Self::CandidatePrepared),
            "awaiting_activation" => Ok(Self::AwaitingActivation),
            "rejected" => Ok(Self::Rejected),
            "activated_restarting" => Ok(Self::ActivatedRestarting),
            "verifying" => Ok(Self::Verifying),
            "verified" => Ok(Self::Verified),
            "rolled_back" => Ok(Self::RolledBack),
            "regression_reopened" => Ok(Self::RegressionReopened),
            _ => Err(RepairLedgerError::Corrupt),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorGroup {
    pub fingerprint: String,
    pub ledger_version: u64,
    pub status: ErrorGroupStatus,
    pub occurrence_count: u64,
    pub first_seen_epoch_ms: u64,
    pub last_seen_epoch_ms: u64,
    pub active_candidate_id: Option<String>,
    pub active_candidate_version: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAvailability {
    Available,
    Expired,
    Unavailable,
    Corrupt,
}

/// Immutable artifact identity; byte availability can later be superseded by
/// an append-only tombstone without rewriting this reference.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceReference {
    pub artifact_id: String,
    pub content_hash: String,
    pub availability: EvidenceAvailability,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvidenceReference {
    pub diagnostic_record_id: String,
    pub availability: EvidenceAvailability,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorOccurrence {
    pub occurrence_id: String,
    pub fingerprint: String,
    pub observed_at_epoch_ms: u64,
    pub summary: String,
    pub semantic_event_id: Option<String>,
    pub attempt_id: Option<String>,
    pub diagnostics: Vec<DiagnosticEvidenceReference>,
    pub evidence: Vec<EvidenceReference>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordOccurrenceRequest {
    pub operation_id: String,
    pub expected_ledger_version: Option<u64>,
    pub occurrence: ErrorOccurrence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionRecord {
    pub regression_id: String,
    pub fingerprint: String,
    pub occurrence_id: String,
    pub prior_status: ErrorGroupStatus,
    pub prior_candidate_id: Option<String>,
    pub recorded_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OccurrenceReceipt {
    pub group: ErrorGroup,
    pub regression: Option<RegressionRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosisRecord {
    pub diagnosis_id: String,
    pub fingerprint: String,
    pub recorded_at_epoch_ms: u64,
    pub summary: String,
    pub evidence: Vec<EvidenceReference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkaroundRecord {
    pub workaround_id: String,
    pub fingerprint: String,
    pub recorded_at_epoch_ms: u64,
    pub summary: String,
    pub consequence_summary: String,
    pub evidence: Vec<EvidenceReference>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerAppendRequest<T> {
    pub operation_id: String,
    pub expected_ledger_version: u64,
    pub record: T,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackPoint {
    pub rollback_point_id: String,
    pub previous_working_build: EvidenceReference,
}

/// Every disclosure category is explicit and immutable. Empty evidence is not
/// accepted as a "complete" repair candidate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateEvidence {
    pub diff: EvidenceReference,
    pub tests: EvidenceReference,
    pub benchmarks: EvidenceReference,
    pub consequences: EvidenceReference,
    pub removal_plan: EvidenceReference,
    pub authority_broadening: EvidenceReference,
    pub uncertainties: EvidenceReference,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairCandidate {
    pub candidate_id: String,
    pub candidate_version: u64,
    pub fingerprint: String,
    pub candidate_hash: String,
    pub candidate_build: EvidenceReference,
    pub evidence: CandidateEvidence,
    pub rollback_point: RollbackPoint,
    pub prepared_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrepareCandidateRequest {
    pub operation_id: String,
    pub expected_ledger_version: u64,
    pub expected_candidate_version: Option<u64>,
    pub candidate: RepairCandidate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDisclosure {
    pub disclosure_id: String,
    pub candidate_id: String,
    pub candidate_version: u64,
    pub management_checkpoint_id: String,
    pub disclosure_hash: String,
    pub disclosed_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectionRecord {
    pub rejection_id: String,
    pub candidate_id: String,
    pub candidate_version: u64,
    pub reason: String,
    pub rejected_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartBaton {
    pub baton_id: String,
    pub fingerprint: String,
    pub candidate_id: String,
    pub candidate_version: u64,
    pub candidate_hash: String,
    pub rollback_point_id: String,
    pub previous_working_build_hash: String,
    pub management_checkpoint_id: String,
    pub activation_decision_hash: String,
    pub activated_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivateCandidateRequest {
    pub operation_id: String,
    pub expected_ledger_version: u64,
    pub expected_candidate_version: u64,
    pub baton: RestartBaton,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationStart {
    pub verification_id: String,
    pub candidate_id: String,
    pub candidate_version: u64,
    pub started_build_hash: String,
    pub started_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRecord {
    pub verification_id: String,
    pub candidate_id: String,
    pub candidate_version: u64,
    pub started_build_hash: String,
    pub identity_matched: bool,
    pub outcome: VerificationOutcome,
    pub evidence: EvidenceReference,
    pub completed_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackRecord {
    pub rollback_id: String,
    pub candidate_id: String,
    pub candidate_version: u64,
    pub restored_build_hash: String,
    pub reason: String,
    pub evidence: EvidenceReference,
    pub manual_recovery_required: bool,
    pub completed_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceTombstone {
    pub tombstone_id: String,
    pub artifact_id: String,
    pub content_hash: String,
    pub availability: EvidenceAvailability,
    pub reason: String,
    pub recorded_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationEligibility {
    pub eligible: bool,
    pub reasons: Vec<String>,
}

/// Opaque trusted-core event append request. The local store validates only
/// bounds, redaction, idempotency, and compare-and-swap sequencing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventAppendRequest {
    pub operation_id: String,
    pub group_id: String,
    /// Zero creates the first event in a group; later appends must present the
    /// exact current sequence.
    pub expected_group_sequence: u64,
    pub event_fingerprint: String,
    pub occurred_at_epoch_ms: u64,
    pub event: serde_json::Value,
}

/// One event inside an atomic opaque core-event batch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventInput {
    pub event_fingerprint: String,
    pub occurred_at_epoch_ms: u64,
    pub event: serde_json::Value,
}

/// One idempotent operation containing one or more contiguous events for a
/// single group stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventAppendBatchRequest {
    pub operation_id: String,
    pub group_id: String,
    pub expected_group_sequence: u64,
    pub events: Vec<CoreEventInput>,
}

/// One immutable opaque event with independent group and global hash links.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCoreEvent {
    pub global_sequence: u64,
    pub group_id: String,
    pub group_sequence: u64,
    pub operation_id: String,
    pub event_fingerprint: String,
    pub occurred_at_epoch_ms: u64,
    pub canonical_event_json: String,
    pub event_content_hash: String,
    pub previous_group_event_hash: Option<String>,
    pub previous_global_event_hash: Option<String>,
    pub event_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventAppendReceipt {
    pub event: StoredCoreEvent,
    pub current_group_sequence: u64,
    pub current_global_version: u64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventAppendBatchReceipt {
    pub events: Vec<StoredCoreEvent>,
    pub current_group_sequence: u64,
    pub current_global_version: u64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventVersions {
    pub group_id: String,
    pub current_group_sequence: u64,
    pub current_global_version: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairTransition {
    pub sequence: u64,
    pub fingerprint: String,
    pub from: Option<ErrorGroupStatus>,
    pub to: ErrorGroupStatus,
    pub kind: String,
    pub occurred_at_epoch_ms: u64,
    pub previous_transition_hash: Option<String>,
    pub transition_hash: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepairIntegrityReport {
    pub groups_checked: u64,
    pub immutable_records_checked: u64,
    pub transitions_checked: u64,
    pub errors: Vec<String>,
}

impl RepairIntegrityReport {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairLedgerMode {
    ReadWrite,
    InspectableReadOnly { found_schema: u32 },
}

#[derive(Debug, Error)]
pub enum RepairLedgerError {
    #[error("repair-ledger filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("repair-ledger database failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("repair-ledger JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("repair-ledger identifier is invalid")]
    InvalidId,
    #[error("repair-ledger record is invalid or incomplete")]
    InvalidRecord,
    #[error("repair evidence contains forbidden secret material")]
    ForbiddenSecretMaterial,
    #[error("repair evidence hash is not canonical SHA-256")]
    InvalidHash,
    #[error("error group does not exist")]
    UnknownGroup,
    #[error("repair candidate does not exist")]
    UnknownCandidate,
    #[error("restart baton does not exist")]
    UnknownBaton,
    #[error("ledger version conflict: expected {expected}, found {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("candidate version conflict")]
    CandidateVersionConflict,
    #[error("core-event group sequence conflict: expected {expected}, found {actual}")]
    CoreEventVersionConflict { expected: u64, actual: u64 },
    #[error("operation identity was reused for a different request")]
    OperationConflict,
    #[error("immutable record identity was reused with different facts")]
    IdentityConflict,
    #[error("transition from {from:?} to {to:?} is not allowed")]
    InvalidTransition {
        from: ErrorGroupStatus,
        to: ErrorGroupStatus,
    },
    #[error("repair candidate is not eligible for activation: {0:?}")]
    Ineligible(Vec<String>),
    #[error("repair ledger integrity verification failed")]
    Integrity,
    #[error("repair ledger contains corrupt durable data")]
    Corrupt,
    #[error("repair ledger lock is unavailable after a previous panic")]
    Poisoned,
    #[error("repair ledger numeric value overflowed")]
    NumericOverflow,
    #[error("repair ledger page request is invalid")]
    InvalidPage,
    #[error("repair schema {found_schema} is newer and inspectable read-only")]
    ForwardSchema { found_schema: u32 },
}
