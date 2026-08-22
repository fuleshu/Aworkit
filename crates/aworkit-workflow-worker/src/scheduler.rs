//! Deterministic token scheduling and frozen rule-based transition selection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aworkit_protocol::{StableId, WorkerExecutorKindV1, WorkerTransitionV1};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::plan::{ExecutionPlan, Transition};
use crate::{plan::ExecutionPlanV1, routing::evaluate_predicate};

/// A runnable point in the immutable graph and the context it may read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub id: u64,
    pub node_id: StableId,
    pub context_revision: u64,
}
/// A frozen route result. Selection order is priority then stable transition ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteDecision {
    pub transition: Transition,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SchedulerError {
    #[error("unknown node {0}")]
    UnknownNode(String),
    #[error("no legal transition from node {0}")]
    NoTransition(String),
}

/// A single-threaded deterministic queue. Concurrent execution may return results
/// in any order, but callers only admit them by this stable token identity.
#[derive(Debug)]
pub struct Scheduler {
    plan: ExecutionPlan,
    queue: VecDeque<Token>,
    next_token: u64,
    loop_counts: BTreeMap<String, u32>,
}
impl Scheduler {
    #[must_use]
    pub fn new(plan: ExecutionPlan) -> Self {
        Self {
            plan,
            queue: VecDeque::new(),
            next_token: 1,
            loop_counts: BTreeMap::new(),
        }
    }
    pub fn enqueue(
        &mut self,
        node_id: StableId,
        context_revision: u64,
    ) -> Result<Token, SchedulerError> {
        if self.plan.node(&node_id).is_none() {
            return Err(SchedulerError::UnknownNode(node_id.to_string()));
        }
        let token = Token {
            id: self.next_token,
            node_id,
            context_revision,
        };
        self.next_token += 1;
        self.queue.push_back(token.clone());
        Ok(token)
    }
    #[must_use]
    pub fn next(&mut self) -> Option<Token> {
        self.queue.pop_front()
    }
    /// Chooses the first outgoing transition whose optional `when` config matches.
    pub fn route(
        &mut self,
        token: &Token,
        context: &Value,
    ) -> Result<RouteDecision, SchedulerError> {
        let edge = self
            .plan
            .outgoing(&token.node_id)
            .iter()
            .find(|edge| self.is_legal(edge, context))
            .cloned()
            .ok_or_else(|| SchedulerError::NoTransition(token.node_id.to_string()))?;
        if edge.loop_bound.is_some() {
            *self.loop_counts.entry(edge.id.to_string()).or_default() += 1;
        }
        Ok(RouteDecision { transition: edge })
    }
    fn is_legal(&self, edge: &Transition, context: &Value) -> bool {
        if let Some(bound) = edge.loop_bound
            && self.loop_counts.get(edge.id.as_str()).copied().unwrap_or(0) >= bound
        {
            return false;
        }
        // A transition can require an exact visible JSON scalar carried by its source port.
        edge.from_port == "default"
            || context
                .get(&edge.from_port)
                .is_some_and(|value| !value.is_null())
    }
    /// Admits the next graph token only after a normalized, committed outcome.
    pub fn admit_transition(
        &mut self,
        route: RouteDecision,
        context_revision: u64,
    ) -> Result<Token, SchedulerError> {
        self.enqueue(route.transition.to, context_revision)
    }
    #[must_use]
    pub fn plan(&self) -> &ExecutionPlan {
        &self.plan
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStateV1 {
    Ready,
    InFlight,
    AwaitingCommit,
    Completed,
    Suspended,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenV1 {
    pub token_id: StableId,
    pub node_id: StableId,
    pub context_revision: u64,
    pub logical_tick: u64,
    pub branch_lineage: String,
    pub state: TokenStateV1,
    pub awaiting_proposal_id: Option<StableId>,
    pub selected_transition_id: Option<StableId>,
    pub selected_loop_id: Option<StableId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadyKeyV1 {
    logical_tick: u64,
    node_ordinal: u32,
    branch_lineage: String,
    token_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransitionProposalV1 {
    pub proposal_id: StableId,
    pub token_id: StableId,
    pub transition_id: StableId,
    pub facts: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalProposalV1 {
    pub proposal_id: StableId,
    pub token_id: StableId,
    pub outcome: String,
    pub facts: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AckAdmissionV1 {
    pub duplicate: bool,
    pub admitted_token: Option<TokenV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchedulerCheckpointV1 {
    pub logical_tick: u64,
    pub next_token_ordinal: u64,
    pub committed_cursor: u64,
    pub tokens: Vec<TokenV1>,
    pub ready: Vec<(u64, u32, String, String)>,
    pub loop_counts: BTreeMap<String, u32>,
    pub acknowledged_proposals: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SchedulerErrorV1 {
    #[error("unknown node {0}")]
    UnknownNode(String),
    #[error("unknown token {0}")]
    UnknownToken(String),
    #[error("token {0} is not in the required state")]
    InvalidTokenState(String),
    #[error("no declared transition matched node {0}")]
    NoTransition(String),
    #[error("route predicate is invalid: {0}")]
    InvalidPredicate(String),
    #[error("loop {0} exhausted its frozen bound")]
    LoopExhausted(String),
    #[error("proposal acknowledgement does not match token")]
    AckMismatch,
    #[error("token {0} does not name a frozen terminal node")]
    NotTerminal(String),
    #[error("committed cursor regressed")]
    CursorRegression,
    #[error("scheduler checkpoint is inconsistent")]
    InvalidCheckpoint,
    #[error("stable identifier construction failed")]
    InvalidIdentifier,
}

/// A deterministic scheduler whose only progress across an effect boundary is
/// admission of a proposal after the trusted core acknowledges its commit.
#[derive(Debug)]
pub struct SchedulerV1 {
    plan: ExecutionPlanV1,
    node_ordinals: BTreeMap<String, u32>,
    ready: BTreeSet<ReadyKeyV1>,
    tokens: BTreeMap<String, TokenV1>,
    next_token_ordinal: u64,
    logical_tick: u64,
    committed_cursor: u64,
    loop_counts: BTreeMap<String, u32>,
    acknowledged_proposals: BTreeMap<String, Option<String>>,
}

impl SchedulerV1 {
    #[must_use]
    pub fn new(plan: ExecutionPlanV1) -> Self {
        let node_ordinals = plan
            .snapshot()
            .nodes
            .iter()
            .enumerate()
            .map(|(ordinal, node)| {
                (
                    node.node_id.as_str().to_owned(),
                    u32::try_from(ordinal).expect("snapshot bounds fit u32"),
                )
            })
            .collect();
        Self {
            plan,
            node_ordinals,
            ready: BTreeSet::new(),
            tokens: BTreeMap::new(),
            next_token_ordinal: 1,
            logical_tick: 0,
            committed_cursor: 0,
            loop_counts: BTreeMap::new(),
            acknowledged_proposals: BTreeMap::new(),
        }
    }

    pub fn seed_entries(
        &mut self,
        context_revision: u64,
    ) -> Result<Vec<TokenV1>, SchedulerErrorV1> {
        let entries = self.plan.snapshot().entry_nodes.clone();
        entries
            .into_iter()
            .map(|node| self.enqueue(node, context_revision, "root".to_owned()))
            .collect()
    }

    pub fn enqueue(
        &mut self,
        node_id: StableId,
        context_revision: u64,
        branch_lineage: String,
    ) -> Result<TokenV1, SchedulerErrorV1> {
        let ordinal = *self
            .node_ordinals
            .get(node_id.as_str())
            .ok_or_else(|| SchedulerErrorV1::UnknownNode(node_id.to_string()))?;
        if branch_lineage.is_empty() || branch_lineage.len() > 512 {
            return Err(SchedulerErrorV1::InvalidCheckpoint);
        }
        let token_id = stable_id(&format!(
            "token:{}:{}:{}:{}",
            self.plan.fingerprint(),
            self.next_token_ordinal,
            node_id,
            branch_lineage
        ))?;
        self.next_token_ordinal = self
            .next_token_ordinal
            .checked_add(1)
            .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
        let token = TokenV1 {
            token_id: token_id.clone(),
            node_id,
            context_revision,
            logical_tick: self.logical_tick,
            branch_lineage: branch_lineage.clone(),
            state: TokenStateV1::Ready,
            awaiting_proposal_id: None,
            selected_transition_id: None,
            selected_loop_id: None,
        };
        self.ready.insert(ReadyKeyV1 {
            logical_tick: token.logical_tick,
            node_ordinal: ordinal,
            branch_lineage,
            token_id: token_id.as_str().to_owned(),
        });
        self.tokens
            .insert(token_id.as_str().to_owned(), token.clone());
        Ok(token)
    }

    pub fn claim_next(&mut self) -> Option<TokenV1> {
        let key = self.ready.pop_first()?;
        let token = self.tokens.get_mut(&key.token_id)?;
        if token.state != TokenStateV1::Ready {
            return None;
        }
        token.state = TokenStateV1::InFlight;
        Some(token.clone())
    }

    pub fn propose_transition(
        &mut self,
        token_id: &StableId,
        facts: Value,
    ) -> Result<TransitionProposalV1, SchedulerErrorV1> {
        let token = self
            .tokens
            .get(token_id.as_str())
            .ok_or_else(|| SchedulerErrorV1::UnknownToken(token_id.to_string()))?;
        if token.state != TokenStateV1::InFlight {
            return Err(SchedulerErrorV1::InvalidTokenState(token_id.to_string()));
        }
        let transition = self.select_transition(&token.node_id, &facts)?;
        if let Some(loop_id) = &transition.declared_loop_id {
            let descriptor = self
                .plan
                .loop_descriptor(loop_id)
                .ok_or_else(|| SchedulerErrorV1::LoopExhausted(loop_id.to_string()))?;
            let committed = self.loop_counts.get(loop_id.as_str()).copied().unwrap_or(0);
            let pending = u32::try_from(
                self.tokens
                    .values()
                    .filter(|candidate| {
                        candidate.state == TokenStateV1::AwaitingCommit
                            && candidate.selected_loop_id.as_ref() == Some(loop_id)
                    })
                    .count(),
            )
            .map_err(|_| SchedulerErrorV1::LoopExhausted(loop_id.to_string()))?;
            if committed.saturating_add(pending) >= descriptor.maximum_iterations {
                return Err(SchedulerErrorV1::LoopExhausted(loop_id.to_string()));
            }
        }
        let proposal_id = stable_id(&format!(
            "transition:{}:{}:{}",
            self.plan.fingerprint(),
            token_id,
            transition.transition_id
        ))?;
        let token = self
            .tokens
            .get_mut(token_id.as_str())
            .expect("token checked above");
        token.state = TokenStateV1::AwaitingCommit;
        token.awaiting_proposal_id = Some(proposal_id.clone());
        token.selected_transition_id = Some(transition.transition_id.clone());
        token.selected_loop_id = transition.declared_loop_id.clone();
        Ok(TransitionProposalV1 {
            proposal_id,
            token_id: token_id.clone(),
            transition_id: transition.transition_id,
            facts,
        })
    }

    /// Proposes the canonical terminal event for a frozen terminal node. The
    /// token remains blocked until the trusted core commits and acknowledges
    /// that proposal; reaching a terminal node alone is not completion.
    pub fn propose_terminal(
        &mut self,
        token_id: &StableId,
        outcome: impl Into<String>,
        facts: Value,
    ) -> Result<TerminalProposalV1, SchedulerErrorV1> {
        let outcome = outcome.into();
        let token = self
            .tokens
            .get(token_id.as_str())
            .ok_or_else(|| SchedulerErrorV1::UnknownToken(token_id.to_string()))?;
        if token.state != TokenStateV1::InFlight {
            return Err(SchedulerErrorV1::InvalidTokenState(token_id.to_string()));
        }
        if self
            .plan
            .node(&token.node_id)
            .is_none_or(|node| node.executor != WorkerExecutorKindV1::Terminal)
        {
            return Err(SchedulerErrorV1::NotTerminal(token_id.to_string()));
        }
        if outcome.trim().is_empty() || outcome.len() > 256 {
            return Err(SchedulerErrorV1::InvalidCheckpoint);
        }
        let proposal_id = stable_id(&format!(
            "terminal:{}:{}:{}",
            self.plan.fingerprint(),
            token_id,
            outcome
        ))?;
        let token = self
            .tokens
            .get_mut(token_id.as_str())
            .expect("token checked above");
        token.state = TokenStateV1::AwaitingCommit;
        token.awaiting_proposal_id = Some(proposal_id.clone());
        token.selected_transition_id = None;
        token.selected_loop_id = None;
        Ok(TerminalProposalV1 {
            proposal_id,
            token_id: token_id.clone(),
            outcome,
            facts,
        })
    }

    pub fn acknowledge_transition(
        &mut self,
        proposal_id: &StableId,
        committed_cursor: u64,
        context_revision: u64,
    ) -> Result<AckAdmissionV1, SchedulerErrorV1> {
        if committed_cursor < self.committed_cursor {
            return Err(SchedulerErrorV1::CursorRegression);
        }
        if let Some(child) = self.acknowledged_proposals.get(proposal_id.as_str()) {
            let admitted_token = child.as_ref().and_then(|id| self.tokens.get(id)).cloned();
            return Ok(AckAdmissionV1 {
                duplicate: true,
                admitted_token,
            });
        }
        if committed_cursor <= self.committed_cursor {
            return Err(SchedulerErrorV1::CursorRegression);
        }
        let source_id = self
            .tokens
            .values()
            .find(|token| token.awaiting_proposal_id.as_ref() == Some(proposal_id))
            .map(|token| token.token_id.clone())
            .ok_or(SchedulerErrorV1::AckMismatch)?;
        let source = self
            .tokens
            .get(source_id.as_str())
            .cloned()
            .ok_or(SchedulerErrorV1::AckMismatch)?;
        if source.state != TokenStateV1::AwaitingCommit {
            return Err(SchedulerErrorV1::AckMismatch);
        }
        let transition_id = source
            .selected_transition_id
            .as_ref()
            .ok_or(SchedulerErrorV1::AckMismatch)?;
        let transition = self
            .plan
            .transition(transition_id)
            .cloned()
            .ok_or(SchedulerErrorV1::AckMismatch)?;
        if let Some(loop_id) = &source.selected_loop_id {
            *self.loop_counts.entry(loop_id.to_string()).or_default() += 1;
        }
        self.tokens
            .get_mut(source_id.as_str())
            .expect("source exists")
            .state = TokenStateV1::Completed;
        self.logical_tick = self.logical_tick.saturating_add(1);
        self.committed_cursor = committed_cursor;
        let child = self.enqueue(transition.to_node, context_revision, source.branch_lineage)?;
        self.acknowledged_proposals.insert(
            proposal_id.as_str().to_owned(),
            Some(child.token_id.as_str().to_owned()),
        );
        Ok(AckAdmissionV1 {
            duplicate: false,
            admitted_token: Some(child),
        })
    }

    pub fn acknowledge_terminal(
        &mut self,
        proposal_id: &StableId,
        committed_cursor: u64,
    ) -> Result<AckAdmissionV1, SchedulerErrorV1> {
        if committed_cursor < self.committed_cursor {
            return Err(SchedulerErrorV1::CursorRegression);
        }
        if self
            .acknowledged_proposals
            .contains_key(proposal_id.as_str())
        {
            return Ok(AckAdmissionV1 {
                duplicate: true,
                admitted_token: None,
            });
        }
        if committed_cursor <= self.committed_cursor {
            return Err(SchedulerErrorV1::CursorRegression);
        }
        let source_id = self
            .tokens
            .values()
            .find(|token| token.awaiting_proposal_id.as_ref() == Some(proposal_id))
            .map(|token| token.token_id.clone())
            .ok_or(SchedulerErrorV1::AckMismatch)?;
        let source = self
            .tokens
            .get(source_id.as_str())
            .ok_or(SchedulerErrorV1::AckMismatch)?;
        if source.state != TokenStateV1::AwaitingCommit
            || source.selected_transition_id.is_some()
            || self
                .plan
                .node(&source.node_id)
                .is_none_or(|node| node.executor != WorkerExecutorKindV1::Terminal)
        {
            return Err(SchedulerErrorV1::AckMismatch);
        }
        self.tokens
            .get_mut(source_id.as_str())
            .expect("source checked above")
            .state = TokenStateV1::Completed;
        self.logical_tick = self.logical_tick.saturating_add(1);
        self.committed_cursor = committed_cursor;
        self.acknowledged_proposals
            .insert(proposal_id.as_str().to_owned(), None);
        Ok(AckAdmissionV1 {
            duplicate: false,
            admitted_token: None,
        })
    }

    pub fn suspend(&mut self, token_id: &StableId) -> Result<(), SchedulerErrorV1> {
        let token = self
            .tokens
            .get_mut(token_id.as_str())
            .ok_or_else(|| SchedulerErrorV1::UnknownToken(token_id.to_string()))?;
        if token.state != TokenStateV1::InFlight {
            return Err(SchedulerErrorV1::InvalidTokenState(token_id.to_string()));
        }
        token.state = TokenStateV1::Suspended;
        Ok(())
    }

    pub fn resume(&mut self, token_id: &StableId) -> Result<(), SchedulerErrorV1> {
        let token = self
            .tokens
            .get_mut(token_id.as_str())
            .ok_or_else(|| SchedulerErrorV1::UnknownToken(token_id.to_string()))?;
        if token.state != TokenStateV1::Suspended {
            return Err(SchedulerErrorV1::InvalidTokenState(token_id.to_string()));
        }
        token.state = TokenStateV1::Ready;
        let node_ordinal = *self
            .node_ordinals
            .get(token.node_id.as_str())
            .ok_or_else(|| SchedulerErrorV1::UnknownNode(token.node_id.to_string()))?;
        self.ready.insert(ReadyKeyV1 {
            logical_tick: token.logical_tick,
            node_ordinal,
            branch_lineage: token.branch_lineage.clone(),
            token_id: token.token_id.as_str().to_owned(),
        });
        Ok(())
    }

    pub fn cancel_lineage(&mut self, lineage: &str) -> usize {
        let mut count = 0;
        self.ready
            .retain(|key| !lineage_matches(&key.branch_lineage, lineage));
        for token in self.tokens.values_mut() {
            // AwaitingCommit is deliberately excluded: cancellation cannot
            // erase a core-visible proposal and then reuse its loop capacity.
            if lineage_matches(&token.branch_lineage, lineage)
                && !matches!(
                    token.state,
                    TokenStateV1::AwaitingCommit
                        | TokenStateV1::Completed
                        | TokenStateV1::Cancelled
                )
            {
                token.state = TokenStateV1::Cancelled;
                count += 1;
            }
        }
        count
    }

    #[must_use]
    pub fn is_quiescent(&self) -> bool {
        self.ready.is_empty()
            && self.tokens.values().all(|token| {
                matches!(
                    token.state,
                    TokenStateV1::Completed | TokenStateV1::Suspended | TokenStateV1::Cancelled
                )
            })
    }

    #[must_use]
    pub fn checkpoint(&self) -> SchedulerCheckpointV1 {
        SchedulerCheckpointV1 {
            logical_tick: self.logical_tick,
            next_token_ordinal: self.next_token_ordinal,
            committed_cursor: self.committed_cursor,
            tokens: self.tokens.values().cloned().collect(),
            ready: self
                .ready
                .iter()
                .map(|key| {
                    (
                        key.logical_tick,
                        key.node_ordinal,
                        key.branch_lineage.clone(),
                        key.token_id.clone(),
                    )
                })
                .collect(),
            loop_counts: self.loop_counts.clone(),
            acknowledged_proposals: self.acknowledged_proposals.clone(),
        }
    }

    pub fn restore(
        plan: ExecutionPlanV1,
        checkpoint: SchedulerCheckpointV1,
    ) -> Result<Self, SchedulerErrorV1> {
        if checkpoint.next_token_ordinal == 0
            || u64::try_from(checkpoint.acknowledged_proposals.len())
                .map_err(|_| SchedulerErrorV1::InvalidCheckpoint)?
                > checkpoint.committed_cursor
        {
            return Err(SchedulerErrorV1::InvalidCheckpoint);
        }
        for (loop_id, count) in &checkpoint.loop_counts {
            let loop_id = StableId::parse(loop_id.clone())
                .map_err(|_| SchedulerErrorV1::InvalidCheckpoint)?;
            if checkpoint_loop_bound(&plan, &loop_id).is_none_or(|bound| *count > bound) {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
        }
        let mut scheduler = Self::new(plan);
        scheduler.logical_tick = checkpoint.logical_tick;
        scheduler.next_token_ordinal = checkpoint.next_token_ordinal;
        scheduler.committed_cursor = checkpoint.committed_cursor;
        scheduler.loop_counts = checkpoint.loop_counts;
        scheduler.acknowledged_proposals = checkpoint.acknowledged_proposals;
        let mut proposal_ids = BTreeSet::new();
        for token in checkpoint.tokens {
            if token.logical_tick > scheduler.logical_tick
                || validate_restored_token(&scheduler.plan, &token).is_err()
                || token
                    .awaiting_proposal_id
                    .as_ref()
                    .is_some_and(|id| !proposal_ids.insert(id.as_str().to_owned()))
                || scheduler
                    .tokens
                    .insert(token.token_id.as_str().to_owned(), token)
                    .is_some()
            {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
        }
        let expected_next = u64::try_from(scheduler.tokens.len())
            .map_err(|_| SchedulerErrorV1::InvalidCheckpoint)?
            .checked_add(1)
            .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
        if scheduler.next_token_ordinal != expected_next {
            return Err(SchedulerErrorV1::InvalidCheckpoint);
        }
        for (logical_tick, node_ordinal, branch_lineage, token_id) in checkpoint.ready {
            let token = scheduler
                .tokens
                .get(&token_id)
                .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
            if token.state != TokenStateV1::Ready
                || token.logical_tick != logical_tick
                || token.branch_lineage != branch_lineage
                || scheduler.node_ordinals.get(token.node_id.as_str()) != Some(&node_ordinal)
            {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
            if !scheduler.ready.insert(ReadyKeyV1 {
                logical_tick,
                node_ordinal,
                branch_lineage,
                token_id,
            }) {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
        }
        if scheduler.tokens.values().any(|token| {
            token.state == TokenStateV1::Ready
                && !scheduler
                    .ready
                    .iter()
                    .any(|key| key.token_id == token.token_id.as_str())
        }) {
            return Err(SchedulerErrorV1::InvalidCheckpoint);
        }
        for (proposal_id, child_id) in &scheduler.acknowledged_proposals {
            StableId::parse(proposal_id.clone())
                .map_err(|_| SchedulerErrorV1::InvalidCheckpoint)?;
            let source = scheduler
                .tokens
                .values()
                .find(|token| {
                    token.awaiting_proposal_id.as_ref().map(StableId::as_str)
                        == Some(proposal_id.as_str())
                })
                .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
            if source.state != TokenStateV1::Completed {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
            match child_id {
                Some(child_id) => {
                    let child = scheduler
                        .tokens
                        .get(child_id)
                        .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
                    let transition = source
                        .selected_transition_id
                        .as_ref()
                        .and_then(|id| scheduler.plan.transition(id))
                        .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
                    if transition.to_node != child.node_id
                        || source.branch_lineage != child.branch_lineage
                    {
                        return Err(SchedulerErrorV1::InvalidCheckpoint);
                    }
                }
                None => {
                    if source.selected_transition_id.is_some()
                        || scheduler
                            .plan
                            .node(&source.node_id)
                            .is_none_or(|node| node.executor != WorkerExecutorKindV1::Terminal)
                    {
                        return Err(SchedulerErrorV1::InvalidCheckpoint);
                    }
                }
            }
        }
        for token in scheduler.tokens.values() {
            let acknowledged = token.awaiting_proposal_id.as_ref().is_some_and(|proposal| {
                scheduler
                    .acknowledged_proposals
                    .contains_key(proposal.as_str())
            });
            if (token.state == TokenStateV1::Completed) != acknowledged
                || (token.state == TokenStateV1::AwaitingCommit && acknowledged)
            {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
        }
        for descriptor in &scheduler.plan.snapshot().loop_descriptors {
            let committed = scheduler
                .loop_counts
                .get(descriptor.loop_id.as_str())
                .copied()
                .unwrap_or(0);
            let pending = u32::try_from(
                scheduler
                    .tokens
                    .values()
                    .filter(|token| {
                        token.state == TokenStateV1::AwaitingCommit
                            && token.selected_loop_id.as_ref() == Some(&descriptor.loop_id)
                    })
                    .count(),
            )
            .map_err(|_| SchedulerErrorV1::InvalidCheckpoint)?;
            if committed.saturating_add(pending) > descriptor.maximum_iterations {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
        }
        Ok(scheduler)
    }

    #[must_use]
    pub fn plan(&self) -> &ExecutionPlanV1 {
        &self.plan
    }

    fn select_transition(
        &self,
        node_id: &StableId,
        facts: &Value,
    ) -> Result<WorkerTransitionV1, SchedulerErrorV1> {
        let routes = self.plan.route_rules(node_id);
        if !routes.is_empty() {
            let decision = crate::routing::choose_route(node_id, routes, facts)
                .map_err(|error| SchedulerErrorV1::InvalidPredicate(error.to_string()))?;
            return self
                .plan
                .transition(&decision.transition_id)
                .cloned()
                .ok_or_else(|| SchedulerErrorV1::NoTransition(node_id.to_string()));
        }
        for transition in self.plan.outgoing(node_id) {
            let matches = transition
                .predicate
                .as_ref()
                .map_or(Ok(true), |predicate| {
                    evaluate_predicate(predicate, facts)
                        .map_err(|error| SchedulerErrorV1::InvalidPredicate(error.to_string()))
                })?;
            if matches {
                return Ok(transition.clone());
            }
        }
        Err(SchedulerErrorV1::NoTransition(node_id.to_string()))
    }
}

fn stable_id(material: &str) -> Result<StableId, SchedulerErrorV1> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    StableId::parse(format!("worker.{}", &digest[..48]))
        .map_err(|_| SchedulerErrorV1::InvalidIdentifier)
}

fn checkpoint_loop_bound(plan: &ExecutionPlanV1, loop_id: &StableId) -> Option<u32> {
    plan.loop_descriptor(loop_id)
        .map(|descriptor| descriptor.maximum_iterations)
}

fn validate_restored_token(
    plan: &ExecutionPlanV1,
    token: &TokenV1,
) -> Result<(), SchedulerErrorV1> {
    let node = plan
        .node(&token.node_id)
        .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
    if token.branch_lineage.is_empty() || token.branch_lineage.len() > 512 {
        return Err(SchedulerErrorV1::InvalidCheckpoint);
    }
    match token.state {
        TokenStateV1::Ready
        | TokenStateV1::InFlight
        | TokenStateV1::Suspended
        | TokenStateV1::Cancelled => {
            if token.awaiting_proposal_id.is_some()
                || token.selected_transition_id.is_some()
                || token.selected_loop_id.is_some()
            {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
        }
        TokenStateV1::AwaitingCommit | TokenStateV1::Completed => {
            if token.awaiting_proposal_id.is_none() {
                return Err(SchedulerErrorV1::InvalidCheckpoint);
            }
            match &token.selected_transition_id {
                Some(transition_id) => {
                    let transition = plan
                        .transition(transition_id)
                        .ok_or(SchedulerErrorV1::InvalidCheckpoint)?;
                    if transition.from_node != token.node_id
                        || transition.declared_loop_id != token.selected_loop_id
                    {
                        return Err(SchedulerErrorV1::InvalidCheckpoint);
                    }
                }
                None => {
                    if token.selected_loop_id.is_some()
                        || node.executor != WorkerExecutorKindV1::Terminal
                    {
                        return Err(SchedulerErrorV1::InvalidCheckpoint);
                    }
                }
            }
        }
    }
    Ok(())
}

fn lineage_matches(candidate: &str, cancelled: &str) -> bool {
    candidate == cancelled
        || candidate
            .strip_prefix(cancelled)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
