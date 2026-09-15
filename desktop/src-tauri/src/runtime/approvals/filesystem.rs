//! User-granted filesystem access is shared by enabled file tools, independently
//! of each invocation's immutable broker decision and resolved target.
use super::{ApprovalChoice, ApprovalContext, digest};
use aworkit_trusted_core::{ProjectCoordinator, WorkspaceBindingV1};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    Read,
    Write,
}

impl FilesystemAccess {
    pub(crate) fn covers(self, required: Self) -> bool {
        self == Self::Write || self == required
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemLocation {
    Directory,
    AllExternal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemSelection {
    pub access: FilesystemAccess,
    pub location: FilesystemLocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemApprovalRequest {
    pub access: FilesystemAccess,
    pub directory: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "scope",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum FilesystemOwner {
    Chat {
        chat_id: String,
    },
    Project {
        project_key: String,
        project_name: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemGrant {
    pub id: String,
    pub owner: FilesystemOwner,
    pub access: FilesystemAccess,
    /// None explicitly means every external location; never inferred from a tool grant.
    pub directory: Option<WorkspaceBindingV1>,
}

impl FilesystemGrant {
    pub(crate) fn create(
        context: &ApprovalContext,
        choice: &ApprovalChoice,
        selection: &FilesystemSelection,
        required: FilesystemAccess,
        target: &Path,
        projects: &ProjectCoordinator,
    ) -> Result<Self, String> {
        if !selection.access.covers(required) {
            return Err("This operation requires read and write access.".into());
        }
        let owner = match choice {
            ApprovalChoice::AlwaysApproveInProject => FilesystemOwner::Project {
                project_key: context
                    .project_key
                    .clone()
                    .ok_or("Select a project to save a project permission.")?,
                project_name: context
                    .project_name
                    .clone()
                    .unwrap_or_else(|| "Project".into()),
            },
            ApprovalChoice::ApproveForChat if !context.chat_id.is_empty() => {
                FilesystemOwner::Chat {
                    chat_id: context.chat_id.clone(),
                }
            }
            _ => return Err("Filesystem permissions require a chat or project approval.".into()),
        };
        let directory = match selection.location {
            FilesystemLocation::AllExternal => {
                if selection.directory.is_some() {
                    return Err("All external locations cannot include a directory.".into());
                }
                None
            }
            FilesystemLocation::Directory => {
                let path = selection
                    .directory
                    .as_deref()
                    .ok_or("Choose a permission directory.")?;
                if path.len() > 4096 || !Path::new(path).is_absolute() {
                    return Err("Choose an absolute permission directory.".into());
                }
                let directory = projects
                    .resolve_workspace_v1(path)
                    .map_err(|e| e.to_string())?;
                if !target.starts_with(&directory.root) {
                    return Err("The permission directory must contain the requested path.".into());
                }
                Some(directory)
            }
        };
        // Names are presentation only: renaming a project must not duplicate its grant.
        let owner_key = match &owner {
            FilesystemOwner::Chat { chat_id } => ("chat", chat_id),
            FilesystemOwner::Project { project_key, .. } => ("project", project_key),
        };
        let id = digest(&(owner_key, selection.access, &directory));
        Ok(Self {
            id,
            owner,
            access: selection.access,
            directory,
        })
    }

    pub(crate) fn permits(
        &self,
        context: &ApprovalContext,
        required: FilesystemAccess,
        target: &Path,
        projects: &ProjectCoordinator,
    ) -> bool {
        let owner_matches = match &self.owner {
            FilesystemOwner::Chat { chat_id } => chat_id == &context.chat_id,
            FilesystemOwner::Project { project_key, .. } => {
                context.project_key.as_ref() == Some(project_key)
            }
        };
        owner_matches
            && self.access.covers(required)
            && self.directory.as_ref().is_none_or(|directory| {
                target.starts_with(&directory.root)
                    && projects.revalidate_workspace_v1(directory).is_ok()
            })
    }
}
