//! Project, workspace identity, and canonical-document coordination ports.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const SUPPORTED_DOCUMENT_SCHEMA: u16 = 1;

/// Compatibility identity retained for the original M04 API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    pub root: PathBuf,
    pub identity: WorkspaceIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceIdentity {
    pub canonical_path: String,
    pub created_at_nanos: Option<u128>,
}

/// Conservative workspace identity frozen by current Run snapshots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceIdentityV1 {
    pub canonical_path: String,
    pub platform: String,
    pub filesystem_object_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceBindingV1 {
    pub root: PathBuf,
    pub identity: WorkspaceIdentityV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDocumentKindV1 {
    ProjectConfig,
    Workflow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDocumentV1 {
    pub schema_version: u16,
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredProjectDocumentV1 {
    pub kind: ProjectDocumentKindV1,
    pub document_id: StableId,
    pub version: u64,
    pub content_hash: String,
    pub document: ProjectDocumentV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectPortErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Storage-process-neutral document boundary. Concrete local-store adapters
/// implement this outside the trusted-core crate.
pub trait ProjectDocumentPort: Send + Sync {
    fn load(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
    ) -> Result<Option<StoredProjectDocumentV1>, ProjectPortErrorV1>;
    fn save(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
        expected_version: Option<u64>,
        document: &ProjectDocumentV1,
    ) -> Result<StoredProjectDocumentV1, ProjectPortErrorV1>;
    fn list(
        &self,
        kind: ProjectDocumentKindV1,
        after_id: Option<&StableId>,
        limit: u32,
    ) -> Result<Vec<StoredProjectDocumentV1>, ProjectPortErrorV1>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectRecordV1 {
    pub project_id: StableId,
    pub display_name: String,
    pub workspace: WorkspaceBindingV1,
    pub config_document_id: StableId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocumentWatchResultV1 {
    Unchanged { version: u64 },
    Changed(StoredProjectDocumentV1),
    Deleted,
}

#[derive(Clone)]
pub struct ProjectCoordinator {
    state_root: PathBuf,
    documents: Option<Arc<dyn ProjectDocumentPort>>,
    projects: Arc<Mutex<BTreeMap<String, ProjectRecordV1>>>,
}

impl ProjectCoordinator {
    /// Opens core-owned coordination state only. Canonical document bytes stay
    /// behind an explicitly injected storage port.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ProjectError> {
        let state_root = root.into();
        fs::create_dir_all(&state_root).map_err(|_| ProjectError::StateUnavailable)?;
        Ok(Self {
            state_root,
            documents: None,
            projects: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn with_document_port(
        root: impl Into<PathBuf>,
        documents: impl ProjectDocumentPort + 'static,
    ) -> Result<Self, ProjectError> {
        let mut coordinator = Self::open(root)?;
        coordinator.documents = Some(Arc::new(documents));
        Ok(coordinator)
    }

    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn resolve_workspace(
        &self,
        root: impl AsRef<Path>,
    ) -> Result<WorkspaceBinding, ProjectError> {
        let canonical = canonical_directory(root.as_ref())?;
        let metadata = fs::metadata(&canonical).map_err(|_| ProjectError::WorkspaceUnavailable)?;
        let created_at_nanos = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Ok(WorkspaceBinding {
            identity: WorkspaceIdentity {
                canonical_path: canonical.to_string_lossy().into_owned(),
                created_at_nanos,
            },
            root: canonical,
        })
    }

    pub fn revalidate_workspace(&self, binding: &WorkspaceBinding) -> Result<(), ProjectError> {
        let observed = self.resolve_workspace(&binding.root)?;
        if observed.identity != binding.identity {
            return Err(ProjectError::WorkspaceDrift);
        }
        Ok(())
    }

    pub fn resolve_workspace_v1(
        &self,
        root: impl AsRef<Path>,
    ) -> Result<WorkspaceBindingV1, ProjectError> {
        let canonical = canonical_directory(root.as_ref())?;
        let handle = same_file::Handle::from_path(&canonical)
            .map_err(|_| ProjectError::WorkspaceUnavailable)?;
        Ok(WorkspaceBindingV1 {
            root: canonical.clone(),
            identity: WorkspaceIdentityV1 {
                canonical_path: canonical.to_string_lossy().into_owned(),
                platform: std::env::consts::OS.to_owned(),
                filesystem_object_id: filesystem_object_id(&handle),
            },
        })
    }

    pub fn revalidate_workspace_v1(
        &self,
        binding: &WorkspaceBindingV1,
    ) -> Result<(), ProjectError> {
        let observed = self.resolve_workspace_v1(&binding.root)?;
        if observed.identity != binding.identity {
            return Err(ProjectError::WorkspaceDrift);
        }
        Ok(())
    }

    pub fn register_project(&self, record: ProjectRecordV1) -> Result<(), ProjectError> {
        validate_display_name(&record.display_name)?;
        self.revalidate_workspace_v1(&record.workspace)?;
        let mut projects = self.projects.lock().map_err(|_| ProjectError::Poisoned)?;
        if projects
            .insert(record.project_id.as_str().to_owned(), record)
            .is_some()
        {
            return Err(ProjectError::DuplicateProject);
        }
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRecordV1>, ProjectError> {
        Ok(self
            .projects
            .lock()
            .map_err(|_| ProjectError::Poisoned)?
            .values()
            .cloned()
            .collect())
    }

    pub fn load_document_v1(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
    ) -> Result<Option<StoredProjectDocumentV1>, ProjectError> {
        let stored = self.document_port()?.load(kind, document_id)?;
        if let Some(stored) = &stored {
            validate_stored_document(kind, document_id, stored)?;
        }
        Ok(stored)
    }

    pub fn save_document_v1(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
        expected_version: Option<u64>,
        document: &ProjectDocumentV1,
    ) -> Result<StoredProjectDocumentV1, ProjectError> {
        validate_document(document)?;
        let stored = self
            .document_port()?
            .save(kind, document_id, expected_version, document)?;
        validate_stored_document(kind, document_id, &stored)?;
        Ok(stored)
    }

    pub fn export_document_v1(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
    ) -> Result<Vec<u8>, ProjectError> {
        let stored = self
            .load_document_v1(kind, document_id)?
            .ok_or(ProjectError::DocumentMissing)?;
        serde_jcs::to_vec(&stored.document).map_err(|_| ProjectError::InvalidDocument)
    }

    pub fn import_document_v1(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
        expected_version: Option<u64>,
        bytes: &[u8],
    ) -> Result<StoredProjectDocumentV1, ProjectError> {
        if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(ProjectError::InvalidDocument);
        }
        let document: ProjectDocumentV1 =
            serde_json::from_slice(bytes).map_err(|_| ProjectError::InvalidDocument)?;
        self.save_document_v1(kind, document_id, expected_version, &document)
    }

    pub fn watch_document_v1(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
        observed_version: Option<u64>,
    ) -> Result<DocumentWatchResultV1, ProjectError> {
        match self.load_document_v1(kind, document_id)? {
            None => Ok(DocumentWatchResultV1::Deleted),
            Some(stored) if Some(stored.version) == observed_version => {
                Ok(DocumentWatchResultV1::Unchanged {
                    version: stored.version,
                })
            }
            Some(stored) => Ok(DocumentWatchResultV1::Changed(stored)),
        }
    }

    fn document_port(&self) -> Result<&dyn ProjectDocumentPort, ProjectError> {
        self.documents
            .as_deref()
            .ok_or(ProjectError::DocumentPortUnavailable)
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ProjectError> {
    let canonical = fs::canonicalize(path).map_err(|_| ProjectError::WorkspaceUnavailable)?;
    if !fs::metadata(&canonical)
        .map_err(|_| ProjectError::WorkspaceUnavailable)?
        .is_dir()
    {
        return Err(ProjectError::WorkspaceUnavailable);
    }
    Ok(canonical)
}

/// Platform object identity for a workspace root: device/inode on Unix and
/// volume serial/file index on Windows, folded through the same `Handle` hash
/// the executable identity uses. Creation-time or directory-size fallbacks are
/// not used because they are not stable enough to detect a swapped directory.
fn filesystem_object_id(handle: &same_file::Handle) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    handle.hash(&mut hasher);
    format!("object:{:016x}", hasher.finish())
}

fn validate_display_name(name: &str) -> Result<(), ProjectError> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        Err(ProjectError::InvalidProject)
    } else {
        Ok(())
    }
}

fn validate_document(document: &ProjectDocumentV1) -> Result<(), ProjectError> {
    if document.schema_version == 0 || document.schema_version > SUPPORTED_DOCUMENT_SCHEMA {
        return Err(ProjectError::UnsupportedDocumentSchema(
            document.schema_version,
        ));
    }
    if serde_json::to_vec(document)
        .map_err(|_| ProjectError::InvalidDocument)?
        .len()
        > MAX_DOCUMENT_BYTES
        || !document.body.is_object()
        || contains_secret_material(&document.body, 0)?
    {
        return Err(ProjectError::InvalidDocument);
    }
    Ok(())
}

fn validate_stored_document(
    kind: ProjectDocumentKindV1,
    document_id: &StableId,
    stored: &StoredProjectDocumentV1,
) -> Result<(), ProjectError> {
    if stored.kind != kind
        || stored.document_id != *document_id
        || stored.version == 0
        || !is_sha256(&stored.content_hash)
    {
        return Err(ProjectError::InvalidPortResponse);
    }
    validate_document(&stored.document)?;
    let calculated = format!(
        "{:x}",
        sha2::Sha256::digest(
            serde_jcs::to_vec(&stored.document).map_err(|_| ProjectError::InvalidDocument)?
        )
    );
    if calculated != stored.content_hash {
        return Err(ProjectError::InvalidPortResponse);
    }
    Ok(())
}

fn contains_secret_material(value: &Value, depth: usize) -> Result<bool, ProjectError> {
    if depth > 64 {
        return Err(ProjectError::InvalidDocument);
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                let compact = normalized.replace('_', "");
                let forbidden = ["password", "secret", "apikey", "accesstoken", "privatekey"];
                if forbidden.iter().any(|needle| compact.contains(needle)) {
                    return Ok(true);
                }
                if contains_secret_material(value, depth + 1)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Array(values) => {
            for value in values {
                if contains_secret_material(value, depth + 1)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("the selected workspace is unavailable")]
    WorkspaceUnavailable,
    #[error("the selected workspace changed and must be selected again")]
    WorkspaceDrift,
    #[error("trusted-core project coordination state is unavailable")]
    StateUnavailable,
    #[error("no canonical document port was configured")]
    DocumentPortUnavailable,
    #[error("canonical document is missing")]
    DocumentMissing,
    #[error("document schema version {0} is unsupported")]
    UnsupportedDocumentSchema(u16),
    #[error("project document is malformed, oversized, or contains direct secret material")]
    InvalidDocument,
    #[error("document port returned inconsistent identity or content")]
    InvalidPortResponse,
    #[error("project record is invalid")]
    InvalidProject,
    #[error("project is already registered")]
    DuplicateProject,
    #[error("project state lock is unavailable")]
    Poisoned,
    #[error("document port failed: {code}: {message}")]
    Port {
        code: String,
        message: String,
        retryable: bool,
    },
}

impl From<ProjectPortErrorV1> for ProjectError {
    fn from(error: ProjectPortErrorV1) -> Self {
        Self::Port {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        }
    }
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_revalidation_rejects_deleted_root() {
        // Test-harness thread names contain ':' which is not a valid Windows
        // filename component; sanitize the suffix for the temporary path.
        let thread_suffix: String = std::thread::current()
            .name()
            .unwrap_or("test")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        let root = std::env::temp_dir().join(format!(
            "aworkit-core-workspace-{}-{}",
            std::process::id(),
            thread_suffix
        ));
        fs::create_dir_all(&root).expect("root");
        let coordinator = ProjectCoordinator::open(root.join("state")).expect("coordinator");
        let binding = coordinator.resolve_workspace_v1(&root).expect("binding");
        fs::remove_dir_all(&root).expect("cleanup");
        assert!(matches!(
            coordinator.revalidate_workspace_v1(&binding),
            Err(ProjectError::WorkspaceUnavailable)
        ));
    }

    #[test]
    fn direct_secret_shapes_are_rejected_but_credential_refs_are_allowed() {
        assert!(
            validate_document(&ProjectDocumentV1 {
                schema_version: 1,
                body: serde_json::json!({"password": "cleartext"}),
            })
            .is_err()
        );
        assert!(
            validate_document(&ProjectDocumentV1 {
                schema_version: 1,
                body: serde_json::json!({"credentialRef": "credential.one"}),
            })
            .is_ok()
        );
    }
}
