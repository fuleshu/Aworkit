//! Resolution and integrity checks for the project selected before a Chat starts.
//!
//! A saved project is only a configuration record. The trusted desktop runtime
//! resolves its workspace again at first send and freezes the resulting native
//! identity. Later Settings edits therefore cannot retarget an active Chat.

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use aworkit_trusted_core::{ProjectCoordinator, WorkspaceBindingV1};
use serde::{Deserialize, Serialize};

use super::{
    dto::ProjectChoiceDto,
    history::canonical_hash,
    settings_v2::{ProjectConfigurationV2, WorkspaceKindV2},
};

const MAXIMUM_GIT_POINTER_BYTES: u64 = 4 * 1024;
const MAXIMUM_BRANCH_BYTES: usize = 1024;

/// Immutable, secret-free project and native workspace authority resolved at
/// the first input of a Chat/Run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenProjectScopeV1 {
    pub project_id: String,
    pub project_name: String,
    pub project_configuration_hash: String,
    pub project_snapshot: ProjectConfigurationV2,
    pub workspace_kind: WorkspaceKindV2,
    pub workspace_binding: WorkspaceBindingV1,
    pub workspace_identity_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Projects that the current native Simple Chat slice can select. Remote
/// records stay configurable in Settings but require a future remote adapter.
pub(crate) fn selectable_projects(projects: &[ProjectConfigurationV2]) -> Vec<ProjectChoiceDto> {
    projects
        .iter()
        .filter(|project| project.workspace.kind != WorkspaceKindV2::Remote)
        .map(|project| ProjectChoiceDto {
            project_id: project.id.clone(),
            name: project.name.clone(),
            workspace_kind: project.workspace.kind,
        })
        .collect()
}

/// Resolves one exact saved selection without accepting a renderer-supplied
/// path or project body.
pub(crate) fn resolve_project_scope(
    coordinator: &ProjectCoordinator,
    projects: &[ProjectConfigurationV2],
    selected_project_id: Option<&str>,
) -> Result<Option<FrozenProjectScopeV1>, String> {
    let Some(selected_project_id) = selected_project_id else {
        return Ok(None);
    };
    if selected_project_id.trim().is_empty() {
        return Err("projectId must be null or a non-empty saved project ID".into());
    }
    let project = projects
        .iter()
        .find(|project| project.id == selected_project_id)
        .ok_or_else(|| format!("selected project '{selected_project_id}' is not saved"))?;
    if project.workspace.kind == WorkspaceKindV2::Remote {
        return Err(format!(
            "project '{}' uses a remote workspace, but no remote-workspace adapter is installed",
            project.name
        ));
    }
    let workspace_binding = coordinator
        .resolve_workspace_v1(&project.workspace.location)
        .map_err(|_| {
            format!(
                "project '{}' workspace is unavailable or cannot be resolved",
                project.name
            )
        })?;
    let branch = if project.workspace.kind == WorkspaceKindV2::GitWorktree {
        Some(
            resolve_git_branch(&workspace_binding.root).map_err(|message| {
                format!(
                    "project '{}' Git worktree is invalid: {message}",
                    project.name
                )
            })?,
        )
    } else {
        None
    };
    let scope = FrozenProjectScopeV1 {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        project_configuration_hash: canonical_hash(project)?,
        project_snapshot: project.clone(),
        workspace_kind: project.workspace.kind,
        workspace_identity_hash: canonical_hash(&workspace_binding.identity)?,
        workspace_binding,
        branch,
    };
    validate_frozen_project_scope(&scope)?;
    Ok(Some(scope))
}

pub(crate) fn validate_frozen_project_scope(scope: &FrozenProjectScopeV1) -> Result<(), String> {
    let root = scope.workspace_binding.root.to_string_lossy();
    let identity = &scope.workspace_binding.identity;
    let branch_invalid = scope.branch.as_ref().is_some_and(|branch| {
        branch.trim().is_empty()
            || branch.len() > MAXIMUM_BRANCH_BYTES
            || branch.chars().any(char::is_control)
    });
    if scope.project_id != scope.project_snapshot.id
        || scope.project_name != scope.project_snapshot.name
        || scope.workspace_kind != scope.project_snapshot.workspace.kind
        || root != identity.canonical_path
        || canonical_hash(&scope.project_snapshot)? != scope.project_configuration_hash
        || canonical_hash(identity)? != scope.workspace_identity_hash
        || !is_sha256(&scope.project_configuration_hash)
        || !is_sha256(&scope.workspace_identity_hash)
        || (scope.workspace_kind == WorkspaceKindV2::GitWorktree) != scope.branch.is_some()
        || branch_invalid
    {
        return Err("stored frozen project scope failed integrity validation".into());
    }
    Ok(())
}

pub(crate) fn resolve_git_branch(workspace_root: &Path) -> Result<String, String> {
    let dot_git = workspace_root.join(".git");
    let metadata = fs::symlink_metadata(&dot_git)
        .map_err(|_| "the selected root has no .git identity".to_owned())?;
    if metadata.file_type().is_symlink() {
        return Err("the .git identity must not be a symbolic link".into());
    }
    let head = if metadata.is_dir() {
        dot_git.join("HEAD")
    } else if metadata.is_file() {
        linked_worktree_head(workspace_root, &dot_git)?
    } else {
        return Err("the .git identity is neither a directory nor a worktree pointer".into());
    };
    let value = read_bounded_text(&head)?;
    parse_head(&value)
}

/// Rechecks the exact Git HEAD label frozen at first send. Workspace inode and
/// canonical-path validation alone cannot detect a branch switch in place.
pub(crate) fn revalidate_git_branch(
    workspace_root: &Path,
    expected_branch: &str,
) -> Result<(), String> {
    let actual = resolve_git_branch(workspace_root)?;
    if actual == expected_branch {
        Ok(())
    } else {
        Err(format!(
            "Git HEAD drifted from frozen branch '{expected_branch}' to '{actual}'"
        ))
    }
}

fn linked_worktree_head(workspace_root: &Path, dot_git: &Path) -> Result<PathBuf, String> {
    let pointer = read_bounded_text(dot_git)?;
    let target = pointer
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .ok_or_else(|| "the .git worktree pointer is malformed".to_owned())?;
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        workspace_root.join(target)
    };
    let canonical = fs::canonicalize(target)
        .map_err(|_| "the linked Git metadata directory is unavailable".to_owned())?;
    if !canonical.is_dir() {
        return Err("the linked Git metadata target is not a directory".into());
    }
    Ok(canonical.join("HEAD"))
}

fn read_bounded_text(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| "Git HEAD is unavailable".to_owned())?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAXIMUM_GIT_POINTER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Git HEAD cannot be read".to_owned())?;
    if bytes.is_empty() || bytes.len() as u64 > MAXIMUM_GIT_POINTER_BYTES || bytes.contains(&0) {
        return Err("Git HEAD is empty, oversized, or malformed".into());
    }
    String::from_utf8(bytes)
        .map(|value| value.trim().to_owned())
        .map_err(|_| "Git HEAD is not UTF-8".into())
}

fn parse_head(value: &str) -> Result<String, String> {
    if let Some(reference) = value.strip_prefix("ref:").map(str::trim) {
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        if branch.is_empty()
            || branch.len() > MAXIMUM_BRANCH_BYTES
            || branch.chars().any(char::is_control)
        {
            return Err("Git HEAD names an invalid branch".into());
        }
        return Ok(branch.to_owned());
    }
    if (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(format!("detached@{}", &value[..value.len().min(12)]));
    }
    Err("Git HEAD is neither a branch reference nor a detached commit".into())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project(root: &Path, kind: WorkspaceKindV2) -> ProjectConfigurationV2 {
        ProjectConfigurationV2 {
            id: "project.fixture".into(),
            name: "Fixture".into(),
            workspace: super::super::settings_v2::WorkspaceConfigurationV2 {
                kind,
                location: root.to_string_lossy().into_owned(),
            },
            default_workflow_id: Some("workflow.simple-chat".into()),
            portable_history_enabled: false,
        }
    }

    #[test]
    fn resolves_canonical_local_identity_and_rejects_remote_or_missing_roots() {
        let root = TempDir::new().unwrap();
        let coordinator = ProjectCoordinator::open(root.path().join("state")).unwrap();
        let local = project(root.path(), WorkspaceKindV2::LocalDirectory);
        let frozen =
            resolve_project_scope(&coordinator, std::slice::from_ref(&local), Some(&local.id))
                .unwrap()
                .unwrap();
        assert_eq!(frozen.project_id, local.id);
        assert_eq!(
            frozen.workspace_binding.identity.canonical_path,
            fs::canonicalize(root.path()).unwrap().to_string_lossy()
        );
        assert!(frozen.workspace_identity_hash.starts_with("sha256:"));

        let remote = project(root.path(), WorkspaceKindV2::Remote);
        assert!(
            resolve_project_scope(&coordinator, &[remote], Some("project.fixture"))
                .unwrap_err()
                .contains("remote-workspace adapter")
        );
        let missing = project(
            &root.path().join("missing"),
            WorkspaceKindV2::LocalDirectory,
        );
        assert!(
            resolve_project_scope(&coordinator, &[missing], Some("project.fixture"))
                .unwrap_err()
                .contains("unavailable")
        );
    }

    #[test]
    fn freezes_normal_and_detached_git_head_labels() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(
            root.path().join(".git/HEAD"),
            b"ref: refs/heads/feature/project-scope\n",
        )
        .unwrap();
        let coordinator = ProjectCoordinator::open(root.path().join("state")).unwrap();
        let git = project(root.path(), WorkspaceKindV2::GitWorktree);
        let frozen = resolve_project_scope(&coordinator, &[git], Some("project.fixture"))
            .unwrap()
            .unwrap();
        assert_eq!(frozen.branch.as_deref(), Some("feature/project-scope"));

        fs::write(root.path().join(".git/HEAD"), "a".repeat(40)).unwrap();
        let detached = project(root.path(), WorkspaceKindV2::GitWorktree);
        let frozen = resolve_project_scope(&coordinator, &[detached], Some("project.fixture"))
            .unwrap()
            .unwrap();
        assert_eq!(frozen.branch.as_deref(), Some("detached@aaaaaaaaaaaa"));
    }

    #[test]
    fn branch_revalidation_detects_in_place_head_drift() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(
            root.path().join(".git/HEAD"),
            b"ref: refs/heads/feature/frozen\n",
        )
        .unwrap();
        revalidate_git_branch(root.path(), "feature/frozen").unwrap();

        fs::write(
            root.path().join(".git/HEAD"),
            b"ref: refs/heads/feature/drifted\n",
        )
        .unwrap();
        assert!(
            revalidate_git_branch(root.path(), "feature/frozen")
                .unwrap_err()
                .contains("drifted")
        );
    }
}
