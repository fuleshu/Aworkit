//! Model/agent orchestration that emits only trusted-core broker proposals.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{
    CapabilityOutcomeClassV1, CapabilityOutcomeV1, StableId, WorkerInvocationProposalV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    context::ContextStore,
    limits::{LimitController, LimitError, Reservation},
    node::Proposal,
};
use crate::{
    context::{ChildContextSpec, ChildIntegration},
    limits::{BudgetEnvelope, LimitLedger, Usage},
};

/// One bounded internal model-tool turn, represented as a broker proposal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStep {
    pub proposal: Proposal,
    pub turn: u32,
}
/// Builds model steps without provider-native types or direct model access.
pub struct AgentLoop;
impl AgentLoop {
    pub fn next_step(
        limits: &mut LimitController,
        node_id: StableId,
        context: Value,
        turn: u32,
    ) -> Result<AgentStep, LimitError> {
        limits.reserve(Reservation {
            turns: 1,
            attempts: 1,
        })?;
        Ok(AgentStep {
            turn,
            proposal: Proposal {
                kind: "model".to_owned(),
                node_id,
                payload: json!({"context": context, "turn": turn}),
            },
        })
    }
}

/// A temporary child execution request with explicit input and result-integration target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentRequest {
    pub parent_revision: u64,
    pub delegated: Value,
    pub depth: u32,
    pub max_depth: u32,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SubagentError {
    #[error("nested subagent depth exceeds the frozen limit")]
    DepthExceeded,
}
/// Tracks no mutable shared child state; integration is a declared new parent revision.
pub struct SubagentManager;
impl SubagentManager {
    pub fn start(
        context: &mut ContextStore,
        request: &SubagentRequest,
    ) -> Result<u64, SubagentError> {
        if request.depth >= request.max_depth {
            return Err(SubagentError::DepthExceeded);
        }
        context
            .append(request.parent_revision, request.delegated.clone())
            .map_err(|_| SubagentError::DepthExceeded)
    }
    pub fn integrate(
        context: &mut ContextStore,
        parent: u64,
        result: Value,
    ) -> Result<u64, SubagentError> {
        context
            .append(parent, result)
            .map_err(|_| SubagentError::DepthExceeded)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLoopConfigV1 {
    pub loop_id: StableId,
    pub node_id: StableId,
    pub model_capability_ref: StableId,
    pub authority_manifest_ref: StableId,
    pub budget_ref: StableId,
    pub scope_id: String,
    pub maximum_turns: u32,
    /// Frozen upper bound reserved before each model invocation. The committed
    /// normalized outcome settles this reservation exactly once.
    pub turn_reservation: Usage,
    pub context_pointers: Vec<String>,
    pub allowed_tool_capability_refs: Vec<StableId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLoopCheckpointV1 {
    pub config: AgentLoopConfigV1,
    pub next_turn: u32,
    pub pending_invocation_id: Option<StableId>,
    pub pending_reservation_id: Option<String>,
    pub completed_outcome_ids: Vec<StableId>,
    pub cancelled: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AgentErrorV1 {
    #[error("agent loop exhausted its frozen turn bound")]
    TurnLimit,
    #[error("agent loop has an uncommitted invocation")]
    InvocationPending,
    #[error("capability is not permitted by the frozen loop")]
    CapabilityDenied,
    #[error("capability outcome does not match the pending invocation")]
    OutcomeMismatch,
    #[error("agent loop is cancelled")]
    Cancelled,
    #[error("context projection is invalid")]
    ContextProjection,
    #[error("budget admission failed: {0}")]
    Budget(String),
    #[error("stable identifier construction failed")]
    InvalidIdentifier,
}

/// A bounded Aworkit-owned model loop. It emits a broker proposal and cannot
/// advance a turn until the core returns the committed normalized outcome.
#[derive(Debug)]
pub struct AgentLoopV1 {
    config: AgentLoopConfigV1,
    next_turn: u32,
    pending_invocation_id: Option<StableId>,
    pending_reservation_id: Option<String>,
    completed_outcome_ids: BTreeSet<String>,
    cancelled: bool,
}

impl AgentLoopV1 {
    pub fn new(config: AgentLoopConfigV1) -> Result<Self, AgentErrorV1> {
        if config.maximum_turns == 0
            || config.scope_id.is_empty()
            || config.turn_reservation.is_zero()
            || config.turn_reservation.turns == 0
            || config.turn_reservation.attempts == 0
            || config.context_pointers.len() > 256
            || config.allowed_tool_capability_refs.len() > 256
        {
            return Err(AgentErrorV1::TurnLimit);
        }
        Ok(Self {
            config,
            next_turn: 0,
            pending_invocation_id: None,
            pending_reservation_id: None,
            completed_outcome_ids: BTreeSet::new(),
            cancelled: false,
        })
    }

    pub fn propose_model_turn(
        &mut self,
        context: &Value,
        limits: &mut LimitLedger,
    ) -> Result<WorkerInvocationProposalV1, AgentErrorV1> {
        if self.cancelled {
            return Err(AgentErrorV1::Cancelled);
        }
        if self.pending_invocation_id.is_some() {
            return Err(AgentErrorV1::InvocationPending);
        }
        if self.next_turn >= self.config.maximum_turns {
            return Err(AgentErrorV1::TurnLimit);
        }
        let invocation_id =
            stable_id(&format!("agent:{}:{}", self.config.loop_id, self.next_turn))?;
        let reservation_id = format!("agent.{}.{}", self.config.loop_id, self.next_turn);
        limits
            .reserve(
                &self.config.scope_id,
                reservation_id.clone(),
                self.config.turn_reservation,
            )
            .map_err(|error| AgentErrorV1::Budget(error.to_string()))?;
        let projected = match project_context(context, &self.config.context_pointers) {
            Ok(projected) => projected,
            Err(error) => {
                // Projection validation occurs after reservation so restore has
                // one deterministic reservation identifier. Roll it back on a
                // local validation failure because no invocation was emitted.
                limits
                    .release(&reservation_id)
                    .map_err(|release| AgentErrorV1::Budget(release.to_string()))?;
                return Err(error);
            }
        };
        self.pending_invocation_id = Some(invocation_id.clone());
        self.pending_reservation_id = Some(reservation_id);
        Ok(WorkerInvocationProposalV1 {
            invocation_id,
            node_id: self.config.node_id.clone(),
            attempt_id: stable_id(&format!(
                "attempt:{}:{}",
                self.config.loop_id, self.next_turn
            ))?,
            capability_ref: self.config.model_capability_ref.clone(),
            authority_manifest_ref: self.config.authority_manifest_ref.clone(),
            budget_ref: self.config.budget_ref.clone(),
            payload: json!({
                "turn": self.next_turn,
                "context": projected,
                "allowedToolCapabilityRefs": self.config.allowed_tool_capability_refs,
            }),
        })
    }

    pub fn settle_committed_outcome(
        &mut self,
        outcome: &CapabilityOutcomeV1,
        limits: &mut LimitLedger,
        actual_usage: Usage,
    ) -> Result<bool, AgentErrorV1> {
        if self.outcome_is_duplicate_or_matching(outcome)? {
            return Ok(false);
        }
        if actual_usage.turns != 1 || actual_usage.attempts != 1 {
            return Err(AgentErrorV1::Budget(
                "a committed model turn must charge exactly one turn and attempt".to_owned(),
            ));
        }
        self.settle_reserved_outcome(outcome, limits, actual_usage)
    }

    /// Settles one outer Agent invocation that durably aggregates a bounded
    /// provider/tool loop. The reservation contains the frozen maxima while
    /// committed usage records the provider turns and authority-settled tool
    /// calls that actually occurred.
    pub fn settle_committed_run_outcome(
        &mut self,
        outcome: &CapabilityOutcomeV1,
        limits: &mut LimitLedger,
        actual_usage: Usage,
    ) -> Result<bool, AgentErrorV1> {
        if self.outcome_is_duplicate_or_matching(outcome)? {
            return Ok(false);
        }
        let permits_zero_turns = matches!(
            outcome.class,
            CapabilityOutcomeClassV1::DefiniteNotStarted | CapabilityOutcomeClassV1::Denied
        );
        if actual_usage.turns != actual_usage.attempts
            || actual_usage.turns > u64::from(self.config.maximum_turns)
            || (actual_usage.turns == 0 && !permits_zero_turns)
        {
            return Err(AgentErrorV1::Budget(
                "a committed Agent run must charge its actual bounded provider turns and attempts"
                    .to_owned(),
            ));
        }
        self.settle_reserved_outcome(outcome, limits, actual_usage)
    }

    /// Returns `true` for an already-settled outcome and otherwise verifies
    /// that the outcome belongs to the sole pending Agent invocation.
    fn outcome_is_duplicate_or_matching(
        &self,
        outcome: &CapabilityOutcomeV1,
    ) -> Result<bool, AgentErrorV1> {
        if self
            .completed_outcome_ids
            .contains(outcome.outcome_id.as_str())
        {
            return Ok(true);
        }
        if self.pending_invocation_id.as_ref() != Some(&outcome.invocation_id) {
            return Err(AgentErrorV1::OutcomeMismatch);
        }
        Ok(false)
    }

    fn settle_reserved_outcome(
        &mut self,
        outcome: &CapabilityOutcomeV1,
        limits: &mut LimitLedger,
        actual_usage: Usage,
    ) -> Result<bool, AgentErrorV1> {
        let reservation_id = self
            .pending_reservation_id
            .as_deref()
            .ok_or(AgentErrorV1::OutcomeMismatch)?;
        limits
            .charge(reservation_id, outcome.outcome_id.as_str(), actual_usage)
            .map_err(|error| AgentErrorV1::Budget(error.to_string()))?;
        self.completed_outcome_ids
            .insert(outcome.outcome_id.as_str().to_owned());
        self.pending_invocation_id = None;
        self.pending_reservation_id = None;
        self.next_turn = self.next_turn.saturating_add(1);
        if matches!(
            outcome.class,
            CapabilityOutcomeClassV1::CancelledEvidence | CapabilityOutcomeClassV1::Denied
        ) {
            self.cancelled = true;
        }
        Ok(true)
    }

    pub fn validate_tool_capability(&self, capability: &StableId) -> Result<(), AgentErrorV1> {
        self.config
            .allowed_tool_capability_refs
            .contains(capability)
            .then_some(())
            .ok_or(AgentErrorV1::CapabilityDenied)
    }

    /// Cancels only while no invocation is in flight. Once a proposal has been
    /// emitted, cancellation must arrive as a committed normalized outcome so
    /// the reservation is settled and no-start/known-started evidence is kept.
    pub fn cancel(&mut self) -> Result<bool, AgentErrorV1> {
        if self.cancelled {
            return Ok(false);
        }
        if self.pending_invocation_id.is_some() {
            return Err(AgentErrorV1::InvocationPending);
        }
        self.cancelled = true;
        Ok(true)
    }

    #[must_use]
    pub fn checkpoint(&self) -> AgentLoopCheckpointV1 {
        AgentLoopCheckpointV1 {
            config: self.config.clone(),
            next_turn: self.next_turn,
            pending_invocation_id: self.pending_invocation_id.clone(),
            pending_reservation_id: self.pending_reservation_id.clone(),
            completed_outcome_ids: self
                .completed_outcome_ids
                .iter()
                .filter_map(|id| StableId::parse(id.clone()).ok())
                .collect(),
            cancelled: self.cancelled,
        }
    }

    pub fn restore(checkpoint: AgentLoopCheckpointV1) -> Result<Self, AgentErrorV1> {
        let pending_consistent = checkpoint.pending_invocation_id.is_some()
            == checkpoint.pending_reservation_id.is_some();
        let completed: BTreeSet<_> = checkpoint
            .completed_outcome_ids
            .iter()
            .map(StableId::as_str)
            .collect();
        if checkpoint.next_turn > checkpoint.config.maximum_turns
            || !pending_consistent
            || completed.len() != checkpoint.completed_outcome_ids.len()
            || usize::try_from(checkpoint.next_turn).ok() != Some(completed.len())
        {
            return Err(AgentErrorV1::TurnLimit);
        }
        let mut restored = Self::new(checkpoint.config)?;
        restored.next_turn = checkpoint.next_turn;
        restored.pending_invocation_id = checkpoint.pending_invocation_id;
        restored.pending_reservation_id = checkpoint.pending_reservation_id;
        restored.completed_outcome_ids = checkpoint
            .completed_outcome_ids
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        restored.cancelled = checkpoint.cancelled;
        Ok(restored)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStateV1 {
    Running,
    Completed,
    Integrated,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildFrameV1 {
    pub child_id: StableId,
    pub parent_revision: u64,
    pub child_root_revision: u64,
    pub child_head_revision: u64,
    pub scope_id: String,
    pub state: ChildStateV1,
    pub result: Option<Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentCheckpointV1 {
    pub children: Vec<ChildFrameV1>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SubagentErrorV1 {
    #[error("subagent {0} already exists")]
    DuplicateChild(String),
    #[error("unknown subagent {0}")]
    UnknownChild(String),
    #[error("subagent is not in the required lifecycle state")]
    InvalidState,
    #[error("subagent context operation failed: {0}")]
    Context(String),
    #[error("subagent budget operation failed: {0}")]
    Budget(String),
}

#[derive(Debug, Default)]
pub struct SubagentManagerV1 {
    children: BTreeMap<String, ChildFrameV1>,
}

impl SubagentManagerV1 {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(
        &mut self,
        context: &mut ContextStore,
        limits: &mut LimitLedger,
        child_id: StableId,
        parent_scope_id: &str,
        child_budget: BudgetEnvelope,
        spec: ChildContextSpec,
    ) -> Result<ChildFrameV1, SubagentErrorV1> {
        if self.children.contains_key(child_id.as_str()) || spec.child_id != child_id.as_str() {
            return Err(SubagentErrorV1::DuplicateChild(child_id.to_string()));
        }
        let scope_id = format!("child.{}", child_id.as_str());
        limits
            .create_child(scope_id.clone(), parent_scope_id, child_budget)
            .map_err(|error| SubagentErrorV1::Budget(error.to_string()))?;
        let root = match context.spawn_child(&spec) {
            Ok(root) => root,
            Err(error) => {
                limits
                    .close_child(&scope_id)
                    .map_err(|rollback| SubagentErrorV1::Budget(rollback.to_string()))?;
                return Err(SubagentErrorV1::Context(error.to_string()));
            }
        };
        let frame = ChildFrameV1 {
            child_id: child_id.clone(),
            parent_revision: spec.parent_revision,
            child_root_revision: root,
            child_head_revision: root,
            scope_id,
            state: ChildStateV1::Running,
            result: None,
        };
        self.children
            .insert(child_id.as_str().to_owned(), frame.clone());
        Ok(frame)
    }

    pub fn complete(
        &mut self,
        child_id: &StableId,
        child_head_revision: u64,
        result: Value,
    ) -> Result<(), SubagentErrorV1> {
        let frame = self
            .children
            .get_mut(child_id.as_str())
            .ok_or_else(|| SubagentErrorV1::UnknownChild(child_id.to_string()))?;
        if frame.state != ChildStateV1::Running {
            return Err(SubagentErrorV1::InvalidState);
        }
        frame.child_head_revision = child_head_revision;
        frame.result = Some(result);
        frame.state = ChildStateV1::Completed;
        Ok(())
    }

    pub fn integrate(
        &mut self,
        context: &mut ContextStore,
        limits: &mut LimitLedger,
        child_id: &StableId,
        parent_head: u64,
        integration: ChildIntegration,
    ) -> Result<u64, SubagentErrorV1> {
        let frame = self
            .children
            .get_mut(child_id.as_str())
            .ok_or_else(|| SubagentErrorV1::UnknownChild(child_id.to_string()))?;
        if frame.state != ChildStateV1::Completed {
            return Err(SubagentErrorV1::InvalidState);
        }
        limits
            .can_close_child(&frame.scope_id)
            .map_err(|error| SubagentErrorV1::Budget(error.to_string()))?;
        let result = frame.result.clone().ok_or(SubagentErrorV1::InvalidState)?;
        let revision = context
            .integrate_child(
                child_id.as_str(),
                frame.child_head_revision,
                parent_head,
                result,
                integration,
            )
            .map_err(|error| SubagentErrorV1::Context(error.to_string()))?;
        limits
            .close_child(&frame.scope_id)
            .map_err(|error| SubagentErrorV1::Budget(error.to_string()))?;
        frame.state = ChildStateV1::Integrated;
        Ok(revision)
    }

    pub fn cancel(
        &mut self,
        limits: &mut LimitLedger,
        child_id: &StableId,
    ) -> Result<bool, SubagentErrorV1> {
        let frame = self
            .children
            .get_mut(child_id.as_str())
            .ok_or_else(|| SubagentErrorV1::UnknownChild(child_id.to_string()))?;
        if matches!(
            frame.state,
            ChildStateV1::Integrated | ChildStateV1::Cancelled
        ) {
            return Ok(false);
        }
        limits
            .close_child(&frame.scope_id)
            .map_err(|error| SubagentErrorV1::Budget(error.to_string()))?;
        frame.state = ChildStateV1::Cancelled;
        Ok(true)
    }

    #[must_use]
    pub fn checkpoint(&self) -> SubagentCheckpointV1 {
        SubagentCheckpointV1 {
            children: self.children.values().cloned().collect(),
        }
    }

    pub fn restore(checkpoint: SubagentCheckpointV1) -> Result<Self, SubagentErrorV1> {
        let mut children = BTreeMap::new();
        let mut scopes = BTreeSet::new();
        for child in checkpoint.children {
            let valid_result = match child.state {
                ChildStateV1::Running => child.result.is_none(),
                ChildStateV1::Completed | ChildStateV1::Integrated => child.result.is_some(),
                ChildStateV1::Cancelled => true,
            };
            if child.parent_revision == 0
                || child.child_root_revision == 0
                || child.child_head_revision < child.child_root_revision
                || child.scope_id != format!("child.{}", child.child_id.as_str())
                || !valid_result
                || !scopes.insert(child.scope_id.clone())
            {
                return Err(SubagentErrorV1::InvalidState);
            }
            if children
                .insert(child.child_id.as_str().to_owned(), child)
                .is_some()
            {
                return Err(SubagentErrorV1::InvalidState);
            }
        }
        Ok(Self { children })
    }
}

fn project_context(context: &Value, pointers: &[String]) -> Result<Value, AgentErrorV1> {
    if pointers.is_empty() {
        return Ok(context.clone());
    }
    let mut projected = Map::new();
    for pointer in pointers {
        if pointer.is_empty() || pointer.len() > 512 || !pointer.starts_with('/') {
            return Err(AgentErrorV1::ContextProjection);
        }
        let value = context
            .pointer(pointer)
            .ok_or(AgentErrorV1::ContextProjection)?;
        projected.insert(pointer.clone(), value.clone());
    }
    Ok(Value::Object(projected))
}

fn stable_id(material: &str) -> Result<StableId, AgentErrorV1> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    StableId::parse(format!("agent.{}", &digest[..48])).map_err(|_| AgentErrorV1::InvalidIdentifier)
}
