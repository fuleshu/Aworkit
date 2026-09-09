//! Persistent, root-confined working folders for Chats without a saved project.
use std::{
    fs,
    path::{Path, PathBuf},
};

use aworkit_protocol::StableId;
use aworkit_trusted_core::{ProjectCoordinator, WorkspaceBindingV1};

pub(crate) struct ChatWorkspaceStore {
    root: PathBuf,
}

impl ChatWorkspaceStore {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join("chat-workspaces"),
        }
    }

    /// Resolve once at first send; later turns use the frozen filesystem identity.
    /// Forks receive their own empty folder, just like other newly created Chats.
    pub(crate) fn create(
        &self,
        coordinator: &ProjectCoordinator,
        chat_id: &StableId,
    ) -> Result<WorkspaceBindingV1, String> {
        fs::create_dir_all(&self.root)
            .map_err(|error| format!("cannot create Chat workspace storage: {error}"))?;
        let root = fs::canonicalize(&self.root).map_err(|error| error.to_string())?;
        let path = root.join(chat_id.as_str());
        fs::create_dir_all(&path)
            .map_err(|error| format!("cannot create Chat working folder: {error}"))?;
        let binding = coordinator
            .resolve_workspace_v1(&path)
            .map_err(|error| error.to_string())?;
        if binding.root.parent() != Some(root.as_path()) {
            return Err("Chat working folder must remain inside Chat workspace storage".into());
        }
        validate_chat_workspace(&binding, chat_id)?;
        Ok(binding)
    }
}

/// Validate the stored shape without touching disk during history projection.
pub(crate) fn validate_chat_workspace(
    workspace: &WorkspaceBindingV1,
    chat_id: &StableId,
) -> Result<(), String> {
    if !workspace.root.is_absolute()
        || workspace.root.to_str() != Some(workspace.identity.canonical_path.as_str())
        || workspace.root.file_name().and_then(|name| name.to_str()) != Some(chat_id.as_str())
        || workspace.identity.filesystem_object_id.is_empty()
        || workspace.identity.platform.is_empty()
    {
        return Err("stored frozen Chat working folder failed integrity validation".into());
    }
    Ok(())
}
