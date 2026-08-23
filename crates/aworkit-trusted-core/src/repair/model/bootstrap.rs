//! Managed-local eligibility, activation baton, and helper receipt records.

use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Deserialize, Serialize};

use super::evidence::{
    BuildBundleRefV1, FocusedVerificationEvidenceV1, FocusedVerificationPlanV1, RepairArtifactRefV1,
};

/// Closed build-origin classification reported by the bootstrap helper.
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

/// Closed activation result shown to callers without optimistic inference.
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

/// Exact unavailable reason and next step supplied by the helper.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformReasonV1 {
    pub code: String,
    pub message: String,
    pub next_steps: Vec<String>,
}

/// Fresh, generation-bound helper report used to decide whether to show activation.
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

/// Explicit user request to prepare a managed-local installation root.
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

/// Durable helper response. Preparation never means activation or auto-resume.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentPreparedV1 {
    pub preparation_id: StableId,
    pub request_id: StableId,
    pub enrollment_digest: String,
    pub stable_launcher: String,
    pub restart_instructions: Vec<String>,
}

/// Durable same-Chat checkpoint created before any activation handoff.
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

/// Explicit rejection or deferral; neither path contacts the helper.
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

/// Independently bounded helper phases sealed into the baton.
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

/// Exact activation decision persisted with its checkpoint and baton.
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

/// Secret-free, tamper-evident handoff record consumed once by the helper.
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

/// Helper admission accepted while the current core is still alive.
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

/// Admission either accepts the baton or returns a protected Unsupported receipt.
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

/// Current-generation cleanup facts; the helper still owns selector/launch mechanics.
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

/// Outcome sealed by the helper. Unknown or ambiguous success has no variant.
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

/// Immutable helper receipt targeted to one exact next core generation.
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

/// OS-authenticated channel proof supplied by the bootstrap transport adapter.
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

/// Closed bootstrap reason code carried by receipts and manual-recovery notices.
///
/// Codes are deterministic and never derived from timestamps, so the same
/// observed condition always yields the same code across helper restarts.
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

/// Same-user integrity strength.
///
/// V1 deliberately makes no publisher-authentication claim: the strength is a
/// SHA-256 content identity plus per-user ownership verification, which is the
/// strongest guarantee available to a helper running as the desktop user.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStrengthV1 {
    SameUserHashAndOwnership,
}

/// Durable notice emitted when no safe automatic terminal can be reached.
///
/// The notice is stored alongside (never instead of) the journal chain and is
/// the only surface that instructs a user to recover manually.
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
