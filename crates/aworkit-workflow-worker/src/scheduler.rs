//! Deterministic token scheduling and frozen rule-based transition selection.

use std::collections::{BTreeMap, VecDeque};

use aworkit_protocol::StableId;
use serde_json::Value;
use thiserror::Error;

use crate::plan::{ExecutionPlan, Transition};

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
