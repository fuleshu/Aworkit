#![allow(
    clippy::assigning_clones,
    clippy::cast_possible_truncation,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::duration_suboptimal_units,
    clippy::format_collect,
    clippy::if_not_else,
    clippy::ignored_unit_patterns,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_pass_by_value,
    clippy::needless_question_mark,
    clippy::nonminimal_bool,
    clippy::op_ref,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::redundant_else,
    clippy::should_implement_trait,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::uninlined_format_args,
    clippy::unnecessary_wraps,
    clippy::unnested_or_patterns,
    clippy::unused_self,
    clippy::wildcard_imports,
    clippy::zero_sized_map_values
)]
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
