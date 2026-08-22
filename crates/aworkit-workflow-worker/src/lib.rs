//! Deterministic, unprivileged workflow execution primitives.
//!
//! This crate deliberately contains no process IPC or capability implementation.
//! It turns core-frozen data into deterministic worker proposals; the trusted core
//! remains the authority, effect broker, and canonical event committer.

pub mod agent;
pub mod branch;
pub mod context;
pub mod gateway;
pub mod limits;
pub mod node;
pub mod plan;
pub mod policy;
pub mod routing;
pub mod runtime;
pub mod scheduler;
pub mod suspension;

pub use agent::{AgentLoop, AgentStep, SubagentManager, SubagentRequest};
pub use context::{ContextError, ContextRevision, ContextStore, JoinStrategy};
pub use limits::{Budget, LimitController, LimitError, Reservation};
pub use node::{ExecutorRegistry, NodeOutcome, NodeTask, Proposal};
pub use plan::{ExecutionPlan, FrozenRunSnapshot, PlanError, PlanNode, Transition};
pub use policy::{AttemptDecision, AttemptPolicy, EffectOutcome};
pub use runtime::{WorkerRuntimeError, WorkerServiceV1, serve_stdio};
pub use scheduler::{RouteDecision, Scheduler, SchedulerError, Token};
pub use suspension::{
    Checkpoint, CheckpointProposal, Rehydrator, Suspension, SuspensionController,
};
