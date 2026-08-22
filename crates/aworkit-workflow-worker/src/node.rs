//! Typed node contracts. Effectful work becomes a trusted-core proposal only.

use std::{collections::BTreeMap, sync::Arc};

use aworkit_protocol::{StableId, WorkerExecutorKindV1, WorkerInvocationProposalV1, WorkerNodeV1};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::plan::PlanNode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeTask {
    pub node_id: StableId,
    pub attempt: u32,
    pub input_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub kind: String,
    pub node_id: StableId,
    pub payload: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeOutcome {
    Completed(Value),
    Proposed(Proposal),
    Failed(String),
}

type Executor = fn(&PlanNode, &NodeTask, &Value) -> NodeOutcome;

#[derive(Default)]
pub struct ExecutorRegistry {
    executors: BTreeMap<(String, u16), Executor>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NodeError {
    #[error("no pinned executor for {0}@{1}")]
    MissingExecutor(String, u16),
    #[error("executor registry is sealed for an active Run")]
    RegistrySealed,
    #[error("executor type/version is already registered")]
    DuplicateExecutor,
    #[error("executor contribution hash differs from the frozen plan")]
    ContributionMismatch,
    #[error("node input or output violates its frozen schema: {0}")]
    SchemaViolation(String),
    #[error("node task identity does not match the frozen node")]
    TaskIdentityMismatch,
    #[error("brokered node is missing its frozen capability reference")]
    MissingCapability,
    #[error("node was cancelled before dispatch")]
    Cancelled,
    #[error("stable node or invocation identity could not be created")]
    InvalidIdentity,
}

impl ExecutorRegistry {
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

    pub fn register(&mut self, name: &str, version: u16, executor: Executor) {
        self.executors.insert((name.to_owned(), version), executor);
    }

    pub fn execute(
        &self,
        node: &PlanNode,
        task: &NodeTask,
        input: &Value,
    ) -> Result<NodeOutcome, NodeError> {
        if node.id != task.node_id {
            return Err(NodeError::TaskIdentityMismatch);
        }
        self.executors
            .get(&(node.node_type.clone(), node.version))
            .map(|executor| executor(node, task, input))
            .ok_or_else(|| NodeError::MissingExecutor(node.node_type.clone(), node.version))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeTaskV1 {
    pub token_id: StableId,
    pub node_id: StableId,
    pub attempt_id: StableId,
    pub input_revision: u64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeOutcomeV1 {
    Completed { value: Value },
    NeedCapability(WorkerInvocationProposalV1),
    Suspended { reason: String, state: Value },
    Failed { code: String, retry_eligible: bool },
}

pub type V1Executor = Arc<
    dyn Fn(&WorkerNodeV1, &NodeTaskV1, &Value) -> Result<NodeOutcomeV1, NodeError> + Send + Sync,
>;

#[derive(Clone)]
struct ExecutorBindingV1 {
    contribution_hash: String,
    kind: WorkerExecutorKindV1,
    executor: V1Executor,
}

/// Exact type/version/hash registry. Registration is build-generation setup;
/// `seal` permanently prevents hot replacement for active Runs.
#[derive(Clone, Default)]
pub struct ExecutorRegistryV1 {
    executors: BTreeMap<(String, u32), ExecutorBindingV1>,
    sealed: bool,
}

impl ExecutorRegistryV1 {
    #[must_use]
    pub fn with_audited_builtins(noop_hash: &str, set_value_hash: &str) -> Self {
        let mut registry = Self::default();
        registry
            .register(
                "aworkit.noop",
                1,
                noop_hash,
                WorkerExecutorKindV1::Pure,
                Arc::new(|_node, _task, input| {
                    Ok(NodeOutcomeV1::Completed {
                        value: input.clone(),
                    })
                }),
            )
            .expect("new registry is not sealed");
        registry
            .register(
                "aworkit.set_value",
                1,
                set_value_hash,
                WorkerExecutorKindV1::Pure,
                Arc::new(|node, _task, _input| {
                    Ok(NodeOutcomeV1::Completed {
                        value: node.config.clone(),
                    })
                }),
            )
            .expect("new registry is not sealed");
        registry
    }

    pub fn register(
        &mut self,
        node_type: impl Into<String>,
        node_version: u32,
        contribution_hash: impl Into<String>,
        kind: WorkerExecutorKindV1,
        executor: V1Executor,
    ) -> Result<(), NodeError> {
        if self.sealed {
            return Err(NodeError::RegistrySealed);
        }
        let key = (node_type.into(), node_version);
        if self.executors.contains_key(&key) {
            return Err(NodeError::DuplicateExecutor);
        }
        self.executors.insert(
            key,
            ExecutorBindingV1 {
                contribution_hash: contribution_hash.into(),
                kind,
                executor,
            },
        );
        Ok(())
    }

    pub fn seal(&mut self) {
        self.sealed = true;
    }

    pub fn execute(
        &self,
        node: &WorkerNodeV1,
        task: &NodeTaskV1,
        input: &Value,
        authority_manifest_ref: &StableId,
        budget_ref: &StableId,
    ) -> Result<NodeOutcomeV1, NodeError> {
        if node.node_id != task.node_id {
            return Err(NodeError::TaskIdentityMismatch);
        }
        if task.cancelled {
            return Err(NodeError::Cancelled);
        }
        validate_input(node, input)?;
        let binding = self
            .executors
            .get(&(node.node_type.clone(), node.node_version))
            .ok_or_else(|| {
                NodeError::MissingExecutor(
                    node.node_type.clone(),
                    u16::try_from(node.node_version).unwrap_or(u16::MAX),
                )
            })?;
        if binding.contribution_hash != node.contribution_hash || binding.kind != node.executor {
            return Err(NodeError::ContributionMismatch);
        }
        let outcome = (binding.executor)(node, task, input)?;
        match &outcome {
            NodeOutcomeV1::Completed { value } => validate_output(node, value)?,
            NodeOutcomeV1::NeedCapability(proposal) => {
                if node.capability_ref.as_ref() != Some(&proposal.capability_ref) {
                    return Err(NodeError::MissingCapability);
                }
            }
            NodeOutcomeV1::Suspended { .. } | NodeOutcomeV1::Failed { .. } => {}
        }
        // These references are deliberately required at dispatch time even for
        // pure nodes so a caller cannot accidentally use an unfrozen context.
        if authority_manifest_ref.as_str().is_empty() || budget_ref.as_str().is_empty() {
            return Err(NodeError::InvalidIdentity);
        }
        Ok(outcome)
    }

    /// Constructs a stable broker proposal. The digest-based identity remains
    /// inside StableId's bound even when node and attempt IDs are maximal.
    pub fn broker_proposal(
        node: &WorkerNodeV1,
        task: &NodeTaskV1,
        input: &Value,
        authority_manifest_ref: StableId,
        budget_ref: StableId,
    ) -> Result<NodeOutcomeV1, NodeError> {
        let capability_ref = node
            .capability_ref
            .clone()
            .ok_or(NodeError::MissingCapability)?;
        validate_input(node, input)?;
        let bytes = serde_json::to_vec(&(
            task.token_id.as_str(),
            task.node_id.as_str(),
            task.attempt_id.as_str(),
            capability_ref.as_str(),
            input,
        ))
        .map_err(|_| NodeError::SchemaViolation("encoding".to_owned()))?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let invocation_id = StableId::parse(format!("inv.{}", &digest[..40]))
            .map_err(|_| NodeError::InvalidIdentity)?;
        Ok(NodeOutcomeV1::NeedCapability(WorkerInvocationProposalV1 {
            invocation_id,
            node_id: node.node_id.clone(),
            attempt_id: task.attempt_id.clone(),
            capability_ref,
            authority_manifest_ref,
            budget_ref,
            payload: json!({"input": input, "config": node.config}),
        }))
    }
}

fn validate_input(node: &WorkerNodeV1, value: &Value) -> Result<(), NodeError> {
    // A node with one required input validates the whole delivered value. Richer
    // multi-port inputs are objects keyed by the frozen port names.
    if node.inputs.len() == 1 && node.inputs[0].name == "input" {
        return validate_schema_ref(node.inputs[0].schema_ref.as_deref(), value);
    }
    let object = value
        .as_object()
        .ok_or_else(|| NodeError::SchemaViolation("multi-port input must be an object".into()))?;
    for port in &node.inputs {
        match object.get(&port.name) {
            Some(value) => validate_schema_ref(port.schema_ref.as_deref(), value)?,
            None if port.required => {
                return Err(NodeError::SchemaViolation(format!(
                    "missing required input {}",
                    port.name
                )));
            }
            None => {}
        }
    }
    Ok(())
}

fn validate_output(node: &WorkerNodeV1, value: &Value) -> Result<(), NodeError> {
    if node.outputs.len() == 1 && node.outputs[0].name == "output" {
        return validate_schema_ref(node.outputs[0].schema_ref.as_deref(), value);
    }
    let object = value
        .as_object()
        .ok_or_else(|| NodeError::SchemaViolation("multi-port output must be an object".into()))?;
    for port in &node.outputs {
        match object.get(&port.name) {
            Some(value) => validate_schema_ref(port.schema_ref.as_deref(), value)?,
            None if port.required => {
                return Err(NodeError::SchemaViolation(format!(
                    "missing required output {}",
                    port.name
                )));
            }
            None => {}
        }
    }
    Ok(())
}

fn validate_schema_ref(schema: Option<&str>, value: &Value) -> Result<(), NodeError> {
    let valid = match schema.unwrap_or("json:any") {
        "json:any" => true,
        "json:null" => value.is_null(),
        "json:boolean" => value.is_boolean(),
        "json:number" => value.is_number(),
        "json:string" => value.is_string(),
        "json:array" => value.is_array(),
        "json:object" => value.is_object(),
        // Named schema references were validated and pinned by the core. The
        // worker validates their identity at plan compilation and delegates
        // full JSON-Schema evaluation to a replaceable bounded adapter.
        reference if reference.starts_with("schema:") => true,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(NodeError::SchemaViolation(
            schema.unwrap_or("json:any").to_owned(),
        ))
    }
}
