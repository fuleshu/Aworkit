//! Authenticated evidence that a candidate came from the bounded investigation Run.

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};

use super::evidence::{REPAIR_SCHEMA_VERSION_V1, RepairInvestigationBudgetV1};

/// Measured bounded resources sealed by the trusted Run boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepairInvestigationUsageV1 {
    pub attempts: u32,
    pub tool_calls: u32,
    pub tokens: u64,
    pub elapsed_ms: u64,
}

/// Immutable execution receipt emitted by the trusted Run/capability boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvestigationExecutionReceiptV1 {
    pub schema_version: u16,
    pub receipt_id: StableId,
    pub investigation_id: StableId,
    pub group_id: StableId,
    pub management_chat_id: StableId,
    pub management_run_id: StableId,
    pub candidate_id: StableId,
    pub candidate_version: u64,
    pub candidate_hash: String,
    pub authority_manifest_id: StableId,
    pub authority_manifest_hash: String,
    /// Exact authority subset frozen for the execution, in canonical order.
    pub frozen_capability_ids: Vec<StableId>,
    /// Exact capabilities actually executed, in canonical order.
    pub executed_capability_ids: Vec<StableId>,
    /// Exact user-approved ceiling dispatched to the Run.
    pub frozen_budget: RepairInvestigationBudgetV1,
    /// Actual resource use attested by the trusted Run boundary.
    pub observed_usage: RepairInvestigationUsageV1,
    pub completed_at_epoch_ms: u64,
    pub receipt_hash: String,
}

/// Same-user authenticated transport facts supplied by the Run boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvestigationExecutionPeerProofV1 {
    pub same_user_authenticated: bool,
    pub ownership_hash: String,
    pub channel_binding_hash: String,
}

/// Protected execution receipt returned through `RepairInvestigationPortV1`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticatedInvestigationExecutionReceiptV1 {
    pub receipt: InvestigationExecutionReceiptV1,
    pub peer: InvestigationExecutionPeerProofV1,
}

impl InvestigationExecutionReceiptV1 {
    #[must_use]
    pub fn has_supported_schema(&self) -> bool {
        self.schema_version == REPAIR_SCHEMA_VERSION_V1
    }
}
