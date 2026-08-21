//! Validation and deterministic compilation of the immutable worker snapshot.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The core-committed input for exactly one worker generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenRunSnapshot {
    pub snapshot_id: StableId,
    pub snapshot_hash: String,
    pub generation: ProcessGeneration,
    pub nodes: Vec<PlanNode>,
    pub transitions: Vec<Transition>,
}

/// A pinned, typed workflow node. `config` is retained as Aworkit-owned JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanNode {
    pub id: StableId,
    pub node_type: String,
    pub version: u16,
    #[serde(default)]
    pub config: Value,
}

/// One statically declared legal graph transition.
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

/// An indexed, immutable view used by every runtime component.
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
}

impl ExecutionPlan {
    /// Validates and compiles the supplied frozen snapshot without consulting live state.
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
        Self::validate_cycles(&nodes, &outgoing)?;
        let fingerprint = format!(
            "{}:{}:{}",
            snapshot.snapshot_id, snapshot.snapshot_hash, generation.0
        );
        Ok(Self {
            snapshot,
            nodes,
            outgoing,
            fingerprint,
        })
    }

    fn validate_cycles(
        nodes: &BTreeMap<String, PlanNode>,
        outgoing: &BTreeMap<String, Vec<Transition>>,
    ) -> Result<(), PlanError> {
        fn visit(
            node: &str,
            nodes: &BTreeMap<String, PlanNode>,
            outgoing: &BTreeMap<String, Vec<Transition>>,
            visiting: &mut BTreeSet<String>,
            visited: &mut BTreeSet<String>,
        ) -> Result<(), PlanError> {
            if !visited.insert(node.to_owned()) {
                return Ok(());
            }
            visiting.insert(node.to_owned());
            if let Some(edges) = outgoing.get(node) {
                for edge in edges {
                    if visiting.contains(edge.to.as_str()) && edge.loop_bound.unwrap_or(0) == 0 {
                        return Err(PlanError::UnboundedCycle(edge.id.to_string()));
                    }
                    if !visiting.contains(edge.to.as_str()) {
                        visit(edge.to.as_str(), nodes, outgoing, visiting, visited)?;
                    }
                }
            }
            visiting.remove(node);
            Ok(())
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for node in nodes.keys() {
            visit(node, nodes, outgoing, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    /// Returns an exact pinned node descriptor.
    #[must_use]
    pub fn node(&self, id: &StableId) -> Option<&PlanNode> {
        self.nodes.get(id.as_str())
    }
    /// Returns transitions in stable priority then ID order.
    #[must_use]
    pub fn outgoing(&self, id: &StableId) -> &[Transition] {
        self.outgoing.get(id.as_str()).map_or(&[], Vec::as_slice)
    }
    /// Returns the immutable compilation identity.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Returns the original immutable snapshot.
    #[must_use]
    pub fn snapshot(&self) -> &FrozenRunSnapshot {
        &self.snapshot
    }
}
