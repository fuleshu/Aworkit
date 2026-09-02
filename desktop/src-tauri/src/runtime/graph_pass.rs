//! Generic v1 workflow-graph pass executor.
//!
//! One graph pass runs per user input on the frozen workflow document. Nodes
//! execute in deterministic topological order with implicit joins (a node runs
//! once every active predecessor settles), conditions route true/false, and the
//! pass ends at a wait or completion node. The agent node owns the standard
//! bounded model/tool loop; model_call nodes are single no-tool completions;
//! tool nodes settle exactly one bound capability invocation through the same
//! durable authority used by the agent loop. An approval node suspends the pass
//! with a durably restorable prefix so a later decision resumes without
//! recomputing completed model work.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{
    CancellationToken, FrozenModelGateway, ModelCandidateV1, ModelDispatchEvidenceV1,
    ModelRequestV1, ModelResolutionPlanV1, ModelToolCallV1, ModelToolExchangeV1, ProviderError,
    project_model_events,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    documents::validate_v1_executable_catalog,
    model_tool_loop::{
        ModelToolInvocationPortV1, ModelToolLoopPendingV1, ModelToolLoopRequestV1,
        ModelToolLoopRunV1, PROVIDER_TIMEOUT_NOTICE, execute_model_tool_loop_approval_v1,
        resume_model_tool_loop_v1,
    },
    pipeline::{
        WORKFLOW_MAX_ASSISTANT_TEXT_BYTES, WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES, WorkflowMessageV1,
    },
    plan_contract::parse_plan_output_v1,
    tool_loop::{StoredFileToolBindingV1, ToolApprovalChallengeV1, WorkflowToolActivityV1},
};

pub(crate) const MAXIMUM_GRAPH_NODES: usize = 64;
const MAXIMUM_NODE_OUTPUT_BYTES: usize = WORKFLOW_MAX_ASSISTANT_TEXT_BYTES;
const MAXIMUM_AGENT_CONTEXT_BYTES: usize = 32 * 1024;
const MAXIMUM_MODEL_CALL_INPUT_BYTES: usize = 96 * 1024;
const MAXIMUM_AGENT_TURNS_WITH_TOOLS: u32 = 12;

/// Per-node pass budget ceilings derived from the frozen snapshot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphPassBudgetV1 {
    pub turns: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub actions: u64,
    pub maximum_timeout_recoveries: u32,
    pub maximum_tool_output_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphNodeActivityV1 {
    pub node_id: String,
    pub node_type: String,
    pub label: String,
    pub status: String,
    pub summary: String,
    /// Exact normalized value presented to the node when it became active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Exact bounded value produced by the node, or its terminal error data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphApprovalRequestV1 {
    pub decision_id: String,
    pub node_id: String,
    pub title: String,
    pub message: String,
}

/// Durable prefix snapshot written when a pass suspends at an approval node.
/// Resuming restores these values and continues from the pending node without
/// recomputing any completed model or tool work.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PendingGraphPassStateV1 {
    pub schema_version: u16,
    pub decision_id: String,
    pub invocation_id: String,
    pub request_id: String,
    pub chat_id: String,
    pub run_id: String,
    pub values: BTreeMap<String, Value>,
    pub completed: Vec<String>,
    pub pending_node_id: String,
    pub title: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_loop: Option<AgentLoopSuspensionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_category: Option<String>,
    pub conversation: Vec<WorkflowMessageV1>,
    pub activity: Vec<GraphNodeActivityV1>,
    pub tool_activity: Vec<WorkflowToolActivityV1>,
    /// Migration sink for suspended records written before semantic events
    /// became canonical. It is never produced by the current runtime.
    #[allow(dead_code)]
    #[serde(default, rename = "runActivity", skip_serializing)]
    pub legacy_run_activity: Vec<Value>,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub input_units: u64,
    pub output_units: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    #[serde(default)]
    pub timeout_recoveries: u32,
}

/// Durable agent-loop suspension captured when a PerInvocation tool call asks
/// for approval inside an agent node.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentLoopSuspensionV1 {
    pub node_id: String,
    pub pending: ModelToolLoopPendingV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphPassStatusV1 {
    Succeeded,
    Failed,
    AwaitingApproval,
}

#[derive(Clone, Debug)]
pub(crate) struct GraphPassOutcomeV1 {
    pub status: GraphPassStatusV1,
    pub assistant_text: Option<String>,
    pub error: Option<String>,
    pub approval: Option<GraphApprovalRequestV1>,
    pub pending_state: Option<PendingGraphPassStateV1>,
    pub activity: Vec<GraphNodeActivityV1>,
    pub tool_activity: Vec<WorkflowToolActivityV1>,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub input_units: u64,
    pub output_units: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledGraphNodeV1 {
    pub id: String,
    pub node_type: String,
    pub label: String,
    pub configuration: Value,
    pub tool_bindings: Vec<StoredFileToolBindingV1>,
    pub maximum_turns: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledGraphEdgeV1 {
    pub source: String,
    pub target: String,
    pub route: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledGraphPassV1 {
    pub nodes: Vec<CompiledGraphNodeV1>,
    pub edges: Vec<CompiledGraphEdgeV1>,
    pub entry_node_id: String,
    pub topological_order: Vec<String>,
}

/// Compiles a validated v1 workflow document into the executable pass shape.
/// Tool bindings are resolved from the frozen binding set in the exact order
/// declared by agent toolIds; a node binding an uninstalled tool fails closed.
pub(crate) fn compile_graph_pass(
    workflow: &Value,
    tool_bindings: &[StoredFileToolBindingV1],
) -> Result<CompiledGraphPassV1, String> {
    validate_v1_executable_catalog(workflow)?;
    let document_nodes = workflow["nodes"]
        .as_array()
        .ok_or_else(|| "workflow nodes are missing".to_owned())?;
    if document_nodes.len() > MAXIMUM_GRAPH_NODES {
        return Err(format!(
            "workflow graph exceeds the v1 {MAXIMUM_GRAPH_NODES}-node execution bound"
        ));
    }
    let mut nodes = Vec::with_capacity(document_nodes.len());
    let mut entry_node_id = String::new();
    for document_node in document_nodes {
        let object = document_node
            .as_object()
            .ok_or_else(|| "workflow node must be an object".to_owned())?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow node id is missing".to_owned())?
            .to_owned();
        StableId::parse(id.clone())
            .map_err(|_| format!("workflow node '{id}' id is not a valid stable identifier"))?;
        let node_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("workflow node '{id}' type is missing"))?
            .to_owned();
        let label = object
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| id.clone());
        let configuration = object
            .get("configuration")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut tool_bindings_for_node = Vec::new();
        let mut maximum_turns = 1_u32;
        match node_type.as_str() {
            "input" => {
                if entry_node_id.is_empty() {
                    entry_node_id = id.clone();
                }
            }
            "agent" => {
                let tool_ids = configuration
                    .get("toolIds")
                    .and_then(Value::as_array)
                    .map(|tool_ids| {
                        tool_ids
                            .iter()
                            .map(|tool_id| tool_id.as_str().map(str::to_owned))
                            .collect::<Option<Vec<String>>>()
                    })
                    .ok_or_else(|| {
                        format!("workflow node '{id}' toolIds must be an array of strings")
                    })?
                    .unwrap_or_default();
                for tool_id in tool_ids {
                    let binding = tool_bindings
                        .iter()
                        .find(|binding| binding.capability_id == tool_id)
                        .cloned()
                        .ok_or_else(|| {
                            format!(
                                "agent node '{id}' binds tool '{tool_id}' with no frozen native binding"
                            )
                        })?;
                    tool_bindings_for_node.push(binding);
                }
                maximum_turns = u32::try_from(
                    configuration
                        .get("maxTurns")
                        .and_then(Value::as_u64)
                        .unwrap_or(1),
                )
                .map_err(|_| format!("workflow node '{id}' maxTurns is out of range"))?;
            }
            "tool" => {
                let tool_id = configuration
                    .get("toolId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("tool node '{id}' has no toolId"))?;
                let binding = tool_bindings
                    .iter()
                    .find(|binding| binding.capability_id == tool_id)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "tool node '{id}' binds tool '{tool_id}' with no frozen native binding"
                        )
                    })?;
                tool_bindings_for_node.push(binding);
            }
            _ => {}
        }
        nodes.push(CompiledGraphNodeV1 {
            id,
            node_type,
            label,
            configuration,
            tool_bindings: tool_bindings_for_node,
            maximum_turns,
        });
    }
    if entry_node_id.is_empty() {
        return Err("workflow graph has no input node".to_owned());
    }
    let document_edges = workflow["edges"]
        .as_array()
        .ok_or_else(|| "workflow edges are missing".to_owned())?;
    let mut edges = Vec::with_capacity(document_edges.len());
    for document_edge in document_edges {
        let object = document_edge
            .as_object()
            .ok_or_else(|| "workflow transition must be an object".to_owned())?;
        let source = object
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow transition source is missing".to_owned())?
            .to_owned();
        let target = object
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow transition target is missing".to_owned())?
            .to_owned();
        if let Some(id) = object.get("id").and_then(Value::as_str)
            && StableId::parse(id.to_owned()).is_err()
        {
            return Err(format!(
                "workflow transition '{id}' id is not a valid stable identifier"
            ));
        }
        let route = object
            .get("configuration")
            .and_then(|configuration| configuration.get("route"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        edges.push(CompiledGraphEdgeV1 {
            source,
            target,
            route,
        });
    }
    let topological_order = topological_order(&nodes, &edges)?;
    Ok(CompiledGraphPassV1 {
        nodes,
        edges,
        entry_node_id,
        topological_order,
    })
}

fn topological_order(
    nodes: &[CompiledGraphNodeV1],
    edges: &[CompiledGraphEdgeV1],
) -> Result<Vec<String>, String> {
    let document_order: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    let mut indegree: BTreeMap<&str, usize> =
        nodes.iter().map(|node| (node.id.as_str(), 0)).collect();
    let mut successors: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges {
        successors
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
        *indegree.entry(edge.target.as_str()).or_default() += 1;
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| *id)
        .collect();
    ready.sort_by_key(|id| document_order.get(id).copied().unwrap_or(usize::MAX));
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(id) = ready.pop() {
        order.push(id.to_owned());
        for next in successors.get(id).into_iter().flatten() {
            let degree = indegree.get_mut(next).expect("edge target exists");
            *degree -= 1;
            if *degree == 0 {
                ready.push(next);
                ready.sort_by_key(|candidate| {
                    document_order.get(candidate).copied().unwrap_or(usize::MAX)
                });
            }
        }
    }
    if order.len() != nodes.len() {
        return Err("workflow graph contains a cycle and cannot execute".to_owned());
    }
    Ok(order)
}

struct PassMachine<'a> {
    compiled: &'a CompiledGraphPassV1,
    gateway: &'a FrozenModelGateway,
    tool_authority: &'a dyn ModelToolInvocationPortV1,
    outer_invocation_id: &'a StableId,
    request_id: &'a str,
    chat_id: &'a str,
    run_id: &'a str,
    model_binding_id: &'a str,
    model_version_hash: &'a str,
    budget: GraphPassBudgetV1,
    now_epoch_millis: u64,
    deadline_epoch_millis: u64,
    conversation: Vec<WorkflowMessageV1>,
    values: BTreeMap<String, Value>,
    completed: Vec<String>,
    executed: BTreeSet<String>,
    active_edges: BTreeSet<usize>,
    activity: Vec<GraphNodeActivityV1>,
    tool_activity: Vec<WorkflowToolActivityV1>,
    exchanges: Vec<ModelToolExchangeV1>,
    input_units: u64,
    output_units: u64,
    attempted_model_turns: u32,
    settled_tool_calls: u32,
    timeout_recoveries: u32,
    final_text: Option<String>,
    pending_tool_approval: Option<(GraphApprovalRequestV1, AgentLoopSuspensionV1)>,
    resume_agent_suspension: Option<(AgentLoopSuspensionV1, Option<bool>)>,
    activity_observer: Option<&'a dyn Fn(&GraphNodeActivityV1)>,
}

impl<'a> PassMachine<'a> {
    fn run(
        mut self,
        pending: Option<&PendingGraphPassStateV1>,
        approval_decision: Option<bool>,
        cancellation: &CancellationToken,
    ) -> GraphPassOutcomeV1 {
        if let Some(pending) = pending {
            self.values = pending.values.clone();
            self.completed = pending.completed.clone();
            self.executed = pending.completed.iter().cloned().collect();
            self.activity = pending.activity.clone();
            self.tool_activity = pending.tool_activity.clone();
            self.exchanges = pending.exchanges.clone();
            self.input_units = pending.input_units;
            self.output_units = pending.output_units;
            self.attempted_model_turns = pending.attempted_model_turns;
            self.settled_tool_calls = pending.settled_tool_calls;
            self.timeout_recoveries = pending.timeout_recoveries;
            self.conversation = pending.conversation.clone();
            self.resume_agent_suspension = pending
                .agent_loop
                .clone()
                .map(|suspension| (suspension, approval_decision));
        }
        for node_id in &self.compiled.topological_order {
            if self.executed.contains(node_id) {
                continue;
            }
            let Some(node) = self.compiled.nodes.iter().find(|node| &node.id == node_id) else {
                return self.failed_outcome(format!("compiled graph is missing node '{node_id}'"));
            };
            if !self.ready(node_id) {
                self.push_activity(node, "skipped", "branch not taken");
                continue;
            }
            if cancellation.is_cancelled() {
                return self.failed_outcome("graph pass was cancelled".to_owned());
            }
            if deadline_elapsed(self.deadline_epoch_millis) {
                return self.failed_outcome(format!(
                    "workflow Run deadline elapsed before node '{}' started",
                    node.id
                ));
            }
            if let Err(error) = self.check_budget() {
                return self.failed_outcome(error);
            }
            self.push_activity(node, "started", "running");
            let value = if node.node_type == "approval" {
                match approval_decision {
                    Some(true) => self.incoming_value(&node.id),
                    Some(false) => {
                        self.push_activity(node, "failed", "rejected by the user");
                        return self.failed_outcome(format!(
                            "approval '{}' was rejected by the user",
                            node.label
                        ));
                    }
                    None => {
                        self.push_activity(node, "waiting", "awaiting user decision");
                        return self.pending_approval(node);
                    }
                }
            } else if self
                .resume_agent_suspension
                .as_ref()
                .is_some_and(|(suspension, _)| suspension.node_id == node.id)
            {
                let (suspension, decision) = self
                    .resume_agent_suspension
                    .take()
                    .expect("resume suspension present");
                match self.resume_agent_after_approval(
                    node,
                    &suspension.pending,
                    decision.unwrap_or(false),
                    cancellation,
                ) {
                    AgentResumeOutcomeV1::Value(value) => value,
                    AgentResumeOutcomeV1::Suspended(approval, next_suspension) => {
                        self.push_activity(node, "waiting", "awaiting user decision");
                        return self.pending_for_tool_approval(approval, next_suspension);
                    }
                    AgentResumeOutcomeV1::Failed(error) => {
                        self.push_activity(node, "failed", &error);
                        return self.failed_outcome(error);
                    }
                }
            } else {
                match self.execute_node(node, cancellation) {
                    Ok(value) => value,
                    Err(error) => {
                        self.push_activity(node, "failed", &error);
                        return self.failed_outcome(error);
                    }
                }
            };
            if let Some((approval, suspension)) = self.pending_tool_approval.take() {
                self.push_activity(node, "waiting", "awaiting user decision");
                return self.pending_for_tool_approval(approval, suspension);
            }
            if let Err(error) = self.enforce_node_output_bound(&value, node) {
                self.push_activity(node, "failed", &error);
                return self.failed_outcome(error);
            }
            self.values.insert(node_id.clone(), value);
            self.executed.insert(node_id.clone());
            self.completed.push(node_id.clone());
            self.push_activity(node, "completed", node_completion_summary(node));
            if matches!(node.node_type.as_str(), "wait" | "completion") {
                return self.succeeded_outcome();
            }
        }
        self.succeeded_outcome()
    }

    fn pending_for_tool_approval(
        &self,
        approval: GraphApprovalRequestV1,
        suspension: AgentLoopSuspensionV1,
    ) -> GraphPassOutcomeV1 {
        let pending_state = PendingGraphPassStateV1 {
            schema_version: 1,
            decision_id: approval.decision_id.clone(),
            invocation_id: self.outer_invocation_id.to_string(),
            request_id: self.request_id.to_owned(),
            chat_id: self.chat_id.to_owned(),
            run_id: self.run_id.to_owned(),
            values: self.values.clone(),
            completed: self.completed.clone(),
            pending_node_id: suspension.node_id.clone(),
            title: approval.title.clone(),
            message: approval.message.clone(),
            agent_loop: Some(suspension),
            reasoning_body: None,
            reasoning_category: None,
            conversation: self.conversation.clone(),
            activity: self.activity.clone(),
            tool_activity: self.tool_activity.clone(),
            legacy_run_activity: Vec::new(),
            exchanges: self.exchanges.clone(),
            input_units: self.input_units,
            output_units: self.output_units,
            attempted_model_turns: self.attempted_model_turns,
            settled_tool_calls: self.settled_tool_calls,
            timeout_recoveries: self.timeout_recoveries,
        };
        GraphPassOutcomeV1 {
            status: GraphPassStatusV1::AwaitingApproval,
            assistant_text: None,
            error: None,
            approval: Some(approval),
            pending_state: Some(pending_state),
            activity: self.activity.clone(),
            tool_activity: self.tool_activity.clone(),
            exchanges: self.exchanges.clone(),
            input_units: self.input_units,
            output_units: self.output_units,
            attempted_model_turns: self.attempted_model_turns,
            settled_tool_calls: self.settled_tool_calls,
        }
    }

    fn execute_node(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        match node.node_type.as_str() {
            "input" => {
                let latest = self
                    .conversation
                    .iter()
                    .rev()
                    .find(|message| message.role == "user")
                    .map(|message| message.content.clone())
                    .unwrap_or_default();
                Ok(Value::String(latest))
            }
            "parallel" => Ok(self.incoming_value(&node.id)),
            "condition" => {
                let input = self.incoming_value(&node.id);
                let predicate = node
                    .configuration
                    .get("predicate")
                    .cloned()
                    .unwrap_or_else(|| json!({"kind": "always"}));
                let result = evaluate_predicate(&predicate, &input).map_err(|error| {
                    format!("condition node '{}' predicate failed: {error}", node.id)
                })?;
                let route = if result { "true" } else { "false" };
                for (index, edge) in self.compiled.edges.iter().enumerate() {
                    if edge.source == node.id {
                        let active = edge.route.as_deref() == Some(route);
                        if active {
                            self.active_edges.insert(index);
                        } else {
                            self.active_edges.remove(&index);
                        }
                    }
                }
                Ok(input)
            }
            "model_call" => self.run_model_call(node, cancellation),
            "agent" => self.run_agent(node, cancellation),
            "tool" => self.run_tool_node(node, cancellation),
            "approval" => Err(format!(
                "approval node '{}' requires an explicit decision",
                node.id
            )),
            "output" => {
                let text = value_text(&self.incoming_value(&node.id));
                self.final_text = Some(text.clone());
                Ok(Value::String(text))
            }
            "wait" | "completion" => Ok(self.incoming_value(&node.id)),
            other => Err(format!("node '{}' has unsupported type '{other}'", node.id)),
        }
    }

    fn ready(&self, node_id: &str) -> bool {
        let incoming: Vec<usize> = self
            .compiled
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| edge.target == node_id)
            .map(|(index, _)| index)
            .collect();
        if incoming.is_empty() {
            return node_id == self.compiled.entry_node_id;
        }
        let mut any_active = false;
        for index in incoming {
            let edge = &self.compiled.edges[index];
            if !self.active_edges.contains(&index) {
                continue;
            }
            any_active = true;
            if !self.executed.contains(&edge.source) {
                return false;
            }
        }
        any_active
    }

    fn check_budget(&self) -> Result<(), String> {
        let actions = self
            .attempted_model_turns
            .saturating_add(self.settled_tool_calls);
        if u64::from(self.attempted_model_turns) > self.budget.turns
            || u64::from(self.settled_tool_calls) > self.budget.tool_calls
            || u64::from(actions) > self.budget.actions
            || self.input_units.saturating_add(self.output_units) > self.budget.tokens
        {
            return Err("graph pass budget is exhausted".to_owned());
        }
        Ok(())
    }

    fn push_activity(&mut self, node: &CompiledGraphNodeV1, status: &str, summary: &str) {
        let input = (status == "started").then(|| self.node_input(node));
        let output = match status {
            "completed" => self.values.get(&node.id).cloned(),
            "failed" => Some(Value::String(summary.to_owned())),
            _ => None,
        };
        let activity = GraphNodeActivityV1 {
            node_id: node.id.clone(),
            node_type: node.node_type.clone(),
            label: node.label.clone(),
            status: status.to_owned(),
            summary: summary.to_owned(),
            input,
            output,
        };
        if let Some(observer) = self.activity_observer {
            observer(&activity);
        }
        self.activity.push(activity);
    }

    fn node_input(&self, node: &CompiledGraphNodeV1) -> Value {
        if node.node_type == "input" {
            return self
                .conversation
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| Value::String(message.content.clone()))
                .unwrap_or(Value::Null);
        }
        self.incoming_value(&node.id)
    }

    fn enforce_node_output_bound(
        &self,
        value: &Value,
        node: &CompiledGraphNodeV1,
    ) -> Result<(), String> {
        if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > MAXIMUM_NODE_OUTPUT_BYTES) {
            return Err(format!(
                "node '{}' output exceeds the {} KiB pass bound",
                node.id,
                MAXIMUM_NODE_OUTPUT_BYTES / 1024
            ));
        }
        Ok(())
    }

    fn incoming_value(&self, node_id: &str) -> Value {
        for (index, edge) in self.compiled.edges.iter().enumerate() {
            if edge.target == node_id
                && self.active_edges.contains(&index)
                && self.executed.contains(&edge.source)
            {
                return self
                    .values
                    .get(&edge.source)
                    .cloned()
                    .unwrap_or(Value::Null);
            }
        }
        Value::Null
    }

    /// Direct input-node text is already present in the frozen conversation.
    /// Only outputs from intervening graph steps become additional Agent
    /// context, preventing the user message from being duplicated.
    fn incoming_agent_context(&self, node_id: &str) -> Value {
        for (index, edge) in self.compiled.edges.iter().enumerate() {
            if edge.target != node_id
                || !self.active_edges.contains(&index)
                || !self.executed.contains(&edge.source)
            {
                continue;
            }
            let source_is_input = self
                .compiled
                .nodes
                .iter()
                .any(|node| node.id == edge.source && node.node_type == "input");
            if source_is_input {
                return Value::Null;
            }
            return self
                .values
                .get(&edge.source)
                .cloned()
                .unwrap_or(Value::Null);
        }
        Value::Null
    }

    fn run_model_call(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let instructions = node
            .configuration
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or("");
        let context_text = value_text(&self.incoming_value(&node.id));
        let mut messages = Vec::new();
        if !instructions.trim().is_empty() {
            messages.push(WorkflowMessageV1 {
                role: "system".into(),
                content: instructions.to_owned(),
            });
        }
        messages.push(WorkflowMessageV1 {
            role: "user".into(),
            content: context_text,
        });
        let input = json!({"messages": messages});
        let plan = ModelResolutionPlanV1 {
            candidates: vec![ModelCandidateV1 {
                binding_id: self.model_binding_id.to_owned(),
                version_hash: self.model_version_hash.to_owned(),
            }],
            maximum_input_bytes: MAXIMUM_MODEL_CALL_INPUT_BYTES,
            maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
        };
        let parameters = node_model_parameters(&node.configuration);
        match self.execute_text_turn(&plan, ModelRequestV1 { input, parameters }, cancellation) {
            Ok(evidence) => {
                if deadline_elapsed(self.deadline_epoch_millis) {
                    return Err(format!(
                        "workflow Run deadline elapsed during model_call node '{}' provider turn",
                        node.id
                    ));
                }
                let turn = project_model_events(&evidence.events);
                let text = turn.assistant_text;
                let units = (turn.input_tokens, turn.output_tokens);
                if text.trim().is_empty() {
                    return Err(format!(
                        "model_call node '{}' returned no assistant text",
                        node.id
                    ));
                }
                self.input_units = self.input_units.saturating_add(units.0);
                self.output_units = self.output_units.saturating_add(units.1);
                if node
                    .configuration
                    .get("outputContract")
                    .and_then(Value::as_str)
                    == Some("plan")
                {
                    parse_plan_output_v1(&text).map_err(|error| {
                        format!(
                            "model_call node '{}' violated its plan output contract: {error}",
                            node.id
                        )
                    })
                } else {
                    Ok(Value::String(text))
                }
            }
            Err(error) => Err(format!("model_call node '{}' failed: {error}", node.id)),
        }
    }

    fn run_agent(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let instructions = node
            .configuration
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or("");
        let upstream = value_text(&self.incoming_agent_context(&node.id));
        let mut messages = Vec::new();
        if !instructions.trim().is_empty() || !upstream.trim().is_empty() {
            let mut system = String::new();
            if !instructions.trim().is_empty() {
                system.push_str(instructions);
            }
            if !upstream.trim().is_empty() {
                if !system.is_empty() {
                    system.push_str("\n\nAdditional context from earlier graph steps:\n");
                }
                system.push_str(&truncate_utf8(upstream, MAXIMUM_AGENT_CONTEXT_BYTES));
            }
            messages.push(WorkflowMessageV1 {
                role: "system".into(),
                content: system,
            });
        }
        messages.extend(self.conversation.iter().cloned());
        let context = json!({"messages": messages});
        let definitions = node
            .tool_bindings
            .iter()
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let parameters = node_model_parameters(&node.configuration);
        if definitions.is_empty() {
            return match self.execute_text_turn(
                &ModelResolutionPlanV1 {
                    candidates: vec![ModelCandidateV1 {
                        binding_id: self.model_binding_id.to_owned(),
                        version_hash: self.model_version_hash.to_owned(),
                    }],
                    maximum_input_bytes: WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES,
                    maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
                },
                ModelRequestV1 {
                    input: context,
                    parameters,
                },
                cancellation,
            ) {
                Ok(evidence) => {
                    if deadline_elapsed(self.deadline_epoch_millis) {
                        return Err(format!(
                            "workflow Run deadline elapsed during agent node '{}' provider turn",
                            node.id
                        ));
                    }
                    let turn = project_model_events(&evidence.events);
                    let text = turn.assistant_text;
                    let units = (turn.input_tokens, turn.output_tokens);
                    self.input_units = self.input_units.saturating_add(units.0);
                    self.output_units = self.output_units.saturating_add(units.1);
                    if text.trim().is_empty() {
                        Err(format!(
                            "agent node '{}' returned no assistant text",
                            node.id
                        ))
                    } else {
                        Ok(Value::String(text))
                    }
                }
                Err(error) => Err(format!("agent node '{}' failed: {error}", node.id)),
            };
        }
        let maximum_turns = if (2..=MAXIMUM_AGENT_TURNS_WITH_TOOLS).contains(&node.maximum_turns) {
            node.maximum_turns
        } else if node.maximum_turns == 1 {
            2
        } else {
            return Err(format!(
                "agent node '{}' maxTurns {} is outside the 1..={MAXIMUM_AGENT_TURNS_WITH_TOOLS} bound",
                node.id, node.maximum_turns
            ));
        };
        match execute_model_tool_loop_approval_v1(
            self.gateway,
            ModelToolLoopRequestV1 {
                outer_invocation_id: self.outer_invocation_id,
                input: context,
                parameters,
                definitions,
                binding_id: self.model_binding_id.to_owned(),
                binding_version_hash: self.model_version_hash.to_owned(),
                maximum_input_bytes: WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES,
                maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
                maximum_tool_output_bytes: self.budget.maximum_tool_output_bytes,
                maximum_turns,
                maximum_timeout_recoveries: self
                    .budget
                    .maximum_timeout_recoveries
                    .saturating_sub(self.timeout_recoveries),
                maximum_tool_calls: u32::try_from(self.budget.tool_calls).unwrap_or(u32::MAX),
                maximum_tokens: self.budget.tokens,
                deadline_epoch_millis: self.deadline_epoch_millis,
            },
            self.tool_authority,
            cancellation,
        ) {
            Ok(ModelToolLoopRunV1::Completed(completed)) => {
                self.attempted_model_turns = self
                    .attempted_model_turns
                    .saturating_add(completed.attempted_model_turns);
                self.settled_tool_calls = self
                    .settled_tool_calls
                    .saturating_add(completed.settled_tool_calls);
                self.input_units = self.input_units.saturating_add(completed.input_tokens);
                self.output_units = self.output_units.saturating_add(completed.output_tokens);
                self.timeout_recoveries = self
                    .timeout_recoveries
                    .saturating_add(completed.timeout_recoveries);
                self.exchanges.extend(completed.exchanges);
                self.tool_activity.extend(completed.activities);
                Ok(Value::String(completed.assistant_text))
            }
            Ok(ModelToolLoopRunV1::Suspended { challenge, pending }) => {
                self.timeout_recoveries = self
                    .timeout_recoveries
                    .saturating_add(pending.timeout_recoveries);
                let approval = tool_approval_request(&challenge, &node.id);
                self.pending_tool_approval = Some((
                    approval,
                    AgentLoopSuspensionV1 {
                        node_id: node.id.clone(),
                        pending,
                    },
                ));
                // The caller inspects pending_tool_approval before using this
                // placeholder value.
                Ok(Value::Null)
            }
            Err(failure) => {
                self.attempted_model_turns = self
                    .attempted_model_turns
                    .saturating_add(failure.attempted_model_turns);
                self.settled_tool_calls = self
                    .settled_tool_calls
                    .saturating_add(failure.settled_tool_calls);
                self.input_units = self.input_units.saturating_add(failure.input_tokens);
                self.output_units = self.output_units.saturating_add(failure.output_tokens);
                self.exchanges.extend(failure.exchanges);
                self.tool_activity.extend(failure.activities);
                Err(format!(
                    "agent node '{}' failed: {}",
                    node.id, failure.error
                ))
            }
        }
    }

    fn execute_text_turn(
        &mut self,
        plan: &ModelResolutionPlanV1,
        mut request: ModelRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<ModelDispatchEvidenceV1, ProviderError> {
        loop {
            if deadline_elapsed(self.deadline_epoch_millis) {
                return Err(ProviderError::Failed(
                    "workflow Run deadline elapsed before provider retry".to_owned(),
                ));
            }
            self.attempted_model_turns = self.attempted_model_turns.saturating_add(1);
            match self
                .gateway
                .execute_cancellable(plan, &request, cancellation)
            {
                Err(ProviderError::RequestTimedOut)
                    if self.timeout_recoveries < self.budget.maximum_timeout_recoveries =>
                {
                    self.timeout_recoveries = self.timeout_recoveries.saturating_add(1);
                    append_retry_notice(&mut request.input)?;
                }
                result => return result,
            }
        }
    }

    /// Resumes a tool-approval suspension inside the agent node: the exact
    /// original call is settled with the decision and the loop continues.
    fn resume_agent_after_approval(
        &mut self,
        node: &CompiledGraphNodeV1,
        pending: &ModelToolLoopPendingV1,
        approved: bool,
        cancellation: &CancellationToken,
    ) -> AgentResumeOutcomeV1 {
        let instructions = node
            .configuration
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or("");
        let upstream = value_text(&self.incoming_agent_context(&node.id));
        let mut messages = Vec::new();
        if !instructions.trim().is_empty() || !upstream.trim().is_empty() {
            let mut system = String::new();
            if !instructions.trim().is_empty() {
                system.push_str(instructions);
            }
            if !upstream.trim().is_empty() {
                if !system.is_empty() {
                    system.push_str("\n\nAdditional context from earlier graph steps:\n");
                }
                system.push_str(&truncate_utf8(upstream, MAXIMUM_AGENT_CONTEXT_BYTES));
            }
            messages.push(WorkflowMessageV1 {
                role: "system".into(),
                content: system,
            });
        }
        messages.extend(self.conversation.iter().cloned());
        let context = json!({"messages": messages});
        let definitions = node
            .tool_bindings
            .iter()
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let maximum_turns = if (2..=MAXIMUM_AGENT_TURNS_WITH_TOOLS).contains(&node.maximum_turns) {
            node.maximum_turns
        } else if node.maximum_turns == 1 {
            2
        } else {
            return AgentResumeOutcomeV1::Failed(format!(
                "agent node '{}' maxTurns {} is outside the 1..={MAXIMUM_AGENT_TURNS_WITH_TOOLS} bound",
                node.id, node.maximum_turns
            ));
        };
        let request = ModelToolLoopRequestV1 {
            outer_invocation_id: self.outer_invocation_id,
            input: context,
            parameters: node_model_parameters(&node.configuration),
            definitions,
            binding_id: self.model_binding_id.to_owned(),
            binding_version_hash: self.model_version_hash.to_owned(),
            maximum_input_bytes: WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES,
            maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
            maximum_tool_output_bytes: self.budget.maximum_tool_output_bytes,
            maximum_turns,
            maximum_timeout_recoveries: self
                .budget
                .maximum_timeout_recoveries
                .saturating_sub(self.timeout_recoveries),
            maximum_tool_calls: u32::try_from(self.budget.tool_calls).unwrap_or(u32::MAX),
            maximum_tokens: self.budget.tokens,
            deadline_epoch_millis: self.deadline_epoch_millis,
        };
        match resume_model_tool_loop_v1(
            self.gateway,
            request,
            self.tool_authority,
            pending,
            approved,
            self.now_epoch_millis,
            cancellation,
        ) {
            Ok(ModelToolLoopRunV1::Completed(completed)) => {
                self.attempted_model_turns = self
                    .attempted_model_turns
                    .saturating_add(completed.attempted_model_turns);
                self.settled_tool_calls = self
                    .settled_tool_calls
                    .saturating_add(completed.settled_tool_calls);
                self.input_units = self.input_units.saturating_add(completed.input_tokens);
                self.output_units = self.output_units.saturating_add(completed.output_tokens);
                self.timeout_recoveries = self
                    .timeout_recoveries
                    .saturating_add(completed.timeout_recoveries);
                self.exchanges.extend(completed.exchanges);
                self.tool_activity.extend(completed.activities);
                AgentResumeOutcomeV1::Value(Value::String(completed.assistant_text))
            }
            Ok(ModelToolLoopRunV1::Suspended { challenge, pending }) => {
                self.timeout_recoveries = self
                    .timeout_recoveries
                    .saturating_add(pending.timeout_recoveries);
                let approval = tool_approval_request(&challenge, &node.id);
                AgentResumeOutcomeV1::Suspended(
                    approval,
                    AgentLoopSuspensionV1 {
                        node_id: node.id.clone(),
                        pending,
                    },
                )
            }
            Err(failure) => {
                self.attempted_model_turns = self
                    .attempted_model_turns
                    .saturating_add(failure.attempted_model_turns);
                self.settled_tool_calls = self
                    .settled_tool_calls
                    .saturating_add(failure.settled_tool_calls);
                self.input_units = self.input_units.saturating_add(failure.input_tokens);
                self.output_units = self.output_units.saturating_add(failure.output_tokens);
                self.exchanges.extend(failure.exchanges);
                self.tool_activity.extend(failure.activities);
                AgentResumeOutcomeV1::Failed(format!(
                    "agent node '{}' failed: {}",
                    node.id, failure.error
                ))
            }
        }
    }

    fn run_tool_node(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let binding = node
            .tool_bindings
            .first()
            .ok_or_else(|| format!("tool node '{}' has no binding", node.id))?;
        let upstream = value_text(&self.incoming_value(&node.id));
        let mut arguments = node
            .configuration
            .get("parameters")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for value in arguments.values_mut() {
            if let Value::String(text) = value
                && text.contains("{input}")
            {
                *text = text.replace("{input}", &upstream);
            }
        }
        let call = ModelToolCallV1 {
            call_id: format!("{}.tool", node.id),
            provider_call_id: None,
            capability_id: binding.capability_id.clone(),
            name: binding.provider_name.clone(),
            arguments: Value::Object(arguments),
            provider_context: None,
        };
        match self
            .tool_authority
            .invoke(self.outer_invocation_id, 0, &call, cancellation)
        {
            Ok(settled) => {
                self.settled_tool_calls = self.settled_tool_calls.saturating_add(1);
                self.tool_activity.push(settled.activity);
                Ok(settled.result.content)
            }
            Err(error) => Err(format!("tool node '{}' failed: {error}", node.id)),
        }
    }

    fn pending_approval(&self, node: &CompiledGraphNodeV1) -> GraphPassOutcomeV1 {
        let decision_id = approval_decision_id(
            self.outer_invocation_id,
            &node.id,
            u64::try_from(self.completed.len()).unwrap_or(u64::MAX),
        );
        let title = node
            .configuration
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("Workflow approval required")
            .to_owned();
        let message = node
            .configuration
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or("The workflow reached an approval gate. Approve to continue the run.")
            .to_owned();
        let approval = GraphApprovalRequestV1 {
            decision_id: decision_id.clone(),
            node_id: node.id.clone(),
            title: title.clone(),
            message: message.clone(),
        };
        let pending_state = PendingGraphPassStateV1 {
            schema_version: 1,
            decision_id,
            invocation_id: self.outer_invocation_id.to_string(),
            request_id: self.request_id.to_owned(),
            chat_id: self.chat_id.to_owned(),
            run_id: self.run_id.to_owned(),
            values: self.values.clone(),
            completed: self.completed.clone(),
            pending_node_id: node.id.clone(),
            title,
            message,
            agent_loop: None,
            reasoning_body: None,
            reasoning_category: None,
            conversation: self.conversation.clone(),
            activity: self.activity.clone(),
            tool_activity: self.tool_activity.clone(),
            legacy_run_activity: Vec::new(),
            exchanges: self.exchanges.clone(),
            input_units: self.input_units,
            output_units: self.output_units,
            attempted_model_turns: self.attempted_model_turns,
            settled_tool_calls: self.settled_tool_calls,
            timeout_recoveries: self.timeout_recoveries,
        };
        GraphPassOutcomeV1 {
            status: GraphPassStatusV1::AwaitingApproval,
            assistant_text: None,
            error: None,
            approval: Some(approval),
            pending_state: Some(pending_state),
            activity: self.activity.clone(),
            tool_activity: self.tool_activity.clone(),
            exchanges: self.exchanges.clone(),
            input_units: self.input_units,
            output_units: self.output_units,
            attempted_model_turns: self.attempted_model_turns,
            settled_tool_calls: self.settled_tool_calls,
        }
    }

    fn failed_outcome(&self, error: String) -> GraphPassOutcomeV1 {
        GraphPassOutcomeV1 {
            status: GraphPassStatusV1::Failed,
            assistant_text: None,
            error: Some(error),
            approval: None,
            pending_state: None,
            activity: self.activity.clone(),
            tool_activity: self.tool_activity.clone(),
            exchanges: self.exchanges.clone(),
            input_units: self.input_units,
            output_units: self.output_units,
            attempted_model_turns: self.attempted_model_turns,
            settled_tool_calls: self.settled_tool_calls,
        }
    }

    fn succeeded_outcome(&self) -> GraphPassOutcomeV1 {
        GraphPassOutcomeV1 {
            status: GraphPassStatusV1::Succeeded,
            assistant_text: self.final_text.clone(),
            error: None,
            approval: None,
            pending_state: None,
            activity: self.activity.clone(),
            tool_activity: self.tool_activity.clone(),
            exchanges: self.exchanges.clone(),
            input_units: self.input_units,
            output_units: self.output_units,
            attempted_model_turns: self.attempted_model_turns,
            settled_tool_calls: self.settled_tool_calls,
        }
    }
}

/// Executes one graph pass while projecting each node transition as it occurs.
/// Approval gates and PerInvocation tool approvals suspend with a durable
/// prefix; callers persist `pending_state`, then resume with `pending` plus
/// `approval_decision`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_graph_pass_observed(
    compiled: &CompiledGraphPassV1,
    conversation: &[WorkflowMessageV1],
    budget: GraphPassBudgetV1,
    gateway: &FrozenModelGateway,
    tool_authority: &dyn ModelToolInvocationPortV1,
    outer_invocation_id: &StableId,
    request_id: &str,
    chat_id: &str,
    run_id: &str,
    model_binding_id: &str,
    model_version_hash: &str,
    now_epoch_millis: u64,
    deadline_epoch_millis: u64,
    pending: Option<&PendingGraphPassStateV1>,
    approval_decision: Option<bool>,
    cancellation: &CancellationToken,
    activity_observer: Option<&dyn Fn(&GraphNodeActivityV1)>,
) -> GraphPassOutcomeV1 {
    let machine = PassMachine {
        compiled,
        gateway,
        tool_authority,
        outer_invocation_id,
        request_id,
        chat_id,
        run_id,
        model_binding_id,
        model_version_hash,
        budget,
        now_epoch_millis,
        deadline_epoch_millis,
        conversation: conversation.to_vec(),
        values: BTreeMap::new(),
        completed: Vec::new(),
        executed: BTreeSet::new(),
        active_edges: (0..compiled.edges.len()).collect(),
        activity: Vec::new(),
        tool_activity: Vec::new(),
        exchanges: Vec::new(),
        input_units: 0,
        output_units: 0,
        attempted_model_turns: 0,
        settled_tool_calls: 0,
        timeout_recoveries: 0,
        final_text: None,
        pending_tool_approval: None,
        resume_agent_suspension: None,
        activity_observer,
    };
    machine.run(pending, approval_decision, cancellation)
}

fn deadline_elapsed(deadline_epoch_millis: u64) -> bool {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(true, |duration| {
            u64::try_from(duration.as_millis()).map_or(true, |now| now >= deadline_epoch_millis)
        })
}

fn append_retry_notice(input: &mut Value) -> Result<(), ProviderError> {
    let notice = json!({"role":"user","content":PROVIDER_TIMEOUT_NOTICE});
    match input {
        Value::String(text) => {
            *input = json!({"messages":[
                {"role":"user","content":text.clone()},
                notice,
            ]});
        }
        Value::Array(messages) => messages.push(notice),
        Value::Object(object) if object.contains_key("messages") => object
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or(ProviderError::InvalidPlan)?
            .push(notice),
        Value::Object(object) if object.contains_key("role") && object.contains_key("content") => {
            *input = Value::Array(vec![Value::Object(object.clone()), notice]);
        }
        _ => return Err(ProviderError::InvalidPlan),
    }
    Ok(())
}

/// Extracts the closed request overrides owned by a model-consuming workflow
/// node. Null means "inherit the concrete model default" and is omitted.
fn node_model_parameters(configuration: &Value) -> BTreeMap<String, Value> {
    ["reasoningEffort", "enableThinking"]
        .into_iter()
        .filter_map(|key| {
            configuration
                .get(key)
                .filter(|value| !value.is_null())
                .cloned()
                .map(|value| (key.to_owned(), value))
        })
        .collect()
}

enum AgentResumeOutcomeV1 {
    Value(Value),
    Suspended(GraphApprovalRequestV1, AgentLoopSuspensionV1),
    Failed(String),
}

fn tool_approval_request(
    challenge: &ToolApprovalChallengeV1,
    node_id: &str,
) -> GraphApprovalRequestV1 {
    GraphApprovalRequestV1 {
        decision_id: challenge.decision_id.clone(),
        node_id: node_id.to_owned(),
        title: if challenge.title.trim().is_empty() {
            format!("Allow tool {}?", challenge.capability_id)
        } else {
            challenge.title.clone()
        },
        message: challenge.summary.clone(),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> String {
    while value.len() > maximum_bytes {
        let mut boundary = maximum_bytes;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
        value.push('…');
        if value.len() <= maximum_bytes {
            break;
        }
        value.pop();
    }
    value
}

fn approval_decision_id(invocation_id: &StableId, node_id: &str, ordinal: u64) -> String {
    let digest = Sha256::digest(
        serde_json::to_vec(&(invocation_id.as_str(), node_id, ordinal)).unwrap_or_default(),
    );
    format!("approval.{}", hex(&digest)[..40].to_owned())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn evaluate_predicate(predicate: &Value, input: &Value) -> Result<bool, String> {
    let object = predicate
        .as_object()
        .ok_or_else(|| "predicate must be an object".to_owned())?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "predicate requires a kind".to_owned())?;
    match kind {
        "always" => Ok(true),
        "exists" => {
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "exists predicate requires a path".to_owned())?;
            Ok(!lookup_path(input, path).is_null())
        }
        "eq" | "neq" => {
            let expected = object
                .get("value")
                .ok_or_else(|| format!("{kind} predicate requires a value"))?;
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("value");
            let actual = lookup_path(input, path);
            let equal = if actual.is_null() {
                expected.is_null()
            } else {
                actual == *expected
            };
            Ok(if kind == "eq" { equal } else { !equal })
        }
        "and" | "or" => {
            let operands = object
                .get("operands")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{kind} predicate requires operands"))?;
            for operand in operands {
                let result = evaluate_predicate(operand, input)?;
                if kind == "and" && !result {
                    return Ok(false);
                }
                if kind == "or" && result {
                    return Ok(true);
                }
            }
            Ok(kind == "and")
        }
        "not" => {
            let operand = object
                .get("operand")
                .ok_or_else(|| "not predicate requires an operand".to_owned())?;
            evaluate_predicate(operand, input).map(|result| !result)
        }
        other => Err(format!("unsupported predicate kind '{other}'")),
    }
}

/// Resolves a dotted path like "text.length" inside a JSON value. Arrays index
/// numerically; missing segments yield null. The reserved path "value" selects
/// the whole scalar input so string conditions can compare directly.
fn lookup_path(value: &Value, path: &str) -> Value {
    if path == "value" && !value.is_object() && !value.is_array() {
        return value.clone();
    }
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            continue;
        }
        current = match current {
            Value::Object(fields) => fields.get(segment).unwrap_or(&Value::Null),
            Value::Array(items) => segment
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
                .unwrap_or(&Value::Null),
            _ => return Value::Null,
        };
    }
    current.clone()
}

fn node_completion_summary(node: &CompiledGraphNodeV1) -> &'static str {
    match node.node_type.as_str() {
        "output" => "Response prepared.",
        "wait" => "Ready for another message.",
        "completion" => "Workflow completed.",
        _ => "settled",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        evaluate_predicate, node_completion_summary, node_model_parameters, topological_order,
        value_text,
    };
    use crate::runtime::graph_pass::{CompiledGraphEdgeV1, CompiledGraphNodeV1};

    fn node(id: &str, node_type: &str) -> CompiledGraphNodeV1 {
        CompiledGraphNodeV1 {
            id: id.into(),
            node_type: node_type.into(),
            label: id.into(),
            configuration: json!({}),
            tool_bindings: Vec::new(),
            maximum_turns: 1,
        }
    }

    fn edge(source: &str, target: &str, route: Option<&str>) -> CompiledGraphEdgeV1 {
        CompiledGraphEdgeV1 {
            source: source.into(),
            target: target.into(),
            route: route.map(str::to_owned),
        }
    }

    #[test]
    fn predicates_evaluate_over_scalar_and_structured_values() {
        let text = json!("plan complete");
        assert!(evaluate_predicate(&json!({"kind":"always"}), &text).unwrap());
        assert!(evaluate_predicate(&json!({"kind":"exists","path":"value"}), &text).unwrap());
        assert!(
            evaluate_predicate(
                &json!({"kind":"eq","path":"value","value":"plan complete"}),
                &text
            )
            .unwrap()
        );
        assert!(
            !evaluate_predicate(&json!({"kind":"eq","path":"value","value":"other"}), &text)
                .unwrap()
        );
        assert!(
            evaluate_predicate(&json!({"kind":"neq","path":"value","value":"other"}), &text)
                .unwrap()
        );
        let structured = json!({"text": "hello", "count": 3});
        assert!(
            evaluate_predicate(&json!({"kind":"eq","path":"count","value":3}), &structured)
                .unwrap()
        );
        assert!(
            evaluate_predicate(
                &json!({
                    "kind": "and",
                    "operands": [
                        {"kind":"eq","path":"text","value":"hello"},
                        {"kind":"neq","path":"count","value":4}
                    ]
                }),
                &structured,
            )
            .unwrap()
        );
        assert!(
            evaluate_predicate(
                &json!({"kind":"not","operand":{"kind":"eq","path":"text","value":"bye"}}),
                &structured,
            )
            .unwrap()
        );
    }

    #[test]
    fn topological_order_is_deterministic_and_document_ordered() {
        let nodes = vec![
            node("input.1", "input"),
            node("plan.1", "model_call"),
            node("agent.1", "agent"),
            node("wait.1", "wait"),
        ];
        let edges = vec![
            edge("input.1", "plan.1", None),
            edge("plan.1", "agent.1", None),
            edge("agent.1", "wait.1", None),
        ];
        assert_eq!(
            topological_order(&nodes, &edges).unwrap(),
            vec!["input.1", "plan.1", "agent.1", "wait.1"]
        );
        let mut cyclic = edges.clone();
        cyclic.push(edge("wait.1", "input.1", None));
        assert!(
            topological_order(&nodes, &cyclic)
                .unwrap_err()
                .contains("cycle")
        );
    }

    #[test]
    fn value_text_stringifies_results() {
        assert_eq!(value_text(&json!("hello")), "hello");
        assert_eq!(value_text(&json!({"a":1})), r#"{"a":1}"#);
        assert_eq!(value_text(&json!(null)), "");
    }

    #[test]
    fn terminal_control_nodes_have_user_facing_completion_summaries() {
        assert_eq!(
            node_completion_summary(&node("output.1", "output")),
            "Response prepared."
        );
        assert_eq!(
            node_completion_summary(&node("wait.1", "wait")),
            "Ready for another message."
        );
        assert_eq!(
            node_completion_summary(&node("completion.1", "completion")),
            "Workflow completed."
        );
    }

    #[test]
    fn model_node_parameters_keep_explicit_overrides_and_omit_inherited_values() {
        assert_eq!(
            node_model_parameters(&json!({
                "reasoningEffort": "xhigh",
                "enableThinking": false,
                "instructions": "ignored"
            })),
            std::collections::BTreeMap::from([
                ("enableThinking".into(), json!(false)),
                ("reasoningEffort".into(), json!("xhigh")),
            ])
        );
        assert!(
            node_model_parameters(&json!({
                "reasoningEffort": null,
                "enableThinking": null
            }))
            .is_empty()
        );
    }
}
