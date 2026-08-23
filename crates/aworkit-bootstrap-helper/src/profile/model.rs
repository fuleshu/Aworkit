//! Runtime observations, activation plans, and selector mutation receipts.

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    BootstrapDeadlinesV1, BuildBundleRefV1, BuildOriginV1, PlatformReasonV1,
};
use serde::{Deserialize, Serialize};

use crate::journal::BootstrapPhaseV1;
use crate::protocol::LocalBuildEnrollmentStateV1;
use crate::slots::{SlotDataCompatibilityV1, VerifiedBuildSlotV1};

/// Fresh OS and managed-root facts. They may only downgrade eligibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileRuntimeObservationsV1 {
    pub detected_origin: BuildOriginV1,
    pub embedded_provenance_digest: String,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub candidate_build_content_hash: String,
    pub current_build: BuildBundleRefV1,
    pub active_selector_hash: String,
    pub installation_identity_matches: bool,
    pub helper_identity_matches: bool,
    pub launcher_identity_matches: bool,
    pub journal_identity_matches: bool,
    pub selector_identity_matches: bool,
    pub candidate_slot_verified: bool,
    pub previous_slot_verified: bool,
    pub per_user_owned: bool,
    pub writable_without_elevation: bool,
    pub same_local_durable_volume: bool,
    pub atomic_selector_supported: bool,
    pub helper_survives_outside_slots: bool,
    pub complete_process_tree_cleanup: bool,
    pub verification_only_launch: bool,
    pub data_compatibility: SlotDataCompatibilityV1,
    pub capability_generation: u64,
    pub valid_from_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
}

/// Fixed helper-controlled enrollment layout identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedLocalLayoutV1 {
    pub installation_id: StableId,
    pub helper_root_identity_hash: String,
    pub helper_identity_hash: String,
    pub launcher_identity_hash: String,
    pub initial_active_slot_root_hash: String,
    pub selector_identity_hash: String,
    pub journal_identity_hash: String,
}

/// Selector observation through an anchored native identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveSelectorObservationV1 {
    pub selector_identity_hash: String,
    pub selected_build_content_hash: String,
    pub selected_root_identity_hash: String,
    pub capability_generation: u64,
    pub observation_hash: String,
}

/// Exact immutable inputs for a candidate switch and possible restore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationPlanV1 {
    pub activation_id: StableId,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub selector_identity_hash: String,
    pub current: VerifiedBuildSlotV1,
    pub candidate: VerifiedBuildSlotV1,
    pub previous: VerifiedBuildSlotV1,
    pub current_process_generation: ProcessGeneration,
    pub candidate_process_generation: ProcessGeneration,
    pub rollback_process_generation: ProcessGeneration,
    pub deadlines: BootstrapDeadlinesV1,
}

/// Closed selector action. There is no force or arbitrary target variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorMutationKindV1 {
    SelectCandidate,
    RestorePrevious,
}

/// Journal-fenced selector mutation passed to the native atomic port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectorMutationV1 {
    pub mutation_id: StableId,
    pub activation_id: StableId,
    pub kind: SelectorMutationKindV1,
    pub expected_phase: BootstrapPhaseV1,
    pub capability_generation: u64,
    pub process_generation: ProcessGeneration,
    pub selector_identity_hash: String,
    pub expected_source_hash: String,
    pub expected_source_root_identity_hash: String,
    pub destination_hash: String,
    pub destination_root_identity_hash: String,
    pub mutation_hash: String,
}

/// Exact before/after selector proof returned after reopen and verification.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectorMutationReceiptV1 {
    pub mutation_id: StableId,
    pub activation_id: StableId,
    pub kind: SelectorMutationKindV1,
    pub before: ActiveSelectorObservationV1,
    pub after: ActiveSelectorObservationV1,
    pub mutation_hash: String,
    pub receipt_hash: String,
}

/// Native port result distinguishes definite non-application from ambiguity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSelectorMutationOutcomeV1 {
    Applied(ActiveSelectorObservationV1),
    DefinitelyNotApplied(PlatformReasonV1),
    Ambiguous,
}

/// Stable classification result used internally before constructing a report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProfileDecisionV1 {
    pub origin: BuildOriginV1,
    pub enrollment: LocalBuildEnrollmentStateV1,
    pub eligibility: aworkit_trusted_core::ActivationEligibilityV1,
    pub reason: PlatformReasonV1,
}
