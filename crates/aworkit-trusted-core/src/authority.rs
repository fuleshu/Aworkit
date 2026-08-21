//! Frozen capability authority and first-input Run snapshot construction.

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ProjectCoordinator, ProjectError, WorkspaceBinding};

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
    pub fn freeze(projects: &ProjectCoordinator, request: SnapshotRequest) -> Result<FrozenRunSnapshot, SnapshotError> {
        projects.revalidate_workspace(&request.workspace)?;
        if request.capability_bindings.is_empty() {
            return Err(SnapshotError::NoCapabilities);
        }
        if request.capability_bindings.iter().any(|binding| !binding.enabled || !binding.compatible) {
            return Err(SnapshotError::UnresolvedBinding);
        }
        let mut bindings = request.capability_bindings;
        bindings.sort_by(|left, right| left.capability_id.as_str().cmp(right.capability_id.as_str()));
        if bindings.windows(2).any(|pair| pair[0].capability_id == pair[1].capability_id) {
            return Err(SnapshotError::DuplicateCapability);
        }
        let canonical = serde_json::to_vec(&(request.chat_id.as_str(), request.workflow_id.as_str(), request.workflow_version, &request.workflow_hash, &request.workspace.identity, &bindings)).map_err(|_| SnapshotError::Encoding)?;
        let digest = format!("{:x}", Sha256::digest(canonical));
        let manifest_id = StableId::parse(format!("manifest.{}", &digest[..24])).map_err(|_| SnapshotError::Encoding)?;
        let summary = format!("{} frozen capability binding(s); {} require per-invocation approval", bindings.len(), bindings.iter().filter(|binding| binding.approval == ApprovalRequirement::PerInvocation).count());
        Ok(FrozenRunSnapshot { chat_id: request.chat_id, workflow_id: request.workflow_id, workflow_version: request.workflow_version, workflow_hash: request.workflow_hash, workspace: request.workspace, authority: AuthorityManifest { manifest_id, capability_bindings: bindings, summary }, snapshot_hash: digest })
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
