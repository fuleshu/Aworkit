//! Stable Trusted Core ↔ Workflow Worker runtime contracts.
//!
//! Complex scheduler state is represented only by explicit Aworkit DTOs or
//! bounded schema-versioned JSON subdocuments. Worker-library implementation
//! types never cross this process boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ExtensionRuntimeBindingV1, HistoryBackendV1, ProcessGeneration, StableId};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerPortV1 {
    pub name: String,
    pub schema_ref: Option<String>,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerExecutorKindV1 {
    Pure,
    Brokered,
    Model,
    Agent,
    Subagent,
    Wait,
    Router,
    Branch,
    Join,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerNodeV1 {
    pub node_id: StableId,
    pub node_type: String,
    pub node_version: u32,
    pub contribution_hash: String,
    pub inputs: Vec<WorkerPortV1>,
    pub outputs: Vec<WorkerPortV1>,
    pub executor: WorkerExecutorKindV1,
    pub config: Value,
    pub capability_ref: Option<StableId>,
    pub result_schema_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerTransitionV1 {
    pub transition_id: StableId,
    pub from_node: StableId,
    pub from_port: String,
    pub to_node: StableId,
    pub to_port: String,
    pub priority: i32,
    pub predicate: Option<Value>,
    pub declared_loop_id: Option<StableId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerLoopDescriptorV1 {
    pub loop_id: StableId,
    pub maximum_iterations: u32,
    pub body_entry: StableId,
    pub body_exit: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerJoinDescriptorV1 {
    pub join_id: StableId,
    pub node_id: StableId,
    pub expected_branches: Vec<StableId>,
    pub merge_policy: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerRouteRuleV1 {
    pub route_id: StableId,
    pub node_id: StableId,
    pub priority: i32,
    pub predicate: Value,
    pub destination_transition: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerBudgetV1 {
    pub turns: u64,
    pub attempts: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub cost_micros: u64,
    pub actions: u64,
    pub depth: u32,
    pub fanout: u32,
    pub parallel: u32,
    pub deadline_ms: u64,
}

/// Exact execution identity copied from the trusted core authority manifest
/// into an immutable worker snapshot. The worker never resolves or substitutes
/// these values; it only references the logical `capability_id` in proposals.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenCapabilityBindingV1 {
    pub capability_id: StableId,
    pub adapter_id: StableId,
    pub adapter_version: String,
    pub descriptor_hash: String,
    pub extension: Option<ExtensionRuntimeBindingV1>,
    pub required_isolation_profile: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerFrozenRunSnapshotV1 {
    pub snapshot_id: StableId,
    /// SHA-256 over RFC 8785/JCS canonical JSON after deterministically sorting
    /// all semantic vectors and encoding this field as the empty string.
    pub snapshot_hash: String,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub schema_version: u16,
    pub compiler_version: String,
    pub workflow_hash: String,
    pub nodes: Vec<WorkerNodeV1>,
    pub transitions: Vec<WorkerTransitionV1>,
    pub entry_nodes: Vec<StableId>,
    pub loop_descriptors: Vec<WorkerLoopDescriptorV1>,
    pub join_descriptors: Vec<WorkerJoinDescriptorV1>,
    pub route_rules: Vec<WorkerRouteRuleV1>,
    pub authority_manifest_ref: StableId,
    pub authority_manifest_hash: String,
    /// Complete immutable bindings retained for provenance and recovery.
    pub capability_bindings: Vec<FrozenCapabilityBindingV1>,
    /// Sorted mirror used by the worker's compact capability lookup. It must
    /// exactly equal the IDs in `capability_bindings`.
    pub capability_refs: Vec<StableId>,
    pub workspace_identity: Value,
    pub budget: WorkerBudgetV1,
    pub history_mode: HistoryBackendV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcomeV1 {
    Approved,
    Denied,
    Expired,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityOutcomeClassV1 {
    Success,
    DefiniteNotStarted,
    FailedKnownStarted,
    CancelledEvidence,
    Uncertain,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityOutcomeV1 {
    pub outcome_id: StableId,
    pub invocation_id: StableId,
    pub class: CapabilityOutcomeClassV1,
    pub retry_safe_proof: bool,
    pub payload: Value,
    pub usage: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerCheckpointV1 {
    pub checkpoint_id: StableId,
    pub snapshot_hash: String,
    pub plan_hash: String,
    pub checkpoint_hash: String,
    pub prior_generation: ProcessGeneration,
    pub committed_cursor: u64,
    pub proposal_sequence: u64,
    pub token_frontier: Value,
    pub context_heads: Value,
    pub context_revision_dag: Value,
    pub branch_frames: Value,
    pub loop_frames: Value,
    pub budget_state: Value,
    pub attempt_state: Value,
    pub no_resend_state: Value,
    pub suspension_state: Value,
    pub child_frames: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RehydrationEnvelopeV1 {
    pub snapshot: WorkerFrozenRunSnapshotV1,
    pub checkpoint: WorkerCheckpointV1,
    pub replacement_generation: ProcessGeneration,
    pub committed_deltas: Vec<Value>,
    pub reconciled_outcomes: Vec<CapabilityOutcomeV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum WorkerControlKindV1 {
    Start(WorkerFrozenRunSnapshotV1),
    Restore(RehydrationEnvelopeV1),
    Input {
        input_id: StableId,
        payload: Value,
    },
    Approval {
        approval_id: StableId,
        outcome: ApprovalOutcomeV1,
    },
    Pause {
        control_id: StableId,
        scope: String,
    },
    Resume {
        control_id: StableId,
        scope: String,
    },
    Cancel {
        control_id: StableId,
        scope: String,
    },
    CapabilityOutcome(CapabilityOutcomeV1),
    CommittedAck {
        proposal_id: StableId,
        committed_cursor: u64,
    },
    Shutdown {
        control_id: StableId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerControlEnvelopeV1 {
    pub message_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub generation: ProcessGeneration,
    pub snapshot_hash: String,
    pub committed_cursor: u64,
    pub control: WorkerControlKindV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerInvocationProposalV1 {
    pub invocation_id: StableId,
    pub node_id: StableId,
    pub attempt_id: StableId,
    pub capability_ref: StableId,
    pub authority_manifest_ref: StableId,
    pub budget_ref: StableId,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum WorkerProposalKindV1 {
    Ready {
        plan_fingerprint: String,
    },
    Transition {
        transition_id: StableId,
        facts: Value,
    },
    Invocation(WorkerInvocationProposalV1),
    Suspension {
        suspension_id: StableId,
        state: Value,
    },
    Checkpoint(WorkerCheckpointV1),
    Terminal {
        outcome: String,
        facts: Value,
    },
    Health {
        facts: Value,
    },
    RehydrationReady {
        checkpoint_hash: String,
    },
    RehydrationBlocked {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerProposalEnvelopeV1 {
    pub proposal_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub generation: ProcessGeneration,
    pub snapshot_hash: String,
    pub worker_sequence: u64,
    pub base_committed_cursor: u64,
    pub proposal: WorkerProposalKindV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerHandshakeV1 {
    pub protocol_version: u16,
    pub worker_version: String,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub generation: ProcessGeneration,
    pub snapshot_hash: String,
    pub plan_fingerprint: String,
    pub executable_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerHeartbeatV1 {
    pub sequence: u64,
    pub monotonic_time_ms: u64,
    pub active: bool,
    pub quiescent: bool,
}

/// Every framed message emitted by the worker executable. The tagged family
/// keeps startup identity validation separate from runtime proposals while
/// still using one bounded stdout decoder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum WorkerOutputKindV1 {
    Handshake(WorkerHandshakeV1),
    Proposal(WorkerProposalEnvelopeV1),
    Heartbeat(WorkerHeartbeatV1),
    Error { code: String, message: String },
    ShutdownAck { control_id: StableId },
}

/// Process-generation-fenced wrapper for one worker stdout frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerOutputEnvelopeV1 {
    pub message_id: StableId,
    pub generation: ProcessGeneration,
    pub output: WorkerOutputKindV1,
}
