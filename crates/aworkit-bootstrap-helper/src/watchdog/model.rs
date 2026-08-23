//! Exact process-generation, handshake, health, cleanup, and verification DTOs.

use aworkit_protocol::{CapabilityOutcomeV1, ProcessGeneration, StableId};
use aworkit_trusted_core::{
    BootstrapDeadlinesV1, FocusedVerificationEvidenceV1, FocusedVerificationPlanV1,
    ManualRecoveryNoticeV1,
};
use serde::{Deserialize, Serialize};

use crate::profile::ActiveSelectorObservationV1;
use crate::slots::{OpenBuildSlotHandleV1, VerifiedBuildSlotV1};

/// Candidate and rollback launches have distinct policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationRoleV1 {
    Candidate,
    Previous,
}

/// Bootstrap launch mode is fixed; normal runtime mode is not representable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapLaunchModeV1 {
    VerificationOnly,
}

/// Opaque process-tree identity returned by the native process port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessTreeHandleV1 {
    pub handle_id: StableId,
    pub process_generation: ProcessGeneration,
    pub root_process_identity_hash: String,
    pub containment_identity_hash: String,
}

/// Proof that one exact process generation is empty.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessTreeCleanupV1 {
    pub process_generation: ProcessGeneration,
    pub cooperative_requested: bool,
    pub forced_termination_used: bool,
    pub descendants_observed: u32,
    pub tree_empty: bool,
    pub orphan_risk: bool,
    pub proof_hash: String,
}

/// Preconditions already proven by journal/selector/coordinator before spawn.
#[derive(Clone, Debug)]
pub struct GenerationLaunchSpecV1 {
    pub activation_id: StableId,
    pub attempt_id: StableId,
    pub role: GenerationRoleV1,
    pub installation_id: StableId,
    pub enrollment_digest: String,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub verification_plan_hash: String,
    pub verification_plan: FocusedVerificationPlanV1,
    pub process_generation: ProcessGeneration,
    pub expected_prior_process_generation: ProcessGeneration,
    pub slot: VerifiedBuildSlotV1,
    pub selector: ActiveSelectorObservationV1,
    pub prior_cleanup: ProcessTreeCleanupV1,
    pub helper_detached_and_surviving: bool,
    pub deadlines: BootstrapDeadlinesV1,
}

/// Minimal fixed spawn request emitted only after all preconditions pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformLaunchRequestV1 {
    pub activation_id: StableId,
    pub attempt_id: StableId,
    pub installation_id: StableId,
    pub enrollment_digest: String,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub verification_plan_hash: String,
    pub verification_plan_id: StableId,
    pub verification_check_ids: Vec<StableId>,
    pub helper_protocol_version: u16,
    pub role: GenerationRoleV1,
    pub mode: BootstrapLaunchModeV1,
    pub process_generation: ProcessGeneration,
    pub slot_handle: OpenBuildSlotHandleV1,
    pub exact_core_entry: String,
    pub launch_nonce_hash: String,
    pub sanitized_environment: bool,
    pub inherited_handles_closed: bool,
}

/// Exact process observation immediately after spawn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchObservationV1 {
    pub attempt_id: StableId,
    pub process_tree: ProcessTreeHandleV1,
    pub executable_hash: String,
    pub slot_root_identity_hash: String,
    pub observed_at_monotonic_ms: u64,
    pub observation_hash: String,
}

/// Authenticated generation identity handshake. PID is deliberately absent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenerationHandshakeV1 {
    pub activation_id: StableId,
    pub attempt_id: StableId,
    pub installation_id: StableId,
    pub enrollment_digest: String,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub launch_nonce_hash: String,
    pub executable_hash: String,
    pub slot_root_identity_hash: String,
    pub helper_protocol_version: u16,
    pub verification_plan_hash: String,
    pub mode: BootstrapLaunchModeV1,
    pub process_generation: ProcessGeneration,
    pub handshake_hash: String,
}

/// Fresh health fact; readiness/handshake alone is not health.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenerationHealthV1 {
    pub attempt_id: StableId,
    pub process_generation: ProcessGeneration,
    pub healthy: bool,
    pub heartbeat_sequence: u64,
    pub observation_hash: String,
}

/// Plan- and generation-bound focused verification result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FocusedVerificationResultV1 {
    pub activation_id: StableId,
    pub attempt_id: StableId,
    pub process_generation: ProcessGeneration,
    pub verification_plan_hash: String,
    pub passed: bool,
    pub outcome: CapabilityOutcomeV1,
    /// Candidate-produced, plan-bound evidence sealed into a verified receipt.
    pub focused_verification: FocusedVerificationEvidenceV1,
    pub result_hash: String,
}

/// Candidate fully verified or previous generation healthy after rollback.
#[derive(Clone, Debug, PartialEq)]
pub enum GenerationWatchdogSuccessV1 {
    CandidateVerified {
        launch: LaunchObservationV1,
        handshake: GenerationHandshakeV1,
        health: GenerationHealthV1,
        verification: FocusedVerificationResultV1,
    },
    PreviousHealthy {
        launch: LaunchObservationV1,
        handshake: GenerationHandshakeV1,
        health: GenerationHealthV1,
    },
}

/// Closed failure stage used by rollback-biased coordination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogFailureStageV1 {
    Preconditions,
    Cleanup,
    Spawn,
    Startup,
    Identity,
    Health,
    FocusedVerification,
}

/// Bounded failure with no raw logs or process snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchdogFailureV1 {
    pub activation_id: StableId,
    pub attempt_id: StableId,
    pub role: GenerationRoleV1,
    pub stage: WatchdogFailureStageV1,
    pub reason_code: String,
    pub diagnostic_id: StableId,
    pub rollback_required: bool,
}

/// Result of a stable-launcher notice read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableLauncherNoticeV1 {
    pub notice: ManualRecoveryNoticeV1,
    pub copy_diagnostic_id_allowed: bool,
    pub open_recovery_instructions_allowed: bool,
    pub exits_after_notice: bool,
}
