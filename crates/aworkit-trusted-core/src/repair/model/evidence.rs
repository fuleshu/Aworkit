//! Candidate evidence, disclosure, and bounded-investigation records.

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};

/// Schema version shared by all M10 repair/bootstrap records.
pub const REPAIR_SCHEMA_VERSION_V1: u16 = 1;

/// Immutable local evidence referenced by ID and exact content hash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairArtifactRefV1 {
    pub artifact_id: StableId,
    pub content_hash: String,
    pub byte_size: u64,
    pub media_type: String,
    pub logical_name: String,
}

/// A complete application bundle staged in the artifact store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildBundleRefV1 {
    pub artifact: RepairArtifactRefV1,
    pub manifest_relative_entry: String,
}

/// Explicit evidence or an explicit explanation that evidence is absent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepairEvidenceDisclosureV1 {
    Evidence {
        summary: String,
        artifacts: Vec<RepairArtifactRefV1>,
    },
    NoneDeclared {
        explanation: String,
    },
    NotPerformed {
        explanation: String,
    },
}

/// One independently addressable user-visible disclosure fact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisclosureItemV1 {
    pub item_id: StableId,
    pub label: String,
    pub detail: String,
}

/// A disclosure list distinguishes "none" from an accidentally omitted list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisclosureItemsV1 {
    pub items: Vec<DisclosureItemV1>,
    pub none_declared: bool,
}

/// Whether candidate data changes preserve a usable rollback path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataCompatibilityV1 {
    RollbackCompatible,
    DeferredUntilVerified { explanation: String },
    ForwardOnlyMigrationRequired { explanation: String },
}

/// One exact check in the focused startup-verification plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationCheckV1 {
    pub check_id: StableId,
    pub label: String,
    /// An approved capability reference, never an executable shell string.
    pub capability_id: StableId,
    pub timeout_ms: u64,
}

/// Plan sealed into the activation baton before the current core quiesces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationPlanV1 {
    pub plan_id: StableId,
    pub checks: Vec<FocusedVerificationCheckV1>,
    pub plan_hash: String,
}

/// Result for one plan-bound verification check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationCheckResultV1 {
    pub check_id: StableId,
    pub passed: bool,
    pub summary: String,
    pub evidence: Vec<RepairArtifactRefV1>,
}

/// Exact focused-verification evidence submitted by the candidate generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocusedVerificationEvidenceV1 {
    pub plan_id: StableId,
    pub plan_hash: String,
    pub results: Vec<FocusedVerificationCheckResultV1>,
    pub evidence_hash: String,
}

/// Reproducible, secret-free provenance for the whole candidate build.
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

/// Complete user-visible disclosure required before activation is possible.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairDisclosureV1 {
    pub source_diff: RepairEvidenceDisclosureV1,
    pub configuration_diff: RepairEvidenceDisclosureV1,
    pub tests: RepairEvidenceDisclosureV1,
    pub benchmarks: RepairEvidenceDisclosureV1,
    pub consequences: DisclosureItemsV1,
    pub removed_behaviors: DisclosureItemsV1,
    pub disabled_behaviors: DisclosureItemsV1,
    pub broadened_behaviors: DisclosureItemsV1,
    pub replaced_behaviors: DisclosureItemsV1,
    pub uncertainties: DisclosureItemsV1,
    pub data_compatibility: DataCompatibilityV1,
    pub rollback_point: BuildBundleRefV1,
    pub verification_plan: FocusedVerificationPlanV1,
    pub disclosure_hash: String,
}

/// Immutable candidate version produced through approved Run capabilities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairCandidateV1 {
    pub candidate_id: StableId,
    pub group_id: StableId,
    pub candidate_version: u64,
    pub summary: String,
    pub build_bundle: BuildBundleRefV1,
    pub provenance: BuildProvenanceV1,
    pub built_under_authority_manifest_hash: String,
    pub disclosure: RepairDisclosureV1,
    pub candidate_hash: String,
}

/// One immutable recurring-error occurrence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorOccurrenceV1 {
    pub occurrence_id: StableId,
    /// A normalized SHA-256 fingerprint; raw diagnostic text is not a key.
    pub fingerprint: String,
    pub summary: String,
    pub semantic_event_id: StableId,
    pub attempt_id: Option<StableId>,
    pub diagnostic_record_id: Option<StableId>,
    pub evidence: Vec<RepairArtifactRefV1>,
    pub observed_at_epoch_ms: u64,
}

/// Bounded investigation resources frozen before dispatch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairInvestigationBudgetV1 {
    pub max_attempts: u32,
    pub max_tool_calls: u32,
    pub max_tokens: u64,
    pub deadline_ms: u64,
}

/// Exact subset of the existing Run authority available to an investigation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenRepairAuthorityV1 {
    pub authority_manifest_id: StableId,
    pub authority_manifest_hash: String,
    pub capability_ids: Vec<StableId>,
}

/// Persisted investigation request; merely recording an error never creates it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairInvestigationV1 {
    pub investigation_id: StableId,
    pub explicit_user_decision_id: StableId,
    pub group_id: StableId,
    pub management_chat_id: StableId,
    pub management_run_id: StableId,
    pub authority: FrozenRepairAuthorityV1,
    pub budget: RepairInvestigationBudgetV1,
}
