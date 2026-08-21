//! Project, workspace, and canonical-document coordination.

use std::{fs, path::{Path, PathBuf}};

use aworkit_local_store::{DocumentKind, DocumentRepository, JsonDocument, RepositoryError, RepositoryRoot};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A revalidated workspace identity.  It is an identity fact, not a sandbox.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    /// The canonical filesystem root observed while resolving the workspace.
    pub root: PathBuf,
    /// Stable metadata used to reject a deleted/replaced workspace at freeze.
    pub identity: WorkspaceIdentity,
}

/// Platform-neutral workspace metadata sufficient for conservative drift checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceIdentity {
    /// Canonical path encoded for diagnostics and stable comparisons.
    pub canonical_path: String,
    /// Best-effort creation timestamp, if the platform provides it.
    pub created_at_nanos: Option<u128>,
}

/// Canonical JSON and workspace operations made available to the desktop API.
#[derive(Clone)]
pub struct ProjectCoordinator {
    repository: RepositoryRoot,
}

impl ProjectCoordinator {
    /// Opens the canonical document repository rooted in application-owned state.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ProjectError> {
        Ok(Self { repository: RepositoryRoot::open(root.into())? })
    }

    /// Revalidates an existing directory before it can be frozen into a run.
    pub fn resolve_workspace(&self, root: impl AsRef<Path>) -> Result<WorkspaceBinding, ProjectError> {
        let canonical = fs::canonicalize(root.as_ref()).map_err(|_| ProjectError::WorkspaceUnavailable)?;
        let metadata = fs::metadata(&canonical).map_err(|_| ProjectError::WorkspaceUnavailable)?;
        if !metadata.is_dir() {
            return Err(ProjectError::WorkspaceUnavailable);
        }
        let created_at_nanos = metadata.created().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|duration| duration.as_nanos());
        Ok(WorkspaceBinding {
            identity: WorkspaceIdentity { canonical_path: canonical.to_string_lossy().into_owned(), created_at_nanos },
            root: canonical,
        })
    }

    /// Confirms that a selected workspace has not disappeared or changed identity.
    pub fn revalidate_workspace(&self, binding: &WorkspaceBinding) -> Result<(), ProjectError> {
        let observed = self.resolve_workspace(&binding.root)?;
        if observed.identity != binding.identity {
            return Err(ProjectError::WorkspaceDrift);
        }
        Ok(())
    }

    /// Loads the one canonical editable JSON body for the requested document.
    pub fn load_document(&self, kind: DocumentKind, id: &str) -> Result<Option<(u64, JsonDocument)>, ProjectError> {
        self.repository.load(kind, id).map(|document| document.map(|stored| (stored.version, stored.document))).map_err(ProjectError::from)
    }

    /// Saves JSON only when the caller's document version still matches.
    pub fn save_document(&self, kind: DocumentKind, id: &str, expected_version: Option<u64>, document: &JsonDocument) -> Result<u64, ProjectError> {
        Ok(self.repository.save(kind, id, expected_version, document)?.version)
    }
}

/// Project operations fail closed on ambiguous workspace or document state.
#[derive(Debug, Error)]
pub enum ProjectError {
    /// A selected path is absent, inaccessible, or not a directory.
    #[error("the selected workspace is unavailable")]
    WorkspaceUnavailable,
    /// The workspace identity changed after it was selected.
    #[error("the selected workspace changed and must be selected again")]
    WorkspaceDrift,
    /// Canonical repository operations retain their precise failure reason.
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_revalidation_rejects_deleted_root() {
        let root = std::env::temp_dir().join(format!("aworkit-core-workspace-{}", std::process::id()));
        fs::create_dir_all(&root).expect("root");
        let coordinator = ProjectCoordinator::open(root.join("state")).expect("coordinator");
        let binding = coordinator.resolve_workspace(&root).expect("binding");
        fs::remove_dir_all(&root).expect("cleanup");
        assert!(matches!(coordinator.revalidate_workspace(&binding), Err(ProjectError::WorkspaceUnavailable)));
    }
}
