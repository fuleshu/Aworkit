//! Typed node contracts. Brokered operations become proposals, never direct effects.

use std::collections::BTreeMap;

use aworkit_protocol::StableId;
use serde_json::{Value, json};
use thiserror::Error;

use crate::plan::PlanNode;

/// A bounded execution identity, unique within a worker run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeTask {
    pub node_id: StableId,
    pub attempt: u32,
    pub input_revision: u64,
}

/// Work that must pass through the trusted core's invocation broker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub kind: String,
    pub node_id: StableId,
    pub payload: Value,
}

/// A completed pure result or a deferred brokered proposal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeOutcome {
    Completed(Value),
    Proposed(Proposal),
    Failed(String),
}

type Executor = fn(&PlanNode, &NodeTask, &Value) -> NodeOutcome;

/// Maps only frozen node type/version pairs to known executor contracts.
#[derive(Default)]
pub struct ExecutorRegistry {
    executors: BTreeMap<(String, u16), Executor>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NodeError {
    #[error("no pinned executor for {0}@{1}")]
    MissingExecutor(String, u16),
}

impl ExecutorRegistry {
    /// Creates the deliberately small built-in executor set.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        registry.register("noop", 1, |_node, _task, input| {
            NodeOutcome::Completed(input.clone())
        });
        registry.register("set_value", 1, |node, _task, _input| {
            NodeOutcome::Completed(node.config.clone())
        });
        registry.register("broker", 1, |node, _task, input| {
            NodeOutcome::Proposed(Proposal {
                kind: node
                    .config
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("capability")
                    .to_owned(),
                node_id: node.id.clone(),
                payload: json!({"input": input, "config": node.config}),
            })
        });
        registry
    }
    /// Registers a concrete, pinned pure executor implementation.
    pub fn register(&mut self, name: &str, version: u16, executor: Executor) {
        self.executors.insert((name.to_owned(), version), executor);
    }
    /// Executes a known node or returns a deterministic unsupported-contract failure.
    pub fn execute(
        &self,
        node: &PlanNode,
        task: &NodeTask,
        input: &Value,
    ) -> Result<NodeOutcome, NodeError> {
        self.executors
            .get(&(node.node_type.clone(), node.version))
            .map(|executor| executor(node, task, input))
            .ok_or_else(|| NodeError::MissingExecutor(node.node_type.clone(), node.version))
    }
}
