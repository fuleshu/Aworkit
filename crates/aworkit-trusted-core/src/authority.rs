//! Frozen capability authority and first-input Run snapshot construction.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{
    ExtensionRuntimeBindingV1, FrozenCapabilityBindingV1, HistoryBackendV1,
    PinnedExtensionContributionV1, StableId, WorkerBudgetV1, WorkerExecutorKindV1,
    WorkerFrozenRunSnapshotV1, WorkerJoinDescriptorV1, WorkerLoopDescriptorV1, WorkerNodeV1,
    WorkerRouteRuleV1, WorkerTransitionV1, is_canonical_sha256,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ProjectCoordinator, ProjectError, WorkspaceBinding, project::WorkspaceBindingV1};

/// An exact capability adapter version that may be admitted for one run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityBinding {
    pub capability_id: StableId,
    pub adapter_version: String,
    pub enabled: bool,
    pub compatible: bool,
    pub approval: ApprovalRequirement,
}

/// The approval rule disclosed before the first effect is dispatched.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    Never,
    PerInvocation,
}

/// Immutable, user-visible authority allowed by an individual Chat/Run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorityManifest {
    pub manifest_id: StableId,
    pub capability_bindings: Vec<CapabilityBinding>,
    pub summary: String,
}

/// Inputs whose identities become immutable at the first accepted Chat input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotRequest {
    pub chat_id: StableId,
    pub workflow_id: StableId,
    pub workflow_version: u64,
    pub workflow_hash: String,
    pub workspace: WorkspaceBinding,
    pub capability_bindings: Vec<CapabilityBinding>,
}

/// The deterministic core-owned snapshot passed to a worker generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrozenRunSnapshot {
    pub chat_id: StableId,
    pub workflow_id: StableId,
    pub workflow_version: u64,
    pub workflow_hash: String,
    pub workspace: WorkspaceBinding,
    pub authority: AuthorityManifest,
    pub snapshot_hash: String,
}

/// Performs fail-closed binding and workspace checks before snapshotting.
pub struct SnapshotFreezer;

impl SnapshotFreezer {
    /// Resolves all required bindings and freezes one immutable snapshot.
    pub fn freeze(
        projects: &ProjectCoordinator,
        request: SnapshotRequest,
    ) -> Result<FrozenRunSnapshot, SnapshotError> {
        projects.revalidate_workspace(&request.workspace)?;
        if request.capability_bindings.is_empty() {
            return Err(SnapshotError::NoCapabilities);
        }
        if request
            .capability_bindings
            .iter()
            .any(|binding| !binding.enabled || !binding.compatible)
        {
            return Err(SnapshotError::UnresolvedBinding);
        }
        let mut bindings = request.capability_bindings;
        bindings.sort_by(|left, right| {
            left.capability_id
                .as_str()
                .cmp(right.capability_id.as_str())
        });
        if bindings
            .windows(2)
            .any(|pair| pair[0].capability_id == pair[1].capability_id)
        {
            return Err(SnapshotError::DuplicateCapability);
        }
        let canonical = serde_jcs::to_vec(&(
            request.chat_id.as_str(),
            request.workflow_id.as_str(),
            request.workflow_version,
            &request.workflow_hash,
            &request.workspace.identity,
            &bindings,
        ))
        .map_err(|_| SnapshotError::Encoding)?;
        let digest = format!("{:x}", Sha256::digest(canonical));
        let manifest_id = StableId::parse(format!("manifest.{}", &digest[..24]))
            .map_err(|_| SnapshotError::Encoding)?;
        let summary = format!(
            "{} frozen capability binding(s); {} require per-invocation approval",
            bindings.len(),
            bindings
                .iter()
                .filter(|binding| binding.approval == ApprovalRequirement::PerInvocation)
                .count()
        );
        Ok(FrozenRunSnapshot {
            chat_id: request.chat_id,
            workflow_id: request.workflow_id,
            workflow_version: request.workflow_version,
            workflow_hash: request.workflow_hash,
            workspace: request.workspace,
            authority: AuthorityManifest {
                manifest_id,
                capability_bindings: bindings,
                summary,
            },
            snapshot_hash: digest,
        })
    }
}

/// Reasons a first input cannot legally start a mutable run.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Workspace(#[from] ProjectError),
    #[error("a run must declare at least one resolved capability")]
    NoCapabilities,
    #[error("a required capability is disabled, missing, or incompatible")]
    UnresolvedBinding,
    #[error("a capability may appear only once in a frozen authority manifest")]
    DuplicateCapability,
    #[error("the snapshot could not be encoded deterministically")]
    Encoding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityBindingV1 {
    pub capability_id: StableId,
    pub adapter_id: StableId,
    pub adapter_version: String,
    pub descriptor_hash: String,
    pub extension: Option<ExtensionRuntimeBindingV1>,
    pub required_isolation_profile: Option<String>,
    pub enabled: bool,
    pub compatible: bool,
    pub approval: ApprovalRequirement,
    pub allowed_node_types: Vec<String>,
}

impl CapabilityBindingV1 {
    /// Creates an executable binding directly from the core registry's exact
    /// contribution pin. This copies only immutable metadata, never code.
    #[must_use]
    pub fn from_extension_pin(
        pin: &PinnedExtensionContributionV1,
        approval: ApprovalRequirement,
        allowed_node_types: Vec<String>,
    ) -> Self {
        Self {
            capability_id: pin.contribution.descriptor.capability_id.clone(),
            adapter_id: pin.contribution.contribution_id.clone(),
            adapter_version: pin.contribution.descriptor.adapter_version.clone(),
            descriptor_hash: pin.contribution.descriptor.descriptor_hash.clone(),
            extension: Some(pin.runtime_binding()),
            required_isolation_profile: pin.contribution.descriptor.required_isolation.clone(),
            enabled: true,
            compatible: true,
            approval,
            allowed_node_types,
        }
    }

    fn frozen_execution_binding(&self) -> FrozenCapabilityBindingV1 {
        FrozenCapabilityBindingV1 {
            capability_id: self.capability_id.clone(),
            adapter_id: self.adapter_id.clone(),
            adapter_version: self.adapter_version.clone(),
            descriptor_hash: self.descriptor_hash.clone(),
            extension: self.extension.clone(),
            required_isolation_profile: self.required_isolation_profile.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityManifestV1 {
    pub manifest_id: StableId,
    pub manifest_hash: String,
    pub capability_bindings: Vec<CapabilityBindingV1>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotRequestV1 {
    pub snapshot_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub workflow_hash: String,
    pub nodes: Vec<WorkerNodeV1>,
    pub transitions: Vec<WorkerTransitionV1>,
    pub entry_nodes: Vec<StableId>,
    pub loop_descriptors: Vec<WorkerLoopDescriptorV1>,
    pub join_descriptors: Vec<WorkerJoinDescriptorV1>,
    pub route_rules: Vec<WorkerRouteRuleV1>,
    pub workspace: WorkspaceBindingV1,
    pub capability_bindings: Vec<CapabilityBindingV1>,
    pub budget: WorkerBudgetV1,
    pub history_mode: HistoryBackendV1,
}

/// Core-owned freezer for the exact process-neutral worker DTO.
pub struct SnapshotFreezerV1;

impl SnapshotFreezerV1 {
    pub fn freeze(
        projects: &ProjectCoordinator,
        request: SnapshotRequestV1,
    ) -> Result<(WorkerFrozenRunSnapshotV1, AuthorityManifestV1), SnapshotErrorV1> {
        projects.revalidate_workspace_v1(&request.workspace)?;
        validate_budget(&request.budget)?;
        let calculated_workflow_hash = workflow_graph_hash_v1(
            &request.nodes,
            &request.transitions,
            &request.entry_nodes,
            &request.loop_descriptors,
            &request.join_descriptors,
            &request.route_rules,
        )?;
        if request.workflow_hash != calculated_workflow_hash {
            return Err(SnapshotErrorV1::WorkflowHashMismatch);
        }

        let mut bindings = request.capability_bindings;
        bindings.sort_by(|left, right| {
            left.capability_id
                .as_str()
                .cmp(right.capability_id.as_str())
        });
        let mut binding_ids = BTreeSet::new();
        for binding in &mut bindings {
            binding.allowed_node_types.sort();
            binding.allowed_node_types.dedup();
            if !binding_ids.insert(binding.capability_id.as_str()) {
                return Err(SnapshotErrorV1::DuplicateCapability);
            }
            validate_binding(binding)?;
        }

        let binding_map: BTreeMap<_, _> = bindings
            .iter()
            .map(|binding| (binding.capability_id.as_str(), binding))
            .collect();
        for node in &request.nodes {
            match (&node.executor, &node.capability_ref) {
                (
                    WorkerExecutorKindV1::Brokered
                    | WorkerExecutorKindV1::Model
                    | WorkerExecutorKindV1::Agent,
                    Some(capability),
                ) => {
                    let binding = binding_map
                        .get(capability.as_str())
                        .ok_or_else(|| SnapshotErrorV1::UnresolvedCapability(capability.clone()))?;
                    if !binding.allowed_node_types.is_empty()
                        && !binding.allowed_node_types.contains(&node.node_type)
                    {
                        return Err(SnapshotErrorV1::NodeTypeDenied(node.node_id.clone()));
                    }
                }
                (
                    WorkerExecutorKindV1::Brokered
                    | WorkerExecutorKindV1::Model
                    | WorkerExecutorKindV1::Agent,
                    None,
                ) => return Err(SnapshotErrorV1::NodeCapabilityMissing(node.node_id.clone())),
                (_, Some(capability)) if !binding_map.contains_key(capability.as_str()) => {
                    return Err(SnapshotErrorV1::UnresolvedCapability(capability.clone()));
                }
                _ => {}
            }
        }

        let manifest_bytes = serde_jcs::to_vec(&bindings).map_err(|_| SnapshotErrorV1::Encoding)?;
        let manifest_hash = format!("{:x}", Sha256::digest(manifest_bytes));
        let manifest_id = StableId::parse(format!("manifest.{}", &manifest_hash[..32]))
            .map_err(|_| SnapshotErrorV1::Encoding)?;
        let frozen_capability_bindings = bindings
            .iter()
            .map(CapabilityBindingV1::frozen_execution_binding)
            .collect::<Vec<_>>();
        let manifest = AuthorityManifestV1 {
            manifest_id: manifest_id.clone(),
            manifest_hash: manifest_hash.clone(),
            summary: format!(
                "{} exact capability binding(s); {} require per-invocation approval",
                bindings.len(),
                bindings
                    .iter()
                    .filter(|binding| binding.approval == ApprovalRequirement::PerInvocation)
                    .count()
            ),
            capability_bindings: bindings,
        };
        let capability_refs = manifest
            .capability_bindings
            .iter()
            .map(|binding| binding.capability_id.clone())
            .collect();
        let workspace_identity = serde_json::to_value(&request.workspace.identity)
            .map_err(|_| SnapshotErrorV1::Encoding)?;
        let mut snapshot = WorkerFrozenRunSnapshotV1 {
            snapshot_id: request.snapshot_id,
            snapshot_hash: String::new(),
            chat_id: request.chat_id,
            run_id: request.run_id,
            schema_version: 1,
            compiler_version: "aworkit-worker-v1".to_owned(),
            workflow_hash: request.workflow_hash,
            nodes: request.nodes,
            transitions: request.transitions,
            entry_nodes: request.entry_nodes,
            loop_descriptors: request.loop_descriptors,
            join_descriptors: request.join_descriptors,
            route_rules: request.route_rules,
            authority_manifest_ref: manifest_id,
            authority_manifest_hash: manifest_hash,
            capability_bindings: frozen_capability_bindings,
            capability_refs,
            workspace_identity,
            budget: request.budget,
            history_mode: request.history_mode,
        };
        snapshot.snapshot_hash = snapshot_hash_v1(&snapshot)?;
        Ok((snapshot, manifest))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalGrantV1 {
    pub approval_id: StableId,
    pub invocation_id: StableId,
    pub authority_manifest_ref: StableId,
    pub expires_at_tick: u64,
    pub constraints: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalDecisionV1 {
    NotRequired,
    Approved { approval_id: StableId },
    Required,
    Denied,
    Expired,
    AlreadyConsumed,
}

#[derive(Debug, Default)]
pub struct ApprovalEngineV1 {
    consumed: BTreeSet<String>,
    denied_invocations: BTreeSet<String>,
}

impl ApprovalEngineV1 {
    pub fn deny(&mut self, invocation_id: &StableId) {
        self.denied_invocations
            .insert(invocation_id.as_str().to_owned());
    }

    pub fn authorize(
        &mut self,
        binding: &CapabilityBindingV1,
        manifest: &AuthorityManifestV1,
        invocation_id: &StableId,
        current_tick: u64,
        grant: Option<&ApprovalGrantV1>,
    ) -> ApprovalDecisionV1 {
        if self.denied_invocations.contains(invocation_id.as_str()) {
            return ApprovalDecisionV1::Denied;
        }
        if binding.approval == ApprovalRequirement::Never {
            return ApprovalDecisionV1::NotRequired;
        }
        let Some(grant) = grant else {
            return ApprovalDecisionV1::Required;
        };
        if grant.invocation_id != *invocation_id
            || grant.authority_manifest_ref != manifest.manifest_id
        {
            return ApprovalDecisionV1::Denied;
        }
        if current_tick >= grant.expires_at_tick {
            return ApprovalDecisionV1::Expired;
        }
        if !self.consumed.insert(grant.approval_id.as_str().to_owned()) {
            return ApprovalDecisionV1::AlreadyConsumed;
        }
        ApprovalDecisionV1::Approved {
            approval_id: grant.approval_id.clone(),
        }
    }
}

pub fn workflow_graph_hash_v1(
    nodes: &[WorkerNodeV1],
    transitions: &[WorkerTransitionV1],
    entry_nodes: &[StableId],
    loops: &[WorkerLoopDescriptorV1],
    joins: &[WorkerJoinDescriptorV1],
    routes: &[WorkerRouteRuleV1],
) -> Result<String, SnapshotErrorV1> {
    let mut nodes = nodes.to_vec();
    let mut transitions = transitions.to_vec();
    let mut entry_nodes = entry_nodes.to_vec();
    let mut loops = loops.to_vec();
    let mut joins = joins.to_vec();
    let mut routes = routes.to_vec();
    nodes.sort_by(|left, right| left.node_id.as_str().cmp(right.node_id.as_str()));
    for node in &mut nodes {
        node.inputs
            .sort_by(|left, right| left.name.cmp(&right.name));
        node.outputs
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
    transitions.sort_by(|left, right| {
        left.transition_id
            .as_str()
            .cmp(right.transition_id.as_str())
    });
    entry_nodes.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    loops.sort_by(|left, right| left.loop_id.as_str().cmp(right.loop_id.as_str()));
    joins.sort_by(|left, right| left.join_id.as_str().cmp(right.join_id.as_str()));
    // `expected_branches` is a declared reconciliation order and is therefore
    // semantic data, not a set. Sorting it would make workflows with different
    // ordered-join results share one graph identity.
    routes.sort_by(|left, right| {
        left.node_id
            .as_str()
            .cmp(right.node_id.as_str())
            .then_with(|| left.priority.cmp(&right.priority))
            .then_with(|| left.route_id.as_str().cmp(right.route_id.as_str()))
    });
    let bytes = serde_jcs::to_vec(&(nodes, transitions, entry_nodes, loops, joins, routes))
        .map_err(|_| SnapshotErrorV1::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn snapshot_hash_v1(snapshot: &WorkerFrozenRunSnapshotV1) -> Result<String, SnapshotErrorV1> {
    let mut canonical = snapshot.clone();
    canonical.snapshot_hash.clear();
    canonical
        .nodes
        .sort_by(|left, right| left.node_id.as_str().cmp(right.node_id.as_str()));
    for node in &mut canonical.nodes {
        node.inputs
            .sort_by(|left, right| left.name.cmp(&right.name));
        node.outputs
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
    canonical.transitions.sort_by(|left, right| {
        left.transition_id
            .as_str()
            .cmp(right.transition_id.as_str())
    });
    canonical
        .entry_nodes
        .sort_by(|left, right| left.as_str().cmp(right.as_str()));
    canonical
        .loop_descriptors
        .sort_by(|left, right| left.loop_id.as_str().cmp(right.loop_id.as_str()));
    canonical
        .join_descriptors
        .sort_by(|left, right| left.join_id.as_str().cmp(right.join_id.as_str()));
    canonical
        .route_rules
        .sort_by(|left, right| left.route_id.as_str().cmp(right.route_id.as_str()));
    canonical
        .capability_bindings
        .sort_by(|left, right| left.capability_id.cmp(&right.capability_id));
    canonical
        .capability_refs
        .sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let bytes = serde_jcs::to_vec(&canonical).map_err(|_| SnapshotErrorV1::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_binding(binding: &CapabilityBindingV1) -> Result<(), SnapshotErrorV1> {
    let invalid_extension = binding.extension.as_ref().is_some_and(|extension| {
        extension.host_generation.0 == 0
            || extension.contribution_id != binding.adapter_id
            || extension.identity.version.trim().is_empty()
            || extension.identity.version.len() > 128
            || !is_canonical_sha256(&extension.identity.content_hash)
            || !is_canonical_sha256(&extension.handshake_hash)
    });
    let invalid_isolation = binding
        .required_isolation_profile
        .as_deref()
        .is_some_and(|profile| {
            profile.is_empty()
                || profile.len() > 256
                || profile.trim() != profile
                || profile.chars().any(char::is_control)
        });
    if !binding.enabled
        || !binding.compatible
        || binding.adapter_version.trim().is_empty()
        || binding.adapter_version.len() > 128
        || !is_canonical_sha256(&binding.descriptor_hash)
        || invalid_extension
        || invalid_isolation
        || binding.allowed_node_types.iter().any(|node_type| {
            node_type.is_empty() || node_type.len() > 128 || node_type.chars().any(char::is_control)
        })
    {
        Err(SnapshotErrorV1::InvalidCapability(
            binding.capability_id.clone(),
        ))
    } else {
        Ok(())
    }
}

fn validate_budget(budget: &WorkerBudgetV1) -> Result<(), SnapshotErrorV1> {
    if budget.turns == 0
        || budget.attempts == 0
        || budget.actions == 0
        || budget.depth > 64
        || budget.fanout > 1_024
        || budget.parallel == 0
        || budget.parallel > budget.fanout.max(1)
        || budget.deadline_ms == 0
    {
        Err(SnapshotErrorV1::InvalidBudget)
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum SnapshotErrorV1 {
    #[error(transparent)]
    Workspace(#[from] ProjectError),
    #[error("workflow hash does not match the exact frozen graph")]
    WorkflowHashMismatch,
    #[error("a capability is duplicated")]
    DuplicateCapability,
    #[error("capability {0} is disabled, incompatible, or malformed")]
    InvalidCapability(StableId),
    #[error("node {0} requires an explicit capability reference")]
    NodeCapabilityMissing(StableId),
    #[error("capability {0} cannot be resolved exactly")]
    UnresolvedCapability(StableId),
    #[error("node type for {0} is outside its frozen binding")]
    NodeTypeDenied(StableId),
    #[error("run budget is empty or structurally invalid")]
    InvalidBudget,
    #[error("snapshot could not be encoded deterministically")]
    Encoding,
}
