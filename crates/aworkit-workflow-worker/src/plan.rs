//! Validation and deterministic compilation of immutable worker snapshots.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{
    ProcessGeneration, StableId, WorkerExecutorKindV1, WorkerFrozenRunSnapshotV1,
    WorkerJoinDescriptorV1, WorkerLoopDescriptorV1, WorkerNodeV1, WorkerRouteRuleV1,
    WorkerTransitionV1, is_canonical_sha256,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const SUPPORTED_WORKER_SCHEMA_VERSION: u16 = 1;
pub const WORKER_COMPILER_VERSION: &str = "aworkit-worker-v1";

/// Compatibility snapshot retained for early tests. Product execution uses the
/// shared `WorkerFrozenRunSnapshotV1` contract below.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenRunSnapshot {
    pub snapshot_id: StableId,
    pub snapshot_hash: String,
    pub generation: ProcessGeneration,
    pub nodes: Vec<PlanNode>,
    pub transitions: Vec<Transition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanNode {
    pub id: StableId,
    pub node_type: String,
    pub version: u16,
    #[serde(default)]
    pub config: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transition {
    pub id: StableId,
    pub from: StableId,
    pub from_port: String,
    pub to: StableId,
    pub to_port: String,
    #[serde(default)]
    pub priority: u32,
    #[serde(default)]
    pub loop_bound: Option<u32>,
}

/// Indexed compatibility plan.
#[derive(Clone, Debug)]
pub struct ExecutionPlan {
    snapshot: FrozenRunSnapshot,
    nodes: BTreeMap<String, PlanNode>,
    outgoing: BTreeMap<String, Vec<Transition>>,
    fingerprint: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PlanError {
    #[error("snapshot belongs to a different worker generation")]
    GenerationMismatch,
    #[error("snapshot must contain at least one node")]
    EmptyGraph,
    #[error("duplicate node id {0}")]
    DuplicateNode(String),
    #[error("duplicate transition id {0}")]
    DuplicateTransition(String),
    #[error("transition {transition} references unknown node {node}")]
    UnknownNode { transition: String, node: String },
    #[error("cycle requires an explicit positive loop bound on transition {0}")]
    UnboundedCycle(String),
    #[error("unsupported worker snapshot schema version {0}")]
    UnsupportedSchema(u16),
    #[error("unsupported worker compiler version {0}")]
    UnsupportedCompiler(String),
    #[error("snapshot hash does not match its canonical content")]
    SnapshotHashMismatch,
    #[error("workflow, contribution, authority, or plan hash is malformed")]
    InvalidHash,
    #[error("entry node is duplicated or absent from the plan: {0}")]
    InvalidEntry(String),
    #[error("node type/version or contribution is malformed: {0}")]
    InvalidNodeContract(String),
    #[error("duplicate or malformed port {port} on node {node}")]
    InvalidPort { node: String, port: String },
    #[error("transition {transition} has incompatible ports")]
    PortMismatch { transition: String },
    #[error("node {node} references an unfrozen capability {capability}")]
    UnresolvedCapability { node: String, capability: String },
    #[error("loop descriptor is missing, duplicate, invalid, or unused: {0}")]
    InvalidLoop(String),
    #[error("join descriptor is missing, duplicate, or invalid: {0}")]
    InvalidJoin(String),
    #[error("route rule is missing, duplicate, invalid, or points outside its router: {0}")]
    InvalidRoute(String),
    #[error("snapshot exceeds a bounded worker compilation limit")]
    PlanTooLarge,
    #[error("snapshot cannot be encoded canonically")]
    Encoding,
}

impl ExecutionPlan {
    pub fn compile(
        snapshot: FrozenRunSnapshot,
        generation: ProcessGeneration,
    ) -> Result<Self, PlanError> {
        if snapshot.generation != generation {
            return Err(PlanError::GenerationMismatch);
        }
        if snapshot.nodes.is_empty() {
            return Err(PlanError::EmptyGraph);
        }
        let mut nodes = BTreeMap::new();
        for node in &snapshot.nodes {
            if nodes
                .insert(node.id.as_str().to_owned(), node.clone())
                .is_some()
            {
                return Err(PlanError::DuplicateNode(node.id.to_string()));
            }
        }
        let mut outgoing: BTreeMap<String, Vec<Transition>> = BTreeMap::new();
        let mut transition_ids = BTreeSet::new();
        for transition in &snapshot.transitions {
            if !transition_ids.insert(transition.id.as_str()) {
                return Err(PlanError::DuplicateTransition(transition.id.to_string()));
            }
            for node in [&transition.from, &transition.to] {
                if !nodes.contains_key(node.as_str()) {
                    return Err(PlanError::UnknownNode {
                        transition: transition.id.to_string(),
                        node: node.to_string(),
                    });
                }
            }
            outgoing
                .entry(transition.from.to_string())
                .or_default()
                .push(transition.clone());
        }
        for transitions in outgoing.values_mut() {
            transitions.sort_by_key(|edge| (edge.priority, edge.id.to_string()));
        }
        validate_legacy_cycles(&nodes, &outgoing)?;
        let fingerprint = legacy_fingerprint(&snapshot)?;
        Ok(Self {
            snapshot,
            nodes,
            outgoing,
            fingerprint,
        })
    }

    #[must_use]
    pub fn node(&self, id: &StableId) -> Option<&PlanNode> {
        self.nodes.get(id.as_str())
    }

    #[must_use]
    pub fn outgoing(&self, id: &StableId) -> &[Transition] {
        self.outgoing.get(id.as_str()).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    #[must_use]
    pub fn snapshot(&self) -> &FrozenRunSnapshot {
        &self.snapshot
    }
}

/// Complete, shared-contract execution plan used by the service runtime.
#[derive(Clone, Debug)]
pub struct ExecutionPlanV1 {
    snapshot: WorkerFrozenRunSnapshotV1,
    nodes: BTreeMap<String, WorkerNodeV1>,
    outgoing: BTreeMap<String, Vec<WorkerTransitionV1>>,
    transitions: BTreeMap<String, WorkerTransitionV1>,
    loops: BTreeMap<String, WorkerLoopDescriptorV1>,
    joins: BTreeMap<String, WorkerJoinDescriptorV1>,
    routes: BTreeMap<String, Vec<WorkerRouteRuleV1>>,
    fingerprint: String,
}

impl ExecutionPlanV1 {
    /// Compiles only the exact frozen DTO. `declared_snapshot_hash` is carried
    /// independently by the generation-fenced control envelope and is checked
    /// against a canonical snapshot digest before any node can be scheduled.
    pub fn compile(
        mut snapshot: WorkerFrozenRunSnapshotV1,
        declared_snapshot_hash: &str,
    ) -> Result<Self, PlanError> {
        validate_snapshot_bounds(&snapshot)?;
        if snapshot.schema_version != SUPPORTED_WORKER_SCHEMA_VERSION {
            return Err(PlanError::UnsupportedSchema(snapshot.schema_version));
        }
        if snapshot.compiler_version != WORKER_COMPILER_VERSION {
            return Err(PlanError::UnsupportedCompiler(
                snapshot.compiler_version.clone(),
            ));
        }
        if !valid_sha256(&snapshot.workflow_hash)
            || !valid_sha256(&snapshot.authority_manifest_hash)
            || !valid_sha256(&snapshot.snapshot_hash)
            || !valid_sha256(declared_snapshot_hash)
        {
            return Err(PlanError::InvalidHash);
        }
        let calculated_hash = snapshot_content_hash(&snapshot)?;
        if snapshot.snapshot_hash != declared_snapshot_hash
            || calculated_hash != snapshot.snapshot_hash
        {
            return Err(PlanError::SnapshotHashMismatch);
        }
        canonicalize_snapshot(&mut snapshot);

        let capability_refs: BTreeSet<_> = snapshot
            .capability_refs
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        if capability_refs.len() != snapshot.capability_refs.len() {
            return Err(PlanError::InvalidNodeContract(
                "duplicate capability reference".to_owned(),
            ));
        }
        let frozen_binding_refs: BTreeSet<_> = snapshot
            .capability_bindings
            .iter()
            .map(|binding| binding.capability_id.as_str().to_owned())
            .collect();
        if frozen_binding_refs.len() != snapshot.capability_bindings.len()
            || frozen_binding_refs != capability_refs
        {
            return Err(PlanError::InvalidNodeContract(
                "frozen capability bindings differ from capability references".to_owned(),
            ));
        }
        for binding in &snapshot.capability_bindings {
            let invalid_extension = binding.extension.as_ref().is_some_and(|extension| {
                extension.host_generation.0 == 0
                    || extension.contribution_id != binding.adapter_id
                    || !is_canonical_sha256(&extension.identity.content_hash)
                    || !is_canonical_sha256(&extension.handshake_hash)
            });
            let invalid_isolation =
                binding
                    .required_isolation_profile
                    .as_deref()
                    .is_some_and(|profile| {
                        profile.is_empty()
                            || profile.len() > 256
                            || profile.trim() != profile
                            || profile.chars().any(char::is_control)
                    });
            if binding.adapter_version.trim().is_empty()
                || !is_canonical_sha256(&binding.descriptor_hash)
                || invalid_extension
                || invalid_isolation
            {
                return Err(PlanError::InvalidNodeContract(format!(
                    "invalid frozen capability binding {}",
                    binding.capability_id
                )));
            }
        }

        let mut nodes = BTreeMap::new();
        for node in &snapshot.nodes {
            validate_node(node, &capability_refs)?;
            if nodes
                .insert(node.node_id.as_str().to_owned(), node.clone())
                .is_some()
            {
                return Err(PlanError::DuplicateNode(node.node_id.to_string()));
            }
        }
        if nodes.is_empty() {
            return Err(PlanError::EmptyGraph);
        }

        let mut entry_ids = BTreeSet::new();
        for entry in &snapshot.entry_nodes {
            if !entry_ids.insert(entry.as_str()) || !nodes.contains_key(entry.as_str()) {
                return Err(PlanError::InvalidEntry(entry.to_string()));
            }
        }
        if entry_ids.is_empty() {
            return Err(PlanError::InvalidEntry("<empty>".to_owned()));
        }

        let mut transitions = BTreeMap::new();
        let mut outgoing: BTreeMap<String, Vec<WorkerTransitionV1>> = BTreeMap::new();
        for transition in &snapshot.transitions {
            validate_transition(transition, &nodes)?;
            if transitions
                .insert(
                    transition.transition_id.as_str().to_owned(),
                    transition.clone(),
                )
                .is_some()
            {
                return Err(PlanError::DuplicateTransition(
                    transition.transition_id.to_string(),
                ));
            }
            outgoing
                .entry(transition.from_node.as_str().to_owned())
                .or_default()
                .push(transition.clone());
        }
        for edges in outgoing.values_mut() {
            edges.sort_by(|left, right| {
                left.priority.cmp(&right.priority).then_with(|| {
                    left.transition_id
                        .as_str()
                        .cmp(right.transition_id.as_str())
                })
            });
        }

        let loops = validate_loops(&snapshot.loop_descriptors, &nodes, &transitions)?;
        validate_v1_cycles(&nodes, &outgoing, &loops)?;
        let joins = validate_joins(&snapshot.join_descriptors, &nodes)?;
        let routes = validate_routes(&snapshot.route_rules, &nodes, &transitions)?;
        let fingerprint = plan_fingerprint(&snapshot)?;
        Ok(Self {
            snapshot,
            nodes,
            outgoing,
            transitions,
            loops,
            joins,
            routes,
            fingerprint,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> &WorkerFrozenRunSnapshotV1 {
        &self.snapshot
    }

    #[must_use]
    pub fn node(&self, id: &StableId) -> Option<&WorkerNodeV1> {
        self.nodes.get(id.as_str())
    }

    #[must_use]
    pub fn transition(&self, id: &StableId) -> Option<&WorkerTransitionV1> {
        self.transitions.get(id.as_str())
    }

    #[must_use]
    pub fn outgoing(&self, id: &StableId) -> &[WorkerTransitionV1] {
        self.outgoing.get(id.as_str()).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn loop_descriptor(&self, id: &StableId) -> Option<&WorkerLoopDescriptorV1> {
        self.loops.get(id.as_str())
    }

    #[must_use]
    pub fn join_descriptor(&self, node_id: &StableId) -> Option<&WorkerJoinDescriptorV1> {
        self.joins.get(node_id.as_str())
    }

    #[must_use]
    pub fn route_rules(&self, node_id: &StableId) -> &[WorkerRouteRuleV1] {
        self.routes.get(node_id.as_str()).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Canonical content hash used by both core and worker. Callers must sort or
/// use this helper before freezing a snapshot.
pub fn snapshot_content_hash(snapshot: &WorkerFrozenRunSnapshotV1) -> Result<String, PlanError> {
    let mut canonical = snapshot.clone();
    canonical.snapshot_hash.clear();
    canonicalize_snapshot(&mut canonical);
    let bytes = serde_jcs::to_vec(&canonical).map_err(|_| PlanError::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn canonical_snapshot_hash(snapshot: &WorkerFrozenRunSnapshotV1) -> Result<String, PlanError> {
    snapshot_content_hash(snapshot)
}

fn validate_snapshot_bounds(snapshot: &WorkerFrozenRunSnapshotV1) -> Result<(), PlanError> {
    if snapshot.nodes.len() > 100_000
        || snapshot.transitions.len() > 500_000
        || snapshot.entry_nodes.len() > 1_024
        || snapshot.loop_descriptors.len() > 10_000
        || snapshot.join_descriptors.len() > 10_000
        || snapshot.route_rules.len() > 100_000
        || snapshot.capability_bindings.len() > 4_096
        || snapshot.capability_refs.len() > 4_096
        || serde_json::to_vec(snapshot)
            .map_err(|_| PlanError::Encoding)?
            .len()
            > 16 * 1024 * 1024
    {
        Err(PlanError::PlanTooLarge)
    } else {
        Ok(())
    }
}

fn validate_node(node: &WorkerNodeV1, capabilities: &BTreeSet<String>) -> Result<(), PlanError> {
    if node.node_type.is_empty()
        || node.node_type.len() > 128
        || node.node_version == 0
        || !valid_sha256(&node.contribution_hash)
    {
        return Err(PlanError::InvalidNodeContract(node.node_id.to_string()));
    }
    let mut input_names = BTreeSet::new();
    for port in &node.inputs {
        if !valid_port_name(&port.name) || !input_names.insert(port.name.as_str()) {
            return Err(PlanError::InvalidPort {
                node: node.node_id.to_string(),
                port: port.name.clone(),
            });
        }
    }
    let mut output_names = BTreeSet::new();
    for port in &node.outputs {
        if !valid_port_name(&port.name) || !output_names.insert(port.name.as_str()) {
            return Err(PlanError::InvalidPort {
                node: node.node_id.to_string(),
                port: port.name.clone(),
            });
        }
    }
    match (&node.executor, &node.capability_ref) {
        (
            WorkerExecutorKindV1::Brokered
            | WorkerExecutorKindV1::Model
            | WorkerExecutorKindV1::Agent,
            Some(capability),
        ) if capabilities.contains(capability.as_str()) => {}
        (
            WorkerExecutorKindV1::Brokered
            | WorkerExecutorKindV1::Model
            | WorkerExecutorKindV1::Agent,
            Some(capability),
        ) => {
            return Err(PlanError::UnresolvedCapability {
                node: node.node_id.to_string(),
                capability: capability.to_string(),
            });
        }
        (
            WorkerExecutorKindV1::Brokered
            | WorkerExecutorKindV1::Model
            | WorkerExecutorKindV1::Agent,
            None,
        ) => {
            return Err(PlanError::InvalidNodeContract(node.node_id.to_string()));
        }
        (_, Some(capability)) if !capabilities.contains(capability.as_str()) => {
            return Err(PlanError::UnresolvedCapability {
                node: node.node_id.to_string(),
                capability: capability.to_string(),
            });
        }
        _ => {}
    }
    Ok(())
}

fn validate_transition(
    transition: &WorkerTransitionV1,
    nodes: &BTreeMap<String, WorkerNodeV1>,
) -> Result<(), PlanError> {
    let from = nodes
        .get(transition.from_node.as_str())
        .ok_or_else(|| PlanError::UnknownNode {
            transition: transition.transition_id.to_string(),
            node: transition.from_node.to_string(),
        })?;
    let to = nodes
        .get(transition.to_node.as_str())
        .ok_or_else(|| PlanError::UnknownNode {
            transition: transition.transition_id.to_string(),
            node: transition.to_node.to_string(),
        })?;
    let output = from
        .outputs
        .iter()
        .find(|port| port.name == transition.from_port)
        .ok_or_else(|| PlanError::PortMismatch {
            transition: transition.transition_id.to_string(),
        })?;
    let input = to
        .inputs
        .iter()
        .find(|port| port.name == transition.to_port)
        .ok_or_else(|| PlanError::PortMismatch {
            transition: transition.transition_id.to_string(),
        })?;
    if output.schema_ref.is_some()
        && input.schema_ref.is_some()
        && output.schema_ref != input.schema_ref
    {
        return Err(PlanError::PortMismatch {
            transition: transition.transition_id.to_string(),
        });
    }
    if let Some(predicate) = &transition.predicate
        && serde_json::to_vec(predicate)
            .map_err(|_| PlanError::Encoding)?
            .len()
            > 64 * 1024
    {
        return Err(PlanError::PlanTooLarge);
    }
    Ok(())
}

fn validate_loops(
    descriptors: &[WorkerLoopDescriptorV1],
    nodes: &BTreeMap<String, WorkerNodeV1>,
    transitions: &BTreeMap<String, WorkerTransitionV1>,
) -> Result<BTreeMap<String, WorkerLoopDescriptorV1>, PlanError> {
    let mut loops = BTreeMap::new();
    for descriptor in descriptors {
        if descriptor.maximum_iterations == 0
            || !nodes.contains_key(descriptor.body_entry.as_str())
            || !nodes.contains_key(descriptor.body_exit.as_str())
            || loops
                .insert(descriptor.loop_id.as_str().to_owned(), descriptor.clone())
                .is_some()
        {
            return Err(PlanError::InvalidLoop(descriptor.loop_id.to_string()));
        }
    }
    let mut used = BTreeSet::new();
    for transition in transitions.values() {
        if let Some(loop_id) = &transition.declared_loop_id {
            let Some(descriptor) = loops.get(loop_id.as_str()) else {
                return Err(PlanError::InvalidLoop(loop_id.to_string()));
            };
            if transition.from_node != descriptor.body_exit
                || transition.to_node != descriptor.body_entry
            {
                return Err(PlanError::InvalidLoop(loop_id.to_string()));
            }
            used.insert(loop_id.as_str());
        }
    }
    if let Some(unused) = loops.keys().find(|id| !used.contains(id.as_str())) {
        return Err(PlanError::InvalidLoop(unused.clone()));
    }
    Ok(loops)
}

fn validate_joins(
    descriptors: &[WorkerJoinDescriptorV1],
    nodes: &BTreeMap<String, WorkerNodeV1>,
) -> Result<BTreeMap<String, WorkerJoinDescriptorV1>, PlanError> {
    let mut joins = BTreeMap::new();
    let allowed = ["require_equal", "object_union", "ordered_array"];
    for descriptor in descriptors {
        let Some(node) = nodes.get(descriptor.node_id.as_str()) else {
            return Err(PlanError::InvalidJoin(descriptor.join_id.to_string()));
        };
        let branch_count = descriptor
            .expected_branches
            .iter()
            .map(StableId::as_str)
            .collect::<BTreeSet<_>>()
            .len();
        if node.executor != WorkerExecutorKindV1::Join
            || descriptor.expected_branches.len() < 2
            || branch_count != descriptor.expected_branches.len()
            || !allowed.contains(&descriptor.merge_policy.as_str())
            || joins
                .insert(descriptor.node_id.as_str().to_owned(), descriptor.clone())
                .is_some()
        {
            return Err(PlanError::InvalidJoin(descriptor.join_id.to_string()));
        }
    }
    Ok(joins)
}

fn validate_routes(
    rules: &[WorkerRouteRuleV1],
    nodes: &BTreeMap<String, WorkerNodeV1>,
    transitions: &BTreeMap<String, WorkerTransitionV1>,
) -> Result<BTreeMap<String, Vec<WorkerRouteRuleV1>>, PlanError> {
    let mut ids = BTreeSet::new();
    let mut routes: BTreeMap<String, Vec<WorkerRouteRuleV1>> = BTreeMap::new();
    for rule in rules {
        let node = nodes
            .get(rule.node_id.as_str())
            .ok_or_else(|| PlanError::InvalidRoute(rule.route_id.to_string()))?;
        let transition = transitions
            .get(rule.destination_transition.as_str())
            .ok_or_else(|| PlanError::InvalidRoute(rule.route_id.to_string()))?;
        if node.executor != WorkerExecutorKindV1::Router
            || transition.from_node != rule.node_id
            || !ids.insert(rule.route_id.as_str())
            || serde_json::to_vec(&rule.predicate)
                .map_err(|_| PlanError::Encoding)?
                .len()
                > 64 * 1024
        {
            return Err(PlanError::InvalidRoute(rule.route_id.to_string()));
        }
        routes
            .entry(rule.node_id.as_str().to_owned())
            .or_default()
            .push(rule.clone());
    }
    for rules in routes.values_mut() {
        rules.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.route_id.as_str().cmp(right.route_id.as_str()))
        });
    }
    Ok(routes)
}

fn validate_v1_cycles(
    nodes: &BTreeMap<String, WorkerNodeV1>,
    outgoing: &BTreeMap<String, Vec<WorkerTransitionV1>>,
    _loops: &BTreeMap<String, WorkerLoopDescriptorV1>,
) -> Result<(), PlanError> {
    fn visit(
        node: &str,
        outgoing: &BTreeMap<String, Vec<WorkerTransitionV1>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), PlanError> {
        if visited.contains(node) {
            return Ok(());
        }
        visiting.insert(node.to_owned());
        if let Some(edges) = outgoing.get(node) {
            // A declared loop transition is the only legal back edge. Removing
            // all such edges must leave a DAG; this is independent of DFS node
            // order and is stronger than accepting whichever edge happens to
            // close a traversal cycle.
            for edge in edges.iter().filter(|edge| edge.declared_loop_id.is_none()) {
                if visiting.contains(edge.to_node.as_str()) {
                    return Err(PlanError::UnboundedCycle(edge.transition_id.to_string()));
                } else {
                    visit(edge.to_node.as_str(), outgoing, visiting, visited)?;
                }
            }
        }
        visiting.remove(node);
        visited.insert(node.to_owned());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for node in nodes.keys() {
        visit(node, outgoing, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_legacy_cycles(
    nodes: &BTreeMap<String, PlanNode>,
    outgoing: &BTreeMap<String, Vec<Transition>>,
) -> Result<(), PlanError> {
    fn visit(
        node: &str,
        outgoing: &BTreeMap<String, Vec<Transition>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), PlanError> {
        if visited.contains(node) {
            return Ok(());
        }
        visiting.insert(node.to_owned());
        if let Some(edges) = outgoing.get(node) {
            for edge in edges {
                if visiting.contains(edge.to.as_str()) {
                    if edge.loop_bound.unwrap_or(0) == 0 {
                        return Err(PlanError::UnboundedCycle(edge.id.to_string()));
                    }
                } else {
                    visit(edge.to.as_str(), outgoing, visiting, visited)?;
                }
            }
        }
        visiting.remove(node);
        visited.insert(node.to_owned());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for node in nodes.keys() {
        visit(node, outgoing, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn legacy_fingerprint(snapshot: &FrozenRunSnapshot) -> Result<String, PlanError> {
    let mut nodes = snapshot.nodes.clone();
    nodes.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut transitions = snapshot.transitions.clone();
    transitions.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let bytes = serde_json::to_vec(&(
        snapshot.snapshot_id.as_str(),
        &snapshot.snapshot_hash,
        snapshot.generation,
        nodes,
        transitions,
    ))
    .map_err(|_| PlanError::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn plan_fingerprint(snapshot: &WorkerFrozenRunSnapshotV1) -> Result<String, PlanError> {
    let bytes = serde_jcs::to_vec(&(WORKER_COMPILER_VERSION, snapshot, "deterministic-plan-v1"))
        .map_err(|_| PlanError::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn canonicalize_snapshot(snapshot: &mut WorkerFrozenRunSnapshotV1) {
    snapshot
        .nodes
        .sort_by(|left, right| left.node_id.as_str().cmp(right.node_id.as_str()));
    for node in &mut snapshot.nodes {
        node.inputs
            .sort_by(|left, right| left.name.cmp(&right.name));
        node.outputs
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
    snapshot.transitions.sort_by(|left, right| {
        left.transition_id
            .as_str()
            .cmp(right.transition_id.as_str())
    });
    snapshot
        .entry_nodes
        .sort_by(|left, right| left.as_str().cmp(right.as_str()));
    snapshot
        .loop_descriptors
        .sort_by(|left, right| left.loop_id.as_str().cmp(right.loop_id.as_str()));
    snapshot
        .join_descriptors
        .sort_by(|left, right| left.join_id.as_str().cmp(right.join_id.as_str()));
    snapshot
        .route_rules
        .sort_by(|left, right| left.route_id.as_str().cmp(right.route_id.as_str()));
    snapshot
        .capability_bindings
        .sort_by(|left, right| left.capability_id.cmp(&right.capability_id));
    snapshot
        .capability_refs
        .sort_by(|left, right| left.as_str().cmp(right.as_str()));
}

fn valid_port_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
