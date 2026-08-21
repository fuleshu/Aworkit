//! Deterministic, unprivileged workflow execution primitives.
//!
//! This crate deliberately contains no process IPC or capability implementation.
//! It turns core-frozen data into deterministic worker proposals; the trusted core
//! remains the authority, effect broker, and canonical event committer.

mod agent;
mod context;
mod limits;
mod node;
mod plan;
mod policy;
mod scheduler;
mod suspension;

pub use agent::{AgentLoop, AgentStep, SubagentManager, SubagentRequest};
pub use context::{ContextError, ContextRevision, ContextStore, JoinStrategy};
pub use limits::{Budget, LimitController, LimitError, Reservation};
pub use node::{ExecutorRegistry, NodeOutcome, NodeTask, Proposal};
pub use plan::{ExecutionPlan, FrozenRunSnapshot, PlanError, PlanNode, Transition};
pub use policy::{AttemptDecision, AttemptPolicy, EffectOutcome};
pub use scheduler::{RouteDecision, Scheduler, SchedulerError, Token};
pub use suspension::{
    Checkpoint, CheckpointProposal, Rehydrator, Suspension, SuspensionController,
};
