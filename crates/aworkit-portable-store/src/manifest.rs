//! Immutable repository, session, and branch metadata.

use serde::{Deserialize, Serialize};

/// Version negotiated before a portable repository is exposed for writing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositoryManifest {
    pub family: String,
    pub major: u16,
    pub minor: u16,
    pub required_features: Vec<String>,
}
/// A persistent Chat/Run identity and its scrubbed frozen snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionManifest {
    pub session_id: String,
    pub chat_id: String,
    pub run_id: String,
    pub frozen_snapshot_hash: String,
    pub canonical_branch_id: String,
    pub export_policy_hash: String,
}
/// Immutable lineage; concurrent successors are branches, never time-selected tips.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BranchManifest {
    pub branch_id: String,
    pub session_id: String,
    pub parent_branch_id: Option<String>,
    pub parent_checkpoint_hash: Option<String>,
    pub parent_head_hash: Option<String>,
}
/// The sole mutable branch pointer, guarded by its expected generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BranchRef {
    pub branch_id: String,
    pub head_segment_hash: Option<String>,
    pub checkpoint_hash: Option<String>,
    pub next_ordinal: u64,
    pub generation: u64,
    pub commit_id: Option<String>,
}
