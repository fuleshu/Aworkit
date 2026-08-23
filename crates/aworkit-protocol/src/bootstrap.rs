//! Stable Trusted Core ↔ bootstrap-helper repair activation contracts.
//!
//! These DTOs and hashes cross an isolated process boundary. They live here so
//! neither process implementation depends on the other.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ProcessGeneration, StableId};

/// Schema version shared by all repair/bootstrap records.
pub const REPAIR_SCHEMA_VERSION_V1: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairArtifactRefV1 {
    pub artifact_id: StableId,
    pub content_hash: String,
    pub byte_size: u64,
    pub media_type: String,
    pub logical_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildBundleRefV1 {
    pub artifact: RepairArtifactRefV1,
    pub manifest_relative_entry: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationCheckV1 {
    pub check_id: StableId,
    pub label: String,
    pub capability_id: StableId,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationPlanV1 {
    pub plan_id: StableId,
    pub checks: Vec<FocusedVerificationCheckV1>,
    pub plan_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationCheckResultV1 {
    pub check_id: StableId,
    pub passed: bool,
    pub summary: String,
    pub evidence: Vec<RepairArtifactRefV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationEvidenceV1 {
    pub plan_id: StableId,
    pub plan_hash: String,
    pub results: Vec<FocusedVerificationCheckResultV1>,
    pub evidence_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildProvenanceV1 {
    pub source_revision: String,
    pub source_tree_hash: String,
    pub workspace_identity_hash: String,
    pub toolchain_hash: String,
    pub build_manifest_hash: String,
    pub provenance_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildOriginV1 {
    ManagedLocal {
        enrollment_digest: String,
        active_slot_hash: String,
    },
    SourceCheckout {
        projected_provenance_hash: String,
    },
    PackagedDistribution {
        owner: String,
    },
    Unknown,
    Conflicting {
        detail: String,
    },
    Mismatched {
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationEligibilityV1 {
    SupportedManagedLocal,
    EnrollmentRequired,
    PackagedDistribution,
    UnknownOrigin,
    ConflictingOrigin,
    MismatchedEnrollment,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformReasonV1 {
    pub code: String,
    pub message: String,
    pub next_steps: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformCapabilityReportV1 {
    pub schema_version: u16,
    pub report_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub candidate_hash: String,
    pub capability_generation: u64,
    pub build_origin: BuildOriginV1,
    pub eligibility: ActivationEligibilityV1,
    pub reason: PlatformReasonV1,
    pub current_build: BuildBundleRefV1,
    pub previous_working_build: Option<BuildBundleRefV1>,
    pub valid_from_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    pub capability_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedLocalEnrollmentRequestV1 {
    pub request_id: StableId,
    pub explicit_user_decision_id: StableId,
    pub group_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub candidate_hash: String,
    pub projected_provenance_hash: String,
    pub whole_bundle: BuildBundleRefV1,
    pub capability_report_id: StableId,
    pub capability_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentPreparedV1 {
    pub preparation_id: StableId,
    pub request_id: StableId,
    pub enrollment_digest: String,
    pub stable_launcher: String,
    pub restart_instructions: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementCheckpointRefV1 {
    pub checkpoint_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub committed_sequence: u64,
    pub snapshot_hash: String,
    pub checkpoint_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairCandidateDispositionV1 {
    Rejected,
    Deferred,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairCandidateDecisionV1 {
    pub decision_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub disposition: RepairCandidateDispositionV1,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapDeadlinesV1 {
    pub admission_ms: u64,
    pub cleanup_ms: u64,
    pub startup_ms: u64,
    pub focused_verification_ms: u64,
    pub rollback_ms: u64,
    pub result_read_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairActivationDecisionV1 {
    pub activation_id: StableId,
    pub explicit_user_decision_id: StableId,
    pub candidate_id: StableId,
    pub expected_candidate_version: u64,
    pub expected_candidate_hash: String,
    pub expected_capability_report_id: StableId,
    pub expected_capability_digest: String,
    pub decided_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairActivationBatonV1 {
    pub schema_version: u16,
    pub baton_id: StableId,
    pub activation_id: StableId,
    pub group_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub candidate_hash: String,
    pub candidate_bundle: BuildBundleRefV1,
    pub disclosure_hash: String,
    pub provenance_hash: String,
    pub enrollment_digest: String,
    pub capability_report_id: StableId,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub previous_working_build: BuildBundleRefV1,
    pub management_checkpoint: ManagementCheckpointRefV1,
    pub verification_plan: FocusedVerificationPlanV1,
    pub current_process_generation: ProcessGeneration,
    pub candidate_process_generation: ProcessGeneration,
    pub rollback_process_generation: ProcessGeneration,
    pub deadlines: BootstrapDeadlinesV1,
    pub expires_at_epoch_ms: u64,
    pub baton_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapAcceptedAdmissionV1 {
    pub admission_id: StableId,
    pub activation_id: StableId,
    pub baton_hash: String,
    pub candidate_process_generation: ProcessGeneration,
    pub rollback_process_generation: ProcessGeneration,
    pub admission_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum BootstrapAdmissionV1 {
    Accepted(BootstrapAcceptedAdmissionV1),
    Unsupported(AuthenticatedBootstrapResultV1),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoreQuiescenceFactsV1 {
    pub quiescence_id: StableId,
    pub activation_id: StableId,
    pub process_generation: ProcessGeneration,
    pub worker_trees_stopped: u32,
    pub host_trees_stopped: u32,
    pub sidecar_trees_stopped: u32,
    pub timed_out: bool,
    pub orphan_risk: bool,
    pub facts_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BootstrapResultKindV1 {
    Unsupported {
        reason: PlatformReasonV1,
    },
    ActivatedVerified {
        focused_verification: FocusedVerificationEvidenceV1,
    },
    RolledBack {
        reason: String,
        rollback_evidence: Vec<RepairArtifactRefV1>,
    },
    ManualRecoveryRequired {
        diagnostic_id: StableId,
        observed_slot_state: String,
        instructions: Vec<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapResultV1 {
    pub schema_version: u16,
    pub receipt_id: StableId,
    pub activation_id: StableId,
    pub baton_hash: String,
    pub management_checkpoint_id: StableId,
    pub recipient_process_generation: ProcessGeneration,
    pub sealed_at_epoch_ms: u64,
    pub result: BootstrapResultKindV1,
    pub receipt_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapPeerProofV1 {
    pub same_user_authenticated: bool,
    pub recipient_process_generation: ProcessGeneration,
    pub ownership_hash: String,
    pub channel_binding_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticatedBootstrapResultV1 {
    pub receipt: BootstrapResultV1,
    pub peer: BootstrapPeerProofV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCodeV1 {
    UnsupportedPlatform,
    UnsupportedVolume,
    UnsupportedSelector,
    UnsupportedProcessCleanup,
    DataMigrationRequired,
    OriginUnverifiable,
    EnrollmentMismatch,
    CapabilityDrift,
    CandidateFailure,
    RollbackFailure,
    TornJournal,
    ChainBroken,
    GenerationProofMissing,
    AmbiguousSelectorState,
    DiskFull,
    SyncFailure,
    OwnershipLost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStrengthV1 {
    SameUserHashAndOwnership,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualRecoveryNoticeV1 {
    pub notice_id: StableId,
    pub activation_id: StableId,
    pub reason: ReasonCodeV1,
    pub observed_slot_state_hash: String,
    pub diagnostic_id: StableId,
    pub instructions: Vec<String>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BootstrapContractError {
    #[error("bootstrap contract could not be encoded canonically")]
    Encoding,
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, BootstrapContractError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| BootstrapContractError::Encoding)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

pub fn focused_verification_plan_hash_v1(
    plan: &FocusedVerificationPlanV1,
) -> Result<String, BootstrapContractError> {
    canonical_hash(&(&plan.plan_id, &plan.checks))
}

pub fn focused_verification_evidence_hash_v1(
    evidence: &FocusedVerificationEvidenceV1,
) -> Result<String, BootstrapContractError> {
    canonical_hash(&(&evidence.plan_id, &evidence.plan_hash, &evidence.results))
}

pub fn bootstrap_result_hash_v1(
    result: &BootstrapResultV1,
) -> Result<String, BootstrapContractError> {
    canonical_hash(&(
        result.schema_version,
        &result.receipt_id,
        &result.activation_id,
        &result.baton_hash,
        &result.management_checkpoint_id,
        result.recipient_process_generation,
        result.sealed_at_epoch_ms,
        &result.result,
    ))
}
