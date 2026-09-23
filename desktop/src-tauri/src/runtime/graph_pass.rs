//! Generic v1 workflow-graph pass executor.
//!
//! One graph pass runs per user input on the frozen workflow document. Nodes
//! execute in deterministic topological order with implicit joins (a node runs
//! once every active predecessor settles), conditions route true/false, and the
//! pass ends at a wait or completion node. The agent node owns the standard
//! bounded model/tool loop; model_call nodes make no-tool completions with one
//! format-correction attempt for invalid structured Plan output;
//! tool nodes settle exactly one bound capability invocation through the same
//! durable authority used by the agent loop. An approval node suspends the pass
//! with a durably restorable prefix so a later decision resumes without
//! recomputing completed model work.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_capability_host::{
    CancellationToken, FrozenModelGateway, ModelCandidateV1, ModelDispatchEvidenceV1,
    ModelRequestV1, ModelResolutionPlanV1, ModelToolCallV1, ModelToolExchangeV1, ProviderError,
    project_model_events,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod context;
mod model_call;
mod steering;
pub(crate) use steering::StoppedGraphPassV1;

use super::{
    documents::{analyze_loop_regions, validate_v1_executable_catalog},
    model_tool_loop::{
        AgentContextV1, ModelToolInvocationPortV1, ModelToolLoopPendingV1, ModelToolLoopRequestV1,
        ModelToolLoopRunV1, execute_model_tool_loop_approval_v1, is_context_overflow,
        provider_recovery_notice, resume_model_tool_loop_v1,
    },
    pipeline::{MAXIMUM_PROVIDER_REQUEST_BYTES, WorkflowMessageV1},
    tool_loop::{
        StoredFileToolBindingV1, ToolApprovalChallengeV1, WorkflowToolActivityV1, question,
    },
};

pub(crate) const MAXIMUM_GRAPH_NODES: usize = 64;
const MAXIMUM_NODE_OUTPUT_BYTES: usize = usize::MAX;
// Node context, model-call input and Agent request bounds are the pipeline's
// runaway guard, not design limits. The model's context window remains the only
// budget that governs a turn; oversize is reduced and retried, never fatal.
const MAXIMUM_AGENT_CONTEXT_BYTES: usize = MAXIMUM_PROVIDER_REQUEST_BYTES;
const MAXIMUM_MODEL_CALL_INPUT_BYTES: usize = MAXIMUM_PROVIDER_REQUEST_BYTES;
/// How many provider failures one text-only node reports to its model before the
/// node surfaces the provider as unreachable. Every failure short of this is the
/// model's next turn, so an ordinary repeated failure never ends the node.
const MAXIMUM_TEXT_ERROR_RECOVERIES: u32 = 32;

fn agent_context(node: &CompiledGraphNodeV1) -> AgentContextV1 {
    AgentContextV1 {
        node_id: node.id.clone(),
        tool_ids: node
            .tool_bindings
            .iter()
            .map(|b| b.capability_id.clone())
            .collect(),
        child: None,
        compaction: super::compaction::Overlay::from_node_configuration(&node.configuration),
    }
}

/// New automatic-context Agents isolate their durable tool exchanges by node.
/// Keep the historical invocation identity for pre-existing frozen workflows.
fn instruction_agent_outer(
    outer: &StableId,
    node: &CompiledGraphNodeV1,
    legacy: bool,
    iteration: Option<u32>,
) -> StableId {
    if legacy && iteration.is_none() && node.tool_bindings.iter().all(|b| b.is_callable()) {
        return outer.clone();
    }
    let identity = match iteration {
        // Each iteration is its own invocation: its context lineage, tool
        // exchanges and checkpoint are new immutable revisions rather than
        // shared mutable state.
        Some(iteration) => format!("{outer}:{}#{}", node.id, iteration),
        None => format!("{outer}:{}", node.id),
    };
    StableId::parse(format!(
        "invocation.agent.{:x}",
        Sha256::digest(identity.as_bytes())
    ))
    .expect("digest is a valid invocation identity")
}

/// Per-node transport recovery and tool-output bounds. Cumulative token usage
/// is reported for the run and never terminates its execution.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphPassBudgetV1 {
    pub maximum_timeout_recoveries: u32,
    pub maximum_tool_output_bytes: usize,
}

/// The bounded-loop frame one activity ran in: which loop, which iteration, and
/// the bound the limits controller charges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphLoopFrameV1 {
    pub header_id: String,
    pub iteration: u32,
    pub maximum_iterations: u32,
}

/// What running one node produced for the pass.
enum NodeStep {
    /// The node settled and its value is available to successors.
    Value(Value),
    /// There was nothing to do: the node already ran this pass, or its branch
    /// was not taken.
    Skipped,
    /// The pass ends with this outcome.
    Done(GraphPassOutcomeV1),
}

/// The three frozen routes a loop header declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopRouteV1 {
    Body,
    Exit,
    Fallback,
}

/// Durable progress of one active loop iteration. A suspension inside a region
/// resumes exactly this iteration instead of replaying settled work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LoopFrameStateV1 {
    pub header_id: String,
    pub iteration: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_this_iteration: Vec<String>,
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
    /// Declared-loop frame this activation belongs to, when the node ran inside
    /// one. Repeated iterations of the same node stay distinguishable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_frame: Option<GraphLoopFrameV1>,
}

/// The one durable suspension a graph pass reports.
///
/// A pass stops for exactly one reason at a time: an authority decision the
/// user must make, or a question the model asked. A question reuses the whole
/// approval suspension — identity, owner scope, resume nonce and no-replay
/// frontier — and only replaces the decision payload with the question.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphApprovalRequestV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<super::approvals::FilesystemApprovalRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<String>,
    pub decision_id: String,
    pub node_id: String,
    pub title: String,
    pub message: String,
    /// Set when this suspension is the model asking the user a question rather
    /// than requesting an authority decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<question::QuestionChallengeV1>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_edges: Option<BTreeSet<usize>>,
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
    /// Active loop frames, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loop_frames: Vec<LoopFrameStateV1>,
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
    AwaitingAnswer,
}

#[derive(Clone, Debug)]
pub(crate) struct GraphPassOutcomeV1 {
    pub status: GraphPassStatusV1,
    pub assistant_text: Option<String>,
    pub error: Option<String>,
    pub approval: Option<GraphApprovalRequestV1>,
    pub pending_state: Option<PendingGraphPassStateV1>,
    pub stopped_state: Option<StoppedGraphPassV1>,
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
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledGraphEdgeV1 {
    pub source: String,
    pub target: String,
    pub route: Option<String>,
}

/// One compiled bounded loop: the frozen routes, the single feedback edge that
/// closes the region, and the enclosed nodes in deterministic order.
#[derive(Clone, Debug)]
pub(crate) struct CompiledLoopV1 {
    pub header_id: String,
    pub body_edge: usize,
    pub exit_edge: usize,
    pub fallback_edge: usize,
    pub feedback_edge: usize,
    pub region: Vec<String>,
    pub maximum_iterations: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledGraphPassV1 {
    pub nodes: Vec<CompiledGraphNodeV1>,
    pub edges: Vec<CompiledGraphEdgeV1>,
    pub entry_node_id: String,
    pub topological_order: Vec<String>,
    /// Declared loops by header node id.
    pub loops: BTreeMap<String, CompiledLoopV1>,
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
            "external_agent" => {
                let tool_id = configuration
                    .get("toolId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("external agent node '{id}' has no toolId"))?;
                if !super::tool_loop::is_external_agent_tool(tool_id) {
                    return Err(format!(
                        "external agent node '{id}' selects '{tool_id}', which is not an installed external delegation tool"
                    ));
                }
                let binding = tool_bindings
                    .iter()
                    .find(|binding| binding.capability_id == tool_id)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "external agent node '{id}' binds tool '{tool_id}' with no frozen native binding"
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
        let route = super::documents::declared_edge_route(document_edge).map(str::to_owned);
        edges.push(CompiledGraphEdgeV1 {
            source,
            target,
            route,
        });
    }
    // Only a declared feedback edge may close a cycle; the order is computed
    // over the region-expanded graph so it stays total and deterministic.
    let node_types: BTreeMap<String, String> = nodes
        .iter()
        .map(|node| (node.id.clone(), node.node_type.clone()))
        .collect();
    let loop_iterations: BTreeMap<String, u32> = nodes
        .iter()
        .filter(|node| node.node_type == "loop")
        .map(|node| {
            let maximum = node
                .configuration
                .get("maximumIterations")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            (node.id.clone(), maximum as u32)
        })
        .collect();
    let edge_records: Vec<(String, String, Option<String>)> = edges
        .iter()
        .map(|edge| (edge.source.clone(), edge.target.clone(), edge.route.clone()))
        .collect();
    let loops = analyze_loop_regions(&node_types, &loop_iterations, &edge_records)?
        .into_iter()
        .map(|(header_id, region)| {
            (
                header_id,
                CompiledLoopV1 {
                    header_id: region.header_id,
                    body_edge: region.body_edge,
                    exit_edge: region.exit_edge,
                    fallback_edge: region.fallback_edge,
                    feedback_edge: region.feedback_edge,
                    region: region.region,
                    maximum_iterations: region.maximum_iterations,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let feedback: BTreeSet<usize> = loops.values().map(|region| region.feedback_edge).collect();
    let topological_order = topological_order(&nodes, &edges, &feedback)?;
    // Region members run in the same deterministic order as the graph.
    let position: BTreeMap<&str, usize> = topological_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut loops = loops;
    for region in loops.values_mut() {
        region
            .region
            .sort_by_key(|id| position.get(id.as_str()).copied().unwrap_or(usize::MAX));
    }
    Ok(CompiledGraphPassV1 {
        nodes,
        edges,
        entry_node_id,
        topological_order,
        loops,
    })
}

fn topological_order(
    nodes: &[CompiledGraphNodeV1],
    edges: &[CompiledGraphEdgeV1],
    feedback: &BTreeSet<usize>,
) -> Result<Vec<String>, String> {
    let document_order: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    let mut indegree: BTreeMap<&str, usize> =
        nodes.iter().map(|node| (node.id.as_str(), 0)).collect();
    let mut successors: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (index, edge) in edges.iter().enumerate() {
        if feedback.contains(&index) {
            continue;
        }
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

/// Selects the value carried into `node_id` by the first active transition
/// whose source has already executed.
///
/// A control node stores its own incoming value as its result, so an approval
/// or a routed condition hands the value on unchanged. A skipped branch never
/// activates its transition, so a successor it alone feeds stays `Null` and
/// contributes no value. The same rule makes an approval that the user rejects
/// produce no successor value at all: the pass fails before any value is read.
fn carried_value(
    edges: &[CompiledGraphEdgeV1],
    active_edges: &BTreeSet<usize>,
    executed: &BTreeSet<String>,
    values: &BTreeMap<String, Value>,
    node_id: &str,
) -> Value {
    for (index, edge) in edges.iter().enumerate() {
        if edge.target == node_id
            && active_edges.contains(&index)
            && executed.contains(&edge.source)
        {
            return values.get(&edge.source).cloned().unwrap_or(Value::Null);
        }
    }
    Value::Null
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
    /// Provider failures reported to a text-only node's model in this pass.
    text_error_recoveries: u32,
    final_text: Option<String>,
    pending_tool_approval: Option<(GraphApprovalRequestV1, AgentLoopSuspensionV1)>,
    resume_agent_suspension: Option<(AgentLoopSuspensionV1, Option<bool>)>,
    steered_node: Option<String>,
    activity_observer: Option<&'a dyn Fn(&GraphNodeActivityV1)>,
    /// Active loop iterations by header, outermost first in `loop_stack`.
    loop_frames: BTreeMap<String, LoopFrameStateV1>,
    loop_stack: Vec<String>,
    /// The one decision this pass was resumed with, if any.
    approval_decision: Option<bool>,
    /// Frozen wall-clock allowance for this pass; a loop stops at it.
    deadline_epoch_millis: u64,
}

impl<'a> PassMachine<'a> {
    fn run(
        mut self,
        pending: Option<&PendingGraphPassStateV1>,
        approval_decision: Option<bool>,
        stopped: Option<&StoppedGraphPassV1>,
        cancellation: &CancellationToken,
    ) -> GraphPassOutcomeV1 {
        if let Some(pending) = pending {
            self.values = pending.values.clone();
            self.completed = pending.completed.clone();
            self.executed = pending.completed.iter().cloned().collect();
            if let Some(edges) = &pending.active_edges {
                self.active_edges = edges.clone();
            }
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
        if let Some(stopped) = stopped {
            if let Err(error) = self.restore_stopped(stopped) {
                return self.failed_outcome(error);
            }
        }
        // A declared loop repeats its region inside the header, so the pass
        // walks a queue instead of one fixed sweep: a node already settled by a
        // later iteration is simply left alone.
        let mut queue: std::collections::VecDeque<String> =
            self.compiled.topological_order.iter().cloned().collect();
        while let Some(node_id) = queue.pop_front() {
            match self.run_node(&node_id, cancellation) {
                NodeStep::Done(outcome) => return outcome,
                NodeStep::Value(_) | NodeStep::Skipped => {}
            }
        }
        self.succeeded_outcome()
    }

    /// Runs one node exactly once per pass, honouring branch readiness.
    fn run_node(&mut self, node_id: &str, cancellation: &CancellationToken) -> NodeStep {
        if self.executed.contains(node_id) {
            return NodeStep::Skipped;
        }
        let Some(node) = self.compiled.nodes.iter().find(|node| node.id == node_id) else {
            return NodeStep::Done(
                self.failed_outcome(format!("compiled graph is missing node '{node_id}'")),
            );
        };
        if !self.ready(node_id) {
            self.push_activity(node, "skipped", "branch not taken");
            return NodeStep::Skipped;
        }
        // Finish pure graph bookkeeping after the last model response. A Stop
        // racing with completion must not leave a continuation that only
        // replays stale output and silently ignores the next input.
        if cancellation.is_cancelled()
            && matches!(
                node.node_type.as_str(),
                "agent" | "model_call" | "tool" | "approval" | "external_agent"
            )
        {
            return NodeStep::Done(self.stopped_outcome(
                node,
                "graph pass was cancelled".to_owned(),
                false,
            ));
        }
        if node.node_type == "loop" {
            return self.run_loop_header(node, cancellation);
        }
        self.push_activity(node, "started", "running");
        let value = if node.node_type == "approval" {
            match self.approval_decision {
                Some(true) => self.incoming_value(&node.id),
                Some(false) => {
                    self.push_activity(node, "failed", "rejected by the user");
                    return NodeStep::Done(self.failed_outcome(format!(
                        "approval '{}' was rejected by the user",
                        node.label
                    )));
                }
                None => {
                    self.push_activity(node, "waiting", "awaiting user decision");
                    return NodeStep::Done(self.pending_approval(node));
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
                    return NodeStep::Done(
                        self.pending_for_tool_approval(approval, next_suspension),
                    );
                }
                AgentResumeOutcomeV1::Failed(error) => {
                    self.push_activity(node, "failed", &error);
                    if cancellation.is_cancelled() {
                        return NodeStep::Done(self.stopped_outcome(node, error, true));
                    }
                    return NodeStep::Done(self.failed_outcome(error));
                }
            }
        } else {
            match self.execute_node(node, cancellation) {
                Ok(value) => value,
                Err(error) => {
                    self.push_activity(node, "failed", &error);
                    if cancellation.is_cancelled() {
                        return NodeStep::Done(self.stopped_outcome(node, error, true));
                    }
                    return NodeStep::Done(self.failed_outcome(error));
                }
            }
        };
        if let Some((approval, suspension)) = self.pending_tool_approval.take() {
            self.push_activity(node, "waiting", "awaiting user decision");
            return NodeStep::Done(self.pending_for_tool_approval(approval, suspension));
        }
        self.settle_node(node, value, &node_completion_summary(node));
        if matches!(node.node_type.as_str(), "wait" | "completion") {
            return NodeStep::Done(self.succeeded_outcome());
        }
        NodeStep::Value(self.values.get(&node.id).cloned().unwrap_or(Value::Null))
    }

    /// Runs a bounded loop: the frozen exit condition is evaluated before each
    /// iteration, the region repeats under it, and exhausting the declared
    /// iteration bound takes the explicit fallback transition instead of
    /// stopping silently.
    fn run_loop_header(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> NodeStep {
        let Some(region) = self.compiled.loops.get(&node.id).cloned() else {
            return NodeStep::Done(
                self.failed_outcome(format!("loop node '{}' has no compiled region", node.id)),
            );
        };
        let condition = node
            .configuration
            .get("exitCondition")
            .cloned()
            .unwrap_or_else(|| json!({"kind": "always"}));
        let restored = self.loop_frames.get(&node.id).cloned();
        // A restored frame continues its iteration; everything it had not yet
        // settled runs again, and nothing it did settles twice.
        let mut runs = restored.as_ref().map_or(0, |frame| frame.iteration);
        let mut body_entered = restored.is_some();
        if let Some(frame) = &restored {
            for region_id in &region.region {
                if !frame.completed_this_iteration.contains(region_id) {
                    self.executed.remove(region_id);
                }
            }
        }
        self.loop_stack.push(node.id.clone());
        let mut input = self.incoming_value(&node.id);
        self.values.insert(node.id.clone(), input.clone());
        self.executed.insert(node.id.clone());
        self.push_activity(node, "started", "running");
        loop {
            if !body_entered {
                let exit = match evaluate_predicate(&condition, &input) {
                    Ok(exit) => exit,
                    Err(error) => {
                        let message =
                            format!("loop node '{}' exit condition failed: {error}", node.id);
                        self.push_activity(node, "failed", &message);
                        self.loop_stack.pop();
                        return NodeStep::Done(self.failed_outcome(message));
                    }
                };
                self.push_activity(
                    node,
                    "evaluated",
                    &format!(
                        "iteration {} of {} · exit condition {}",
                        runs.max(1),
                        region.maximum_iterations,
                        if exit { "true" } else { "false" }
                    ),
                );
                if exit {
                    self.activate_loop_route(&region, LoopRouteV1::Exit);
                    let summary = format!("{runs} iteration(s) · exit condition true");
                    return self.finish_loop(node, input, &summary);
                }
                let deadline_reached = self.deadline_epoch_millis != u64::MAX
                    && self.now_epoch_millis >= self.deadline_epoch_millis;
                if deadline_reached {
                    self.activate_loop_route(&region, LoopRouteV1::Fallback);
                    self.push_activity(
                        node,
                        "limit-exceeded",
                        "the frozen run deadline was reached; routing to the fallback transition",
                    );
                    let summary =
                        "run deadline reached · routed to the fallback transition".to_owned();
                    return self.finish_loop(node, input, &summary);
                }
                if runs >= region.maximum_iterations {
                    self.activate_loop_route(&region, LoopRouteV1::Fallback);
                    self.push_activity(
                        node,
                        "limit-exceeded",
                        &format!(
                            "iteration bound {} reached; routing to the fallback transition",
                            region.maximum_iterations
                        ),
                    );
                    let summary = format!(
                        "iteration bound {} reached · routed to the fallback transition",
                        region.maximum_iterations
                    );
                    return self.finish_loop(node, input, &summary);
                }
                runs += 1;
                self.activate_loop_route(&region, LoopRouteV1::Body);
                for region_id in &region.region {
                    self.executed.remove(region_id);
                }
                self.loop_frames.insert(
                    node.id.clone(),
                    LoopFrameStateV1 {
                        header_id: node.id.clone(),
                        iteration: runs,
                        completed_this_iteration: Vec::new(),
                    },
                );
            }
            body_entered = false;
            for region_id in region.region.clone() {
                match self.run_node(&region_id, cancellation) {
                    NodeStep::Done(outcome) => {
                        // A suspension inside the region keeps its frame so the
                        // resumed pass continues this iteration.
                        return NodeStep::Done(outcome);
                    }
                    NodeStep::Value(_) | NodeStep::Skipped => {}
                }
            }
            // The next iteration receives what the region produced.
            if let Some(next) = self
                .values
                .get(&self.compiled.edges[region.feedback_edge].source)
                .cloned()
            {
                input = next;
            }
        }
    }

    /// Settles the header on its exit or exhaustion route. The pass continues
    /// down that route; a loop is not a terminal node.
    fn finish_loop(&mut self, node: &CompiledGraphNodeV1, value: Value, summary: &str) -> NodeStep {
        self.loop_stack.pop();
        self.loop_frames.remove(&node.id);
        self.settle_node(node, value, summary);
        NodeStep::Value(self.values.get(&node.id).cloned().unwrap_or(Value::Null))
    }

    /// Records a node's value and completion exactly once.
    fn settle_node(&mut self, node: &CompiledGraphNodeV1, value: Value, summary: &str) {
        self.values.insert(node.id.clone(), value);
        self.executed.insert(node.id.clone());
        if !self.completed.contains(&node.id) {
            self.completed.push(node.id.clone());
        }
        self.push_activity(node, "completed", summary);
        let Some(header) = self.loop_stack.last().cloned() else {
            return;
        };
        if self
            .compiled
            .loops
            .get(&header)
            .is_some_and(|region| region.region.contains(&node.id))
            && let Some(frame) = self.loop_frames.get_mut(&header)
        {
            frame.completed_this_iteration.push(node.id.clone());
        }
    }

    fn activate_loop_route(&mut self, region: &CompiledLoopV1, route: LoopRouteV1) {
        let chosen = match route {
            LoopRouteV1::Body => region.body_edge,
            LoopRouteV1::Exit => region.exit_edge,
            LoopRouteV1::Fallback => region.fallback_edge,
        };
        for candidate in [region.body_edge, region.exit_edge, region.fallback_edge] {
            if candidate == chosen {
                self.active_edges.insert(candidate);
            } else {
                self.active_edges.remove(&candidate);
            }
        }
    }

    /// The frame one node's activity belongs to. The loop header itself reports
    /// the iteration it is currently judging; a node shared by no loop has none.
    fn active_loop_frame(&self, node: &CompiledGraphNodeV1) -> Option<GraphLoopFrameV1> {
        let header = if node.node_type == "loop" {
            self.loop_stack
                .iter()
                .rev()
                .find(|header| *header == &node.id)
                .cloned()?
        } else {
            self.loop_stack.last().cloned()?
        };
        // A header reports the iteration it is judging, which is the first one
        // before any body run has opened a frame.
        let iteration = self
            .loop_frames
            .get(&header)
            .map_or(1, |frame| frame.iteration);
        let maximum_iterations = self
            .compiled
            .loops
            .get(&header)
            .map_or(0, |region| region.maximum_iterations);
        Some(GraphLoopFrameV1 {
            header_id: header,
            iteration,
            maximum_iterations,
        })
    }

    /// The iteration the running node belongs to, when it runs inside a loop.
    fn active_iteration(&self) -> Option<u32> {
        let header = self.loop_stack.last()?;
        self.loop_frames.get(header).map(|frame| frame.iteration)
    }

    /// The active iterations in outermost-first order, for durable resume.
    fn persisted_loop_frames(&self) -> Vec<LoopFrameStateV1> {
        self.loop_stack
            .iter()
            .filter_map(|header| self.loop_frames.get(header).cloned())
            .collect()
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
            active_edges: Some(self.active_edges.clone()),
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
            loop_frames: self.persisted_loop_frames(),
        };
        // A question is the same suspension with a different reason, so the
        // pass reports exactly one pending decision either way.
        let status = if approval.question.is_some() {
            GraphPassStatusV1::AwaitingAnswer
        } else {
            GraphPassStatusV1::AwaitingApproval
        };
        GraphPassOutcomeV1 {
            status,
            assistant_text: None,
            error: None,
            approval: Some(approval),
            pending_state: Some(pending_state),
            stopped_state: None,
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
            "external_agent" => self.run_external_agent_node(node, cancellation),
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
            loop_frame: self.active_loop_frame(node),
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

    fn incoming_value(&self, node_id: &str) -> Value {
        carried_value(
            &self.compiled.edges,
            &self.active_edges,
            &self.executed,
            &self.values,
            node_id,
        )
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

    fn run_agent(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let outer = instruction_agent_outer(
            self.outer_invocation_id,
            node,
            self.tool_authority.legacy_context_identity()
                && node
                    .tool_bindings
                    .iter()
                    .any(|binding| binding.is_callable()),
            self.active_iteration(),
        );
        let messages = context::agent_messages(
            node,
            &self.conversation,
            self.tool_authority.project_context(),
        );
        let initial_context =
            context::agent_turn_context(value_text(&self.incoming_agent_context(&node.id)));
        let context = json!({"messages": messages});
        let definitions = node
            .tool_bindings
            .iter()
            .filter(|binding| binding.is_callable())
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let parameters = node_model_parameters(&node.configuration);
        if definitions.is_empty() {
            return match self.execute_text_turn(
                &outer,
                &ModelResolutionPlanV1 {
                    candidates: vec![ModelCandidateV1 {
                        binding_id: self.model_binding_id.to_owned(),
                        version_hash: self.model_version_hash.to_owned(),
                    }],
                    maximum_input_bytes: MAXIMUM_PROVIDER_REQUEST_BYTES,
                    maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
                },
                ModelRequestV1 {
                    input: context,
                    parameters,
                },
                Some(&agent_context(node)),
                &initial_context,
                None,
                cancellation,
            ) {
                Ok(evidence) => {
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
        match execute_model_tool_loop_approval_v1(
            self.gateway,
            ModelToolLoopRequestV1 {
                agent_context: Some(agent_context(node)),
                outer_invocation_id: &outer,
                input: context,
                initial_context,
                initial_exchanges: Vec::new(),
                parameters,
                definitions,
                binding_id: self.model_binding_id.to_owned(),
                binding_version_hash: self.model_version_hash.to_owned(),
                maximum_input_bytes: MAXIMUM_PROVIDER_REQUEST_BYTES,
                maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
                maximum_tool_output_bytes: self.budget.maximum_tool_output_bytes,
                maximum_timeout_recoveries: self
                    .budget
                    .maximum_timeout_recoveries
                    .saturating_sub(self.timeout_recoveries),
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
        context_outer: &StableId,
        plan: &ModelResolutionPlanV1,
        mut request: ModelRequestV1,
        agent: Option<&AgentContextV1>,
        initial_context: &[aworkit_capability_host::ModelToolContextV1],
        retry_notice: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<ModelDispatchEvidenceV1, ProviderError> {
        let mut context = super::context_inspection::ContextDocument::from_input(&request.input)
            .map_err(ProviderError::Failed)?
            .request();
        context.context_messages.extend_from_slice(initial_context);
        context.parameters = request.parameters.clone();
        context.retry_notice = retry_notice.map(str::to_owned);
        let preparation = self
            .tool_authority
            .manage_model_context(
                self.gateway,
                plan,
                context_outer,
                0,
                agent,
                &mut context,
                cancellation,
                super::compaction::Trigger::Pressure,
            )
            .map_err(ProviderError::Failed)?;
        self.input_units = self.input_units.saturating_add(preparation.input_tokens);
        self.output_units = self.output_units.saturating_add(preparation.output_tokens);
        // An auxiliary compaction request the provider rejected is reported to
        // the model on this turn; it does not prove the acting request fails, so
        // it never ends the Agent node.
        let compaction_notice = preparation
            .provider_error
            .as_ref()
            .map(|error| provider_recovery_notice(error).unwrap_or_else(|| error.to_string()));
        if let Some(error) = preparation.error {
            return Err(ProviderError::Failed(error));
        }
        if !context.exchanges.is_empty() || !context.tools.is_empty() {
            return Err(ProviderError::Failed(
                "A text-only node cannot accept tool exchanges.".into(),
            ));
        }
        let mut recorded_context = context.clone();
        if let Some(notice) = compaction_notice {
            recorded_context.retry_notice = Some(match recorded_context.retry_notice.take() {
                Some(existing) => format!("{existing}\n\n{notice}"),
                None => notice,
            });
        }
        let mut overflow_retries = 0;
        loop {
            let mut context = recorded_context.clone();
            if !context.context_messages.is_empty() {
                context.input = context.projected_input()?;
                let messages = context
                    .input
                    .get_mut("messages")
                    .and_then(Value::as_array_mut)
                    .ok_or(ProviderError::InvalidPlan)?;
                messages.extend(
                    context
                        .context_messages
                        .iter()
                        .filter(|c| c.after_input_messages.is_none())
                        .map(|c| c.message()),
                );
            }
            if let Some(notice) = context.retry_notice {
                context.input["messages"]
                    .as_array_mut()
                    .ok_or(ProviderError::InvalidPlan)?
                    .push(json!({"role":"user","content":notice}));
            }
            request.input = context.input;
            self.tool_authority
                .record_text_context(&request.input, &recorded_context);
            self.attempted_model_turns = self.attempted_model_turns.saturating_add(1);
            match self
                .gateway
                .execute_cancellable(plan, &request, cancellation)
            {
                Err(error)
                    if is_context_overflow(&error)
                        && overflow_retries < preparation.max_overflow_retries =>
                {
                    let recovery = self
                        .tool_authority
                        .manage_model_context(
                            self.gateway,
                            plan,
                            context_outer,
                            0,
                            agent,
                            &mut recorded_context,
                            cancellation,
                            super::compaction::Trigger::ContextOverflow,
                        )
                        .map_err(ProviderError::Failed)?;
                    self.input_units = self.input_units.saturating_add(recovery.input_tokens);
                    self.output_units = self.output_units.saturating_add(recovery.output_tokens);
                    if let Some(error) = recovery.error {
                        return Err(ProviderError::Failed(error));
                    }
                    if cancellation.is_cancelled() {
                        return Err(ProviderError::Cancelled);
                    }
                    if !recovery.changed {
                        // The selection is already as small as the authority can
                        // make it. Report the condition to the model rather than
                        // ending the node.
                        self.text_error_recoveries = self.text_error_recoveries.saturating_add(1);
                        if self.text_error_recoveries > MAXIMUM_TEXT_ERROR_RECOVERIES {
                            return Err(error);
                        }
                        let recovery_notice =
                            provider_recovery_notice(&error).unwrap_or_else(|| error.to_string());
                        recorded_context.retry_notice = Some(match retry_notice {
                            Some(notice) => format!("{notice}\n\n{recovery_notice}"),
                            None => recovery_notice,
                        });
                        continue;
                    }
                    overflow_retries += 1;
                }
                Err(error) if provider_recovery_notice(&error).is_some() => {
                    self.text_error_recoveries = self.text_error_recoveries.saturating_add(1);
                    if self.text_error_recoveries > MAXIMUM_TEXT_ERROR_RECOVERIES {
                        return Err(error);
                    }
                    let recovery_notice =
                        provider_recovery_notice(&error).expect("recoverable error");
                    recorded_context.retry_notice = Some(match retry_notice {
                        Some(notice) => format!("{notice}\n\n{recovery_notice}"),
                        None => recovery_notice,
                    });
                }
                Ok(evidence) => {
                    let output = project_model_events(&evidence.events);
                    self.tool_authority
                        .record_context_usage(
                            context_outer,
                            agent,
                            &recorded_context,
                            output.input_tokens,
                            output.output_tokens,
                            super::compaction::text_tokens(&output.assistant_text) + 8,
                        )
                        .map_err(ProviderError::Failed)?;
                    return Ok(evidence);
                }
                Err(error) => return Err(error),
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
        let outer = instruction_agent_outer(
            self.outer_invocation_id,
            node,
            self.tool_authority.legacy_context_identity(),
            self.active_iteration(),
        );
        let messages = context::agent_messages(
            node,
            &self.conversation,
            self.tool_authority.project_context(),
        );
        let context = json!({"messages": messages});
        let definitions = node
            .tool_bindings
            .iter()
            .filter(|binding| binding.is_callable())
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let request = ModelToolLoopRequestV1 {
            agent_context: Some(agent_context(node)),
            outer_invocation_id: &outer,
            input: context,
            initial_context: context::agent_turn_context(value_text(
                &self.incoming_agent_context(&node.id),
            )),
            initial_exchanges: Vec::new(),
            parameters: node_model_parameters(&node.configuration),
            definitions,
            binding_id: self.model_binding_id.to_owned(),
            binding_version_hash: self.model_version_hash.to_owned(),
            maximum_input_bytes: MAXIMUM_PROVIDER_REQUEST_BYTES,
            maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
            maximum_tool_output_bytes: self.budget.maximum_tool_output_bytes,
            maximum_timeout_recoveries: self
                .budget
                .maximum_timeout_recoveries
                .saturating_sub(self.timeout_recoveries),
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

    /// Runs one workflow node as a single unattended external delegation.
    ///
    /// The node reuses the same frozen capability, authority and child-frame
    /// contract the delegation tool uses; its own settings only narrow the
    /// route for this one run.
    fn run_external_agent_node(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let binding = node
            .tool_bindings
            .first()
            .ok_or_else(|| format!("external agent node '{}' has no binding", node.id))?;
        let upstream = value_text(&self.incoming_value(&node.id));
        let instructions = node
            .configuration
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let task = if instructions.trim().is_empty() {
            upstream
        } else if upstream.trim().is_empty() {
            instructions.to_owned()
        } else {
            format!("{instructions}\n\n{upstream}")
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("task".to_owned(), Value::String(task));
        for key in ["model", "reasoningEffort"] {
            if let Some(value) = node.configuration.get(key).filter(|value| !value.is_null()) {
                arguments.insert(key.to_owned(), value.clone());
            }
        }
        let call = ModelToolCallV1 {
            call_id: format!("{}.external", node.id),
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
            Err(error) => Err(format!("external agent node '{}' failed: {error}", node.id)),
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
        let plan_review = plan_review_source(
            &self.compiled.edges,
            &self.active_edges,
            &self.executed,
            &self.compiled.nodes,
            &node.id,
        );
        let title = node
            .configuration
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if plan_review.is_some() {
                    "Approve plan".to_owned()
                } else {
                    "Workflow approval required".to_owned()
                }
            });
        let mut message = node
            .configuration
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or("The workflow reached an approval gate. Approve to continue the run.")
            .to_owned();
        // A plan review must show the plan: the reviewer decides on the content
        // the next node will receive, so the carried plan is part of the gate.
        if plan_review.is_some() {
            let summary = approval_value_summary(&self.incoming_value(&node.id));
            if !summary.trim().is_empty() {
                message.push_str("\n\n");
                message.push_str(&truncate_utf8(summary, MAXIMUM_APPROVAL_CARRIED_BYTES));
            }
        }
        let approval = GraphApprovalRequestV1 {
            question: None,
            filesystem: None,
            project_scope: None,
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
            active_edges: Some(self.active_edges.clone()),
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
            loop_frames: self.persisted_loop_frames(),
        };
        GraphPassOutcomeV1 {
            status: GraphPassStatusV1::AwaitingApproval,
            assistant_text: None,
            error: None,
            approval: Some(approval),
            pending_state: Some(pending_state),
            stopped_state: None,
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
            stopped_state: None,
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
            stopped_state: None,
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
    stopped: Option<&StoppedGraphPassV1>,
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
        conversation: conversation.to_vec(),
        values: BTreeMap::new(),
        completed: Vec::new(),
        executed: BTreeSet::new(),
        // A feedback edge is a declarative region boundary, never a runnable
        // transition: the header drives its own iterations, and excluding it
        // keeps the header ready before its region has run.
        active_edges: (0..compiled.edges.len())
            .filter(|index| {
                !compiled
                    .loops
                    .values()
                    .any(|region| region.feedback_edge == *index)
            })
            .collect(),
        activity: Vec::new(),
        tool_activity: Vec::new(),
        exchanges: Vec::new(),
        input_units: 0,
        output_units: 0,
        attempted_model_turns: 0,
        settled_tool_calls: 0,
        timeout_recoveries: 0,
        text_error_recoveries: 0,
        final_text: None,
        pending_tool_approval: None,
        resume_agent_suspension: None,
        steered_node: None,
        activity_observer,
        // A suspended pass resumes the iterations it was inside.
        loop_frames: pending
            .map(|pending| {
                pending
                    .loop_frames
                    .iter()
                    .map(|frame| (frame.header_id.clone(), frame.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        loop_stack: pending
            .map(|pending| {
                pending
                    .loop_frames
                    .iter()
                    .map(|frame| frame.header_id.clone())
                    .collect()
            })
            .unwrap_or_default(),
        approval_decision,
        deadline_epoch_millis,
    };
    machine.run(pending, approval_decision, stopped, cancellation)
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
        filesystem: challenge.filesystem.clone(),
        project_scope: challenge.project_scope.clone(),
        question: challenge.question.clone(),
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

/// How much of a reviewed plan an approval challenge shows. The frozen plan
/// contract already bounds each field, so this only protects the challenge.
const MAXIMUM_APPROVAL_CARRIED_BYTES: usize = 16 * 1024;

/// Whether the active transition into a node comes from a plan model call.
fn plan_review_source<'a>(
    edges: &[CompiledGraphEdgeV1],
    active_edges: &BTreeSet<usize>,
    executed: &BTreeSet<String>,
    nodes: &'a [CompiledGraphNodeV1],
    node_id: &str,
) -> Option<&'a CompiledGraphNodeV1> {
    for (index, edge) in edges.iter().enumerate() {
        if edge.target == node_id
            && active_edges.contains(&index)
            && executed.contains(&edge.source)
        {
            let source = nodes.iter().find(|node| node.id == edge.source)?;
            let is_plan = source.node_type == "model_call"
                && source
                    .configuration
                    .get("outputContract")
                    .and_then(Value::as_str)
                    == Some("plan");
            return is_plan.then_some(source);
        }
    }
    None
}

/// Formats the value a gate asks the user to approve. A structured plan is
/// rendered as readable lines so the reviewer sees the objective, open
/// questions, evidence and planned actions; any other value falls back to its
/// ordinary text form.
fn approval_value_summary(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return value_text(value);
    };
    let is_plan = ["goal", "openQuestions", "evidenceNeeded", "toolOrder"]
        .iter()
        .any(|key| object.contains_key(*key));
    if !is_plan {
        return value_text(value);
    }
    let mut lines = Vec::new();
    if let Some(goal) = object.get("goal").and_then(Value::as_str) {
        lines.push(format!("Goal: {goal}"));
    }
    for (label, key) in [
        ("Open questions", "openQuestions"),
        ("Evidence needed", "evidenceNeeded"),
        ("Planned actions", "toolOrder"),
    ] {
        let items = object
            .get(key)
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty());
        let Some(items) = items else { continue };
        lines.push(format!("{label}:"));
        for item in items.iter().filter_map(Value::as_str) {
            lines.push(format!("  - {item}"));
        }
    }
    if lines.is_empty() {
        value_text(value)
    } else {
        lines.join("\n")
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
        AgentContextV1, GraphLoopFrameV1, GraphPassBudgetV1, GraphPassOutcomeV1, GraphPassStatusV1,
        LoopFrameStateV1, ModelResolutionPlanV1, ModelToolCallV1, ModelToolInvocationPortV1,
        PendingGraphPassStateV1, WorkflowMessageV1, approval_value_summary, carried_value,
        compile_graph_pass, evaluate_predicate, execute_graph_pass_observed,
        node_completion_summary, node_model_parameters, plan_review_source, topological_order,
        value_text,
    };
    use crate::runtime::graph_pass::{CompiledGraphEdgeV1, CompiledGraphNodeV1};
    use crate::runtime::model_tool_loop::SettledModelToolCallV1;
    use aworkit_capability_host::{CancellationToken, FrozenModelGateway};
    use aworkit_capability_host::{ModelToolExchangeV1, ModelToolRequestV1};
    use aworkit_protocol::StableId;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    fn node(id: &str, node_type: &str) -> CompiledGraphNodeV1 {
        CompiledGraphNodeV1 {
            id: id.into(),
            node_type: node_type.into(),
            label: id.into(),
            configuration: json!({}),
            tool_bindings: Vec::new(),
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
        let none = BTreeSet::new();
        assert_eq!(
            topological_order(&nodes, &edges, &none).unwrap(),
            vec!["input.1", "plan.1", "agent.1", "wait.1"]
        );
        let mut cyclic = edges.clone();
        cyclic.push(edge("wait.1", "input.1", None));
        assert!(
            topological_order(&nodes, &cyclic, &none)
                .unwrap_err()
                .contains("cycle")
        );
        // A declared feedback edge is excluded from the order, which is how a
        // bounded loop keeps a total, deterministic execution order.
        let looped = vec![
            edge("input.1", "plan.1", None),
            edge("plan.1", "agent.1", None),
            edge("agent.1", "wait.1", None),
            edge("agent.1", "plan.1", Some("feedback")),
        ];
        assert_eq!(
            topological_order(&nodes, &looped, &BTreeSet::from([3_usize])).unwrap(),
            vec!["input.1", "plan.1", "agent.1", "wait.1"]
        );
    }

    #[test]
    fn compiled_bounded_loop_carries_its_region_and_edge_roles() {
        let document = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"loop.1","type":"loop","configuration":{
                    "exitCondition":{"kind":"exists","path":"done"},
                    "maximumIterations":3
                }},
                {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":[]}},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"loop.1"},
                {"id":"e2","source":"loop.1","target":"agent.1","configuration":{"route":"body"}},
                {"id":"e3","source":"loop.1","target":"wait.1","configuration":{"route":"exit"}},
                {"id":"e4","source":"loop.1","target":"wait.1","configuration":{"route":"fallback"}},
                {"id":"e5","source":"agent.1","target":"loop.1","configuration":{"route":"feedback"}}
            ]
        });
        let compiled = compile_graph_pass(&document, &[]).expect("compiled loop");
        let region = compiled.loops.get("loop.1").expect("declared loop");
        assert_eq!(region.region, vec!["agent.1".to_owned()]);
        assert_eq!(region.maximum_iterations, 3);
        assert_eq!(region.feedback_edge, 4);
        // The order stays total and deterministic: every node appears once and
        // the only transition that may point backwards is the feedback edge.
        assert_eq!(compiled.topological_order.len(), 4);
        let position = |id: &str| {
            compiled
                .topological_order
                .iter()
                .position(|candidate| candidate == id)
                .expect("node ordered")
        };
        for (index, edge) in compiled.edges.iter().enumerate() {
            if index == region.feedback_edge {
                continue;
            }
            assert!(
                position(&edge.source) < position(&edge.target),
                "transition {} -> {} must follow the order",
                edge.source,
                edge.target
            );
        }
    }

    /// A region that needs no provider: one pure node, optionally behind an
    /// approval gate, closed by the declared feedback edge.
    fn loop_document(predicate: Value, maximum: u32, gated: bool) -> Value {
        let mut nodes = vec![
            json!({"id":"input.1","type":"input"}),
            json!({"id":"loop.1","type":"loop","configuration":{
                "exitCondition": predicate,
                "maximumIterations": maximum
            }}),
            json!({"id":"parallel.1","type":"parallel"}),
            json!({"id":"wait.1","type":"wait"}),
            json!({"id":"wait.2","type":"wait"}),
        ];
        let mut edges = vec![
            json!({"id":"e1","source":"input.1","target":"loop.1"}),
            json!({"id":"e2","source":"loop.1","target":"parallel.1","configuration":{"route":"body"}}),
            json!({"id":"e3","source":"loop.1","target":"wait.1","configuration":{"route":"exit"}}),
            json!({"id":"e4","source":"loop.1","target":"wait.2","configuration":{"route":"fallback"}}),
        ];
        if gated {
            nodes.push(json!({"id":"gate.1","type":"approval","configuration":{}}));
            edges.push(json!({"id":"e5","source":"parallel.1","target":"gate.1"}));
            edges.push(json!({"id":"e6","source":"gate.1","target":"loop.1","configuration":{"route":"feedback"}}));
        } else {
            edges.push(json!({"id":"e5","source":"parallel.1","target":"loop.1","configuration":{"route":"feedback"}}));
        }
        json!({"schemaVersion":1,"nodes":nodes,"edges":edges})
    }

    struct NoTools;

    impl ModelToolInvocationPortV1 for NoTools {
        fn manage_model_context(
            &self,
            _: &FrozenModelGateway,
            _: &ModelResolutionPlanV1,
            _: &StableId,
            _: usize,
            _: Option<&AgentContextV1>,
            _: &mut ModelToolRequestV1,
            _: &CancellationToken,
            _: crate::runtime::compaction::Trigger,
        ) -> Result<crate::runtime::compaction::Preparation, String> {
            Ok(crate::runtime::compaction::Preparation {
                max_overflow_retries: 1,
                ..Default::default()
            })
        }
        fn invoke(
            &self,
            _: &StableId,
            _: u32,
            _: &ModelToolCallV1,
            _: &CancellationToken,
        ) -> Result<SettledModelToolCallV1, String> {
            Err("this graph binds no tools".to_owned())
        }
        fn commit_exchange(
            &self,
            _: &StableId,
            _: u32,
            _: &ModelToolExchangeV1,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn run_loop_pass(
        document: &Value,
        decision: Option<bool>,
        pending: Option<&PendingGraphPassStateV1>,
    ) -> GraphPassOutcomeV1 {
        run_loop_pass_until(document, decision, pending, u64::MAX)
    }

    fn run_loop_pass_until(
        document: &Value,
        decision: Option<bool>,
        pending: Option<&PendingGraphPassStateV1>,
        deadline_epoch_millis: u64,
    ) -> GraphPassOutcomeV1 {
        let compiled = compile_graph_pass(document, &[]).expect("compiled loop graph");
        let gateway = FrozenModelGateway::new(Vec::new());
        execute_graph_pass_observed(
            &compiled,
            &[WorkflowMessageV1 {
                role: "user".to_owned(),
                content: "go".to_owned(),
                images: Vec::new(),
            }],
            GraphPassBudgetV1 {
                maximum_timeout_recoveries: 1,
                maximum_tool_output_bytes: 65_536,
            },
            &gateway,
            &NoTools,
            &StableId::parse("invocation.loop.test").expect("invocation id"),
            "request.loop",
            "chat.loop",
            "run.loop",
            "binding.loop",
            "hash.loop",
            1_788_854_400_000,
            deadline_epoch_millis,
            pending,
            decision,
            None,
            &CancellationToken::default(),
            None,
        )
    }

    fn activity_count(outcome: &GraphPassOutcomeV1, node_id: &str, status: &str) -> usize {
        outcome
            .activity
            .iter()
            .filter(|activity| activity.node_id == node_id && activity.status == status)
            .count()
    }

    fn completed_nodes(outcome: &GraphPassOutcomeV1) -> Vec<String> {
        outcome
            .activity
            .iter()
            .filter(|activity| activity.status == "completed")
            .map(|activity| activity.node_id.clone())
            .collect()
    }

    #[test]
    fn a_loop_whose_exit_condition_holds_runs_no_iteration() {
        let outcome = run_loop_pass(
            &loop_document(json!({"kind": "always"}), 3, false),
            None,
            None,
        );
        assert_eq!(outcome.status, GraphPassStatusV1::Succeeded);
        assert_eq!(activity_count(&outcome, "parallel.1", "started"), 0);
        assert_eq!(
            completed_nodes(&outcome),
            vec!["input.1", "loop.1", "wait.1"]
        );
        let evaluation = outcome
            .activity
            .iter()
            .find(|activity| activity.status == "evaluated")
            .expect("exit condition evaluation");
        assert_eq!(
            evaluation.loop_frame,
            Some(GraphLoopFrameV1 {
                header_id: "loop.1".to_owned(),
                iteration: 1,
                maximum_iterations: 3,
            })
        );
        assert!(evaluation.summary.contains("exit condition true"));
    }

    #[test]
    fn exhausting_the_iteration_bound_takes_the_fallback_transition() {
        let outcome = run_loop_pass(
            &loop_document(json!({"kind": "exists", "path": "done"}), 2, false),
            None,
            None,
        );
        assert_eq!(outcome.status, GraphPassStatusV1::Succeeded);
        // Two admitted iterations, three evaluations, and an explicit limit
        // outcome instead of a silent stop.
        assert_eq!(activity_count(&outcome, "parallel.1", "completed"), 2);
        assert_eq!(activity_count(&outcome, "loop.1", "evaluated"), 3);
        assert_eq!(activity_count(&outcome, "loop.1", "limit-exceeded"), 1);
        let frames = outcome
            .activity
            .iter()
            .filter(|activity| activity.node_id == "parallel.1" && activity.status == "completed")
            .map(|activity| activity.loop_frame.as_ref().map(|frame| frame.iteration))
            .collect::<Vec<_>>();
        assert_eq!(frames, vec![Some(1), Some(2)]);
        // The fallback route runs and the normal exit route is left behind.
        assert!(completed_nodes(&outcome).contains(&"wait.2".to_owned()));
        assert_eq!(activity_count(&outcome, "wait.1", "completed"), 0);
    }

    #[test]
    fn a_frozen_run_deadline_reached_routes_the_fallback_transition() {
        let outcome = run_loop_pass_until(
            &loop_document(json!({"kind": "exists", "path": "done"}), 5, false),
            None,
            None,
            1_788_854_400_000,
        );
        assert_eq!(activity_count(&outcome, "parallel.1", "completed"), 0);
        let limit = outcome
            .activity
            .iter()
            .find(|activity| activity.status == "limit-exceeded")
            .expect("deadline outcome");
        assert!(limit.summary.contains("deadline"), "{}", limit.summary);
        assert!(completed_nodes(&outcome).contains(&"wait.2".to_owned()));
        assert_eq!(activity_count(&outcome, "wait.1", "completed"), 0);
    }

    #[test]
    fn nested_loops_run_their_own_frames_and_charge_their_own_bounds() {
        // outer(body -> inner) with the inner loop's routes joining the outer
        // region, so only one feedback edge closes each region.
        let document = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"loop.outer","type":"loop","configuration":{
                    "exitCondition":{"kind":"exists","path":"done"},"maximumIterations":2
                }},
                {"id":"loop.inner","type":"loop","configuration":{
                    "exitCondition":{"kind":"exists","path":"done"},"maximumIterations":2
                }},
                {"id":"parallel.1","type":"parallel"},
                {"id":"join.1","type":"parallel"},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"loop.outer"},
                {"id":"e2","source":"loop.outer","target":"loop.inner","configuration":{"route":"body"}},
                {"id":"e3","source":"loop.outer","target":"wait.1","configuration":{"route":"exit"}},
                {"id":"e4","source":"loop.outer","target":"wait.1","configuration":{"route":"fallback"}},
                {"id":"e5","source":"join.1","target":"loop.outer","configuration":{"route":"feedback"}},
                {"id":"e6","source":"loop.inner","target":"parallel.1","configuration":{"route":"body"}},
                {"id":"e7","source":"loop.inner","target":"join.1","configuration":{"route":"exit"}},
                {"id":"e8","source":"loop.inner","target":"join.1","configuration":{"route":"fallback"}},
                {"id":"e9","source":"parallel.1","target":"loop.inner","configuration":{"route":"feedback"}}
            ]
        });
        let outcome = run_loop_pass(&document, None, None);
        assert_eq!(outcome.status, GraphPassStatusV1::Succeeded);
        // The inner loop runs its bound inside each outer iteration.
        assert_eq!(activity_count(&outcome, "parallel.1", "completed"), 4);
        let inner_frames = outcome
            .activity
            .iter()
            .filter(|activity| activity.node_id == "parallel.1" && activity.status == "completed")
            .filter_map(|activity| activity.loop_frame.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            inner_frames
                .iter()
                .map(|frame| (frame.header_id.as_str(), frame.iteration))
                .collect::<Vec<_>>(),
            vec![("loop.inner", 1), ("loop.inner", 2), ("loop.inner", 1), ("loop.inner", 2)]
        );
        // The join node belongs to the outer loop's iterations.
        let outer_frames = outcome
            .activity
            .iter()
            .filter(|activity| activity.node_id == "join.1" && activity.status == "completed")
            .filter_map(|activity| activity.loop_frame.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            outer_frames
                .iter()
                .map(|frame| (frame.header_id.as_str(), frame.iteration))
                .collect::<Vec<_>>(),
            vec![("loop.outer", 1), ("loop.outer", 2)]
        );
        // Each loop charges its own bound and routes its own fallback: the
        // inner bound is charged once per outer iteration, the outer bound once.
        assert_eq!(activity_count(&outcome, "loop.inner", "limit-exceeded"), 2);
        assert_eq!(activity_count(&outcome, "loop.outer", "limit-exceeded"), 1);
        assert!(completed_nodes(&outcome).contains(&"wait.1".to_owned()));
    }

    #[test]
    fn bounded_loop_execution_is_deterministic() {
        let document = loop_document(json!({"kind": "exists", "path": "done"}), 2, false);
        let first = run_loop_pass(&document, None, None);
        let second = run_loop_pass(&document, None, None);
        assert_eq!(
            serde_json::to_value(&first.activity).expect("first activity"),
            serde_json::to_value(&second.activity).expect("second activity")
        );
    }

    #[test]
    fn a_suspension_inside_a_region_resumes_its_own_iteration() {
        let document = loop_document(json!({"kind": "exists", "path": "done"}), 2, true);
        let suspended = run_loop_pass(&document, None, None);
        assert_eq!(suspended.status, GraphPassStatusV1::AwaitingApproval);
        let pending = suspended.pending_state.expect("pending state");
        assert_eq!(
            pending.loop_frames,
            vec![LoopFrameStateV1 {
                header_id: "loop.1".to_owned(),
                iteration: 1,
                completed_this_iteration: vec!["parallel.1".to_owned()],
            }]
        );

        let resumed = run_loop_pass(&document, Some(true), Some(&pending));
        assert_eq!(resumed.status, GraphPassStatusV1::Succeeded);
        // Iteration 1 settled the parallel node before the gate; the resume
        // continues that iteration without replaying it, and iteration 2 runs
        // the region once more.
        assert_eq!(activity_count(&resumed, "parallel.1", "completed"), 2);
        assert_eq!(activity_count(&resumed, "gate.1", "completed"), 2);
        assert_eq!(activity_count(&resumed, "loop.1", "limit-exceeded"), 1);
        assert!(completed_nodes(&resumed).contains(&"wait.2".to_owned()));
    }

    #[test]
    fn value_text_stringifies_results() {
        assert_eq!(value_text(&json!("hello")), "hello");
        assert_eq!(value_text(&json!({"a":1})), r#"{"a":1}"#);
        assert_eq!(value_text(&json!(null)), "");
    }

    #[test]
    fn a_plan_review_source_is_recognized_and_the_plan_is_rendered_readably() {
        let mut plan_node = node("plan.1", "model_call");
        plan_node.configuration = json!({"modelTierId": "tier:balanced", "outputContract": "plan"});
        let nodes = vec![plan_node, node("approval.1", "approval")];
        let edges = vec![edge("plan.1", "approval.1", None)];
        let executed = BTreeSet::from(["plan.1".to_string()]);
        assert_eq!(
            plan_review_source(
                &edges,
                &BTreeSet::from([0]),
                &executed,
                &nodes,
                "approval.1"
            )
            .map(|node| node.id.as_str()),
            Some("plan.1")
        );
        // A model call without the plan contract is not a plan review.
        let mut plain = node("plan.1", "model_call");
        plain.configuration = json!({"modelTierId": "tier:balanced"});
        let nodes = vec![plain, node("approval.1", "approval")];
        assert!(
            plan_review_source(
                &edges,
                &BTreeSet::from([0]),
                &executed,
                &nodes,
                "approval.1"
            )
            .is_none()
        );
        // A skipped transition is not a plan review either.
        assert!(
            plan_review_source(&edges, &BTreeSet::new(), &executed, &nodes, "approval.1").is_none()
        );

        let summary = approval_value_summary(&json!({
            "goal": "Ship task 110",
            "openQuestions": ["Which gate?"],
            "evidenceNeeded": ["The workflow contract"],
            "toolOrder": ["Update the catalog", "Add tests"],
        }));
        assert!(summary.contains("Goal: Ship task 110"));
        assert!(summary.contains("- Update the catalog"));
        assert_eq!(approval_value_summary(&json!("plain value")), "plain value");
        assert_eq!(approval_value_summary(&json!(null)), "");
    }

    #[test]
    fn control_values_cross_active_edges_and_skipped_branches_contribute_nothing() {
        let edges = vec![
            edge("approval.1", "agent.1", None),
            edge("condition.1", "agent.1", Some("true")),
        ];
        let values = BTreeMap::from([
            ("approval.1".to_string(), json!("the plan")),
            ("condition.1".to_string(), json!("routed plan")),
        ]);
        let approval_executed = BTreeSet::from(["approval.1".to_string()]);
        // The active approval transition carries its value into the Agent.
        assert_eq!(
            carried_value(
                &edges,
                &BTreeSet::from([0]),
                &approval_executed,
                &values,
                "agent.1"
            ),
            json!("the plan")
        );
        // A skipped branch activates no transition, so no value is invented.
        assert_eq!(
            carried_value(
                &edges,
                &BTreeSet::new(),
                &approval_executed,
                &values,
                "agent.1"
            ),
            json!(null)
        );
        // An active transition from a node that never executed carries nothing.
        assert_eq!(
            carried_value(
                &edges,
                &BTreeSet::from([1]),
                &approval_executed,
                &values,
                "agent.1"
            ),
            json!(null)
        );
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
