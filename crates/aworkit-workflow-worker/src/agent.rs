//! Model/agent orchestration that emits only trusted-core broker proposals.

use aworkit_protocol::StableId;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    context::ContextStore,
    limits::{LimitController, LimitError, Reservation},
    node::Proposal,
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
