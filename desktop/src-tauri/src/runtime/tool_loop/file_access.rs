//! Per-invocation file locations, frozen before approval and revalidated at dispatch.
//! The existing directory-capability adapter remains the execution boundary.

use super::*;
use std::path::Component;

/// These additive provider hints do not change the host envelope contract.
/// Retain the descriptor fingerprint so old frozen manifests still admit.
pub(super) fn descriptor_schema(id: &str, mut schema: Value) -> Value {
    if id == image_tools::READ {
        schema["properties"]["path"]["description"] =
            json!("Absolute local image file path, or a path relative to the Chat workspace.");
    }
    if id.starts_with("tool.files.") {
        if matches!(id, FILE_LIST_CAPABILITY_ID | FILE_GREP_CAPABILITY_ID) {
            schema["properties"].as_object_mut().unwrap().remove("path");
        } else if let Some(path) = schema["properties"]["path"].as_object_mut() {
            path.remove("description");
        }
    }
    schema
}

/// Return usable absolute entries when the caller selected a directory.
pub(super) fn describe_result(
    record: &ToolInvocationRecordV1,
    value: &mut Value,
) -> Result<(), String> {
    let Some(Ok(access)) = &record.file_access else {
        return Ok(());
    };
    if matches!(
        record.call.capability_id.as_str(),
        FILE_LIST_CAPABILITY_ID | FILE_GREP_CAPABILITY_ID
    ) {
        value["path"] = json!(access.target());
        if record.call.arguments.get("path").is_some() {
            for key in ["entries", "matches"] {
                if let Some(entries) = value.get_mut(key).and_then(Value::as_array_mut) {
                    for entry in entries {
                        if let Some(path) = entry["path"].as_str() {
                            entry["path"] = json!(access.directory.root.join(path));
                        }
                    }
                }
            }
        }
        enforce_result_bound(value)?;
    }
    Ok(())
}

/// Keep the original target across approval/restart, including a correctable
/// path error. The payload hash binds the target to the broker's exact decision.
pub(super) fn freeze_record(
    record: &mut ToolInvocationRecordV1,
    runtime: &FileToolAuthorityRuntimeV1,
) -> Result<(), WorkflowPipelineError> {
    if record.binding.file_access_version.is_none() {
        return Ok(());
    }
    record.file_access =
        if let Some(existing) = runtime.records.invocation(&record.proposal.proposal_id)? {
            existing.file_access
        } else {
            Some(FileAccess::resolve(
                &runtime.projects,
                &record.workspace,
                &record.call,
            ))
        };
    record.payload["fileAccess"] = serde_json::to_value(&record.file_access).map_err(json_error)?;
    record.proposal.payload_hash = canonical_hash(&record.payload)?;
    Ok(())
}

/// Presence selects the location-based policy; old frozen Chats retain their contract.
pub(super) fn is_file_tool(id: &str) -> bool {
    matches!(
        id,
        FILE_READ_CAPABILITY_ID
            | FILE_SEARCH_CAPABILITY_ID
            | FILE_LIST_CAPABILITY_ID
            | FILE_GREP_CAPABILITY_ID
            | FILE_EDIT_CAPABILITY_ID
            | FILE_WRITE_CAPABILITY_ID
            | image_tools::READ
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FileAccess {
    pub directory: WorkspaceBindingV1,
    pub path: PathBuf,
    pub outside_workspace: bool,
}

impl FileAccess {
    /// Resolve only metadata before approval. Never read file contents here.
    pub fn resolve(
        projects: &ProjectCoordinator,
        workspace: &WorkspaceBindingV1,
        call: &ModelToolCallV1,
    ) -> Result<Self, String> {
        let directory_tool = matches!(
            call.capability_id.as_str(),
            FILE_LIST_CAPABILITY_ID | FILE_GREP_CAPABILITY_ID
        );
        let raw = match call.arguments.get("path") {
            Some(value) => value.as_str().ok_or("File path must be a string")?,
            None if directory_tool => ".",
            None => return Err("File path is required".into()),
        };
        let path = Path::new(raw);
        if raw.is_empty() || raw.len() > 4096 || raw.contains('\0') {
            return Err("File path must be non-empty and at most 4096 bytes".into());
        }
        // Reject ambiguous drive-relative/device paths. Parent components are
        // resolved by the filesystem, including junctions, before classification.
        if !path.is_absolute()
            && path
                .components()
                .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
        {
            return Err("Use a complete absolute path or a workspace-relative path".into());
        }
        #[cfg(windows)]
        if path.components().any(|part| matches!(part, Component::Prefix(prefix) if matches!(prefix.kind(), std::path::Prefix::DeviceNS(_) | std::path::Prefix::Verbatim(_)))) {
            return Err("Use an ordinary file path, not a Windows device path".into());
        }
        let target = if path.is_absolute() {
            path.to_owned()
        } else {
            workspace.root.join(path)
        };
        // Resolve existing aliases to their actual target. A new file uses its
        // existing parent; write_file does not implicitly create directories.
        let target = match std::fs::canonicalize(&target) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !directory_tool => {
                let parent = target.parent().ok_or("File path has no parent")?;
                std::fs::canonicalize(parent)
                    .map_err(|e| e.to_string())?
                    .join(target.file_name().ok_or("File path must name a file")?)
            }
            Err(error) => return Err(error.to_string()),
        };
        let outside_workspace = !target.starts_with(&workspace.root);
        let (root, path) = if directory_tool {
            (target, PathBuf::from("."))
        } else {
            (
                target.parent().ok_or("File path has no parent")?.to_owned(),
                PathBuf::from(target.file_name().ok_or("File path must name a file")?),
            )
        };
        let directory = projects
            .resolve_workspace_v1(root)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            directory,
            path,
            outside_workspace,
        })
    }

    pub fn target(&self) -> PathBuf {
        if self.path == Path::new(".") {
            self.directory.root.clone()
        } else {
            self.directory.root.join(&self.path)
        }
    }

    /// Hide Windows' canonical namespace marker in the human approval copy.
    pub fn display_target(&self) -> String {
        let path = self.target().to_string_lossy().into_owned();
        #[cfg(windows)]
        {
            if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
                return format!(r"\\{unc}");
            }
            if let Some(disk) = path.strip_prefix(r"\\?\") {
                return disk.to_owned();
            }
        }
        path
    }

    /// Use the reviewed directory identity and relative name, never re-resolve
    /// the caller's alias after approval into a different target.
    pub fn open(
        &self,
        projects: &ProjectCoordinator,
        allow_write: bool,
    ) -> Result<ProjectFiles, String> {
        projects
            .revalidate_workspace_v1(&self.directory)
            .map_err(|e| e.to_string())?;
        let files = ProjectFiles::new(FileAuthority {
            root: self.directory.root.clone(),
            allow_write,
        })
        .map_err(|e| e.to_string())?;
        projects
            .revalidate_workspace_v1(&self.directory)
            .map_err(|e| e.to_string())?;
        Ok(files)
    }
}
