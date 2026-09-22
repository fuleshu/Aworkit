//! Durable child-context frames: the owned runtime state of one delegated
//! child conversation, its lineage and its integration status.
//!
//! Frames are immutable keyed snapshots in the same machine-local record store
//! that owns tool invocations, job identity and the Run task list. Later
//! revisions of one child supersede earlier ones, so a Run resumes a child from
//! its newest committed head without replaying any settled child effect.
use super::*;

/// Record kind for one child-context frame revision.
const CHILD_RECORD: &str = "pipeline.subagent-child";

/// How a child conversation started.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildKindV1 {
    /// Fresh instructions only; the child never sees parent history.
    Fresh,
    /// A declared bounded projection of the parent conversation was inherited.
    Fork,
    /// One unattended one-shot delegation to a configured external agent. It has
    /// identity, lineage and a terminal answer, but no continuation or steering.
    External,
}

/// Terminal and resumable states of one child conversation scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildStatusV1 {
    /// The child is running as a background job right now.
    Running,
    /// The child settled normally and can be messaged again.
    Completed,
    /// The child returned approval-dependent actions to its parent.
    ParentApprovalRequired,
    /// The child loop failed; the parent owns the frozen attempt policy.
    Failed,
    /// Aworkit restarted while the child was running; it was not replayed.
    Interrupted,
    /// The child scope was cancelled and cannot be continued.
    Cancelled,
}

impl ChildStatusV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::ParentApprovalRequired => "parent_approval_required",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One immutable child-context frame revision.
///
/// The frame is the design's `ChildContextFrameV1`: child identity, parent
/// provenance, the declared inherited projection, the child's own conversation
/// head and the counters the parent needs to integrate or supervise it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubagentChildFrameV1 {
    pub child_id: String,
    pub chat_id: String,
    pub run_id: String,
    /// Delegating Agent node; the owner-isolation key together with `chat_id`.
    pub node_id: String,
    /// Invocation that first spawned this child.
    pub parent_invocation_id: String,
    /// The delegating tool call this child was spawned by, so the parent
    /// timeline block can open exactly this child's tab.
    #[serde(default)]
    pub parent_call_id: String,
    /// Enclosing child when a delegation chain exists; always absent while
    /// nested delegation stays unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_child_id: Option<String>,
    pub depth: u32,
    pub kind: ChildKindV1,
    /// Deterministic hash of the declared inherited prefix (fork only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_hash: Option<String>,
    #[serde(default)]
    pub projection_items: usize,
    #[serde(default)]
    pub projection_dropped: usize,
    pub inherited_tool_ids: Vec<String>,
    pub read_only: bool,
    /// Monotonic revision of this child's own conversation.
    pub head_revision: u64,
    pub status: ChildStatusV1,
    pub task: String,
    pub context_text: String,
    /// Exact base input of the child conversation; the immutable prompt prefix
    /// every resumed turn reuses.
    pub input: Value,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub final_text: String,
    #[serde(default)]
    pub blocked_actions: Vec<ModelToolCallV1>,
    pub model_turns: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub created_at: String,
    pub updated_at: String,
}

impl SubagentChildFrameV1 {
    /// Compact listing projection; never returns the child's whole transcript.
    pub(crate) fn listing(&self) -> Value {
        json!({
            "childId": self.child_id,
            "kind": self.kind,
            "status": self.status.as_str(),
            "depth": self.depth,
            "headRevision": self.head_revision,
            "task": self.task,
            "finalText": self.final_text,
            "modelTurns": self.model_turns,
            "toolCalls": self.tool_calls,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }

    /// A model-facing child outcome, shared by spawn, fork and continuation.
    pub(crate) fn outcome(&self, jobs: Value) -> Value {
        json!({
            "childId": self.child_id,
            "kind": self.kind,
            "status": self.status.as_str(),
            "headRevision": self.head_revision,
            "blockedActions": self.blocked_actions,
            "jobs": jobs,
            "finalText": self.final_text,
            "modelTurns": self.model_turns,
            "toolCalls": self.tool_calls,
            "inputTokens": self.input_tokens,
            "outputTokens": self.output_tokens,
        })
    }
}

impl ToolRecordStore {
    /// Appends one child-context frame revision. The key is the child identity
    /// plus its revision, so a replayed pass re-appends at most the same
    /// revision and never rewrites an acknowledged child head.
    pub(crate) fn record_subagent_child(
        &self,
        frame: &SubagentChildFrameV1,
    ) -> Result<(), WorkflowPipelineError> {
        let key = digest_id(
            "record.subagent-child",
            &format!("{}:{}", frame.child_id, frame.head_revision),
        )?;
        self.append(
            CHILD_RECORD,
            &key,
            json!({"schemaVersion": 1, "runId": frame.run_id, "child": frame}),
        )
    }

    /// Newest revision of every child recorded for one Run, ordered by child id.
    pub(crate) fn subagent_children(
        &self,
        run_id: &StableId,
    ) -> Result<Vec<SubagentChildFrameV1>, WorkflowPipelineError> {
        let mut latest: BTreeMap<String, SubagentChildFrameV1> = BTreeMap::new();
        for value in self.events(CHILD_RECORD)? {
            if value.get("runId").and_then(Value::as_str) != Some(run_id.as_str()) {
                continue;
            }
            let Some(child) = value.get("child") else {
                continue;
            };
            let Ok(frame) = serde_json::from_value::<SubagentChildFrameV1>(child.clone()) else {
                continue;
            };
            match latest.get(&frame.child_id) {
                Some(entry) if entry.head_revision >= frame.head_revision => {}
                _ => {
                    latest.insert(frame.child_id.clone(), frame);
                }
            }
        }
        Ok(latest.into_values().collect())
    }

    /// Newest revision of one child, scoped to the exact Run.
    pub(crate) fn subagent_child(
        &self,
        run_id: &StableId,
        child_id: &str,
    ) -> Result<Option<SubagentChildFrameV1>, WorkflowPipelineError> {
        Ok(self
            .subagent_children(run_id)?
            .into_iter()
            .find(|frame| frame.child_id == child_id))
    }
}
