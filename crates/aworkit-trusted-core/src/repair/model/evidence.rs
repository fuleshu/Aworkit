//! Candidate evidence, disclosure, and bounded-investigation records.

use aworkit_protocol::StableId;
pub use aworkit_protocol::{
    BuildBundleRefV1, BuildProvenanceV1, FocusedVerificationCheckResultV1,
    FocusedVerificationCheckV1, FocusedVerificationEvidenceV1, FocusedVerificationPlanV1,
    REPAIR_SCHEMA_VERSION_V1, RepairArtifactRefV1,
};
use serde::{Deserialize, Serialize};

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
