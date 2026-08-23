//! Optimistic, crash-consistent repositories for configuration and workflows.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde_json::Error as JsonError;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    document::{DocumentKind, DocumentValidationError, JsonDocument, SchemaVersion},
    document_policy::{DocumentPolicyError, validate_editable_document, validate_inert_document},
    filesystem::write_and_sync_atomic,
    maintenance::MaintenanceGate,
    manifest::{MANIFEST_SCHEMA_VERSION, Manifest, ManifestEntry},
};

const MANIFEST_FILE_NAME: &str = "manifest.json";
const LOCK_FILE_NAME: &str = ".repository.lock";
const BODY_DIRECTORY_NAME: &str = "bodies";
const MAX_CONFIGURATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORKFLOW_BYTES: usize = 16 * 1024 * 1024;

/// The current accepted version and body returned from a repository read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredDocument {
    /// The monotonic compare-and-swap version maintained in the manifest.
    pub version: u64,
    /// The exact, schema-versioned JSON body.
    pub document: JsonDocument,
    /// Forward-schema inert imports remain byte-preserved but cannot be edited.
    pub access: DocumentAccessMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentAccessMode {
    Editable,
    InspectableReadOnly,
}

/// A compare-and-swap failure that never overwrites a newer document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentConflict {
    /// The caller's expected generation, or `None` for create-only.
    pub expected_version: Option<u64>,
    /// The currently stored generation, or `None` when the document is absent.
    pub actual_version: Option<u64>,
}

/// A canonical repository interface used by the trusted core over its process port.
pub trait DocumentRepository {
    /// Loads the currently indexed immutable generation, if it exists.
    fn load(
        &self,
        kind: DocumentKind,
        document_id: &str,
    ) -> Result<Option<StoredDocument>, RepositoryError>;

    /// Saves only when `expected_version` still matches the manifest generation.
    fn save(
        &self,
        kind: DocumentKind,
        document_id: &str,
        expected_version: Option<u64>,
        document: &JsonDocument,
    ) -> Result<StoredDocument, RepositoryError>;

    /// Lists lightweight document versions without parsing every canonical body.
    fn list(
        &self,
        kind: DocumentKind,
    ) -> Result<Vec<(String, u64, SchemaVersion)>, RepositoryError>;

    /// Stores a lossless, non-executable import, including forward schemas.
    fn import_inert(
        &self,
        kind: DocumentKind,
        document_id: &str,
        expected_version: Option<u64>,
        document: &JsonDocument,
    ) -> Result<StoredDocument, RepositoryError>;

    /// Returns the exact accepted bytes for lossless export.
    fn export_lossless(
        &self,
        kind: DocumentKind,
        document_id: &str,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        Ok(self
            .load(kind, document_id)?
            .map(|stored| stored.document.raw_json().to_vec()))
    }
}

/// Filesystem-backed local-store repository root.
#[derive(Clone, Debug)]
pub struct RepositoryRoot {
    root: PathBuf,
    gate: MaintenanceGate,
}

impl RepositoryRoot {
    /// Opens a repository root, creating its durable collection directories.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RepositoryError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        let gate = MaintenanceGate::for_root(&root)?;
        Ok(Self { root, gate })
    }

    /// Returns the repository root for diagnostics and explicit backup tooling.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    fn collection_root(&self, kind: DocumentKind) -> PathBuf {
        self.root.join(kind.directory_name())
    }

    fn manifest_path(&self, kind: DocumentKind) -> PathBuf {
        self.collection_root(kind).join(MANIFEST_FILE_NAME)
    }

    /// Takes the collection lock before the manifest is read for a save.
    ///
    /// Atomic manifest replacement prevents torn data, while this lock makes
    /// optimistic version checks correct when independent processes save at
    /// the same time.
    fn lock_collection(&self, kind: DocumentKind) -> Result<CollectionLock, RepositoryError> {
        let collection_root = self.collection_root(kind);
        reject_symlink(&collection_root)?;
        fs::create_dir_all(&collection_root)?;
        reject_symlink(&collection_root)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(collection_root.join(LOCK_FILE_NAME))?;
        file.lock_exclusive()?;
        Ok(CollectionLock { file })
    }

    fn load_manifest(&self, kind: DocumentKind) -> Result<Manifest, RepositoryError> {
        let manifest_path = self.manifest_path(kind);
        reject_symlink(&self.collection_root(kind))?;
        if !manifest_path.exists() {
            return Ok(Manifest::empty());
        }
        reject_symlink(&manifest_path)?;
        let raw_manifest = fs::read(manifest_path)?;
        let manifest: Manifest = serde_json::from_slice(&raw_manifest)?;
        if manifest.schema_version == 0 || manifest.schema_version > MANIFEST_SCHEMA_VERSION {
            return Err(RepositoryError::UnsupportedManifestSchema(
                manifest.schema_version,
            ));
        }
        Ok(manifest)
    }

    fn save_manifest(
        &self,
        kind: DocumentKind,
        manifest: &Manifest,
    ) -> Result<(), RepositoryError> {
        reject_symlink(&self.collection_root(kind))?;
        reject_symlink(&self.manifest_path(kind))?;
        let body = serde_json::to_vec(manifest)?;
        write_and_sync_atomic(&self.manifest_path(kind), &body)?;
        Ok(())
    }

    fn load_entry(
        &self,
        kind: DocumentKind,
        entry: &ManifestEntry,
    ) -> Result<StoredDocument, RepositoryError> {
        if entry.kind != kind {
            return Err(RepositoryError::ManifestKindMismatch);
        }
        let expected_relative_path = format!("{BODY_DIRECTORY_NAME}/{}.json", entry.content_hash);
        if entry.relative_path != expected_relative_path {
            return Err(RepositoryError::UnsafeManifestPath);
        }
        let body_path = self.collection_root(kind).join(&entry.relative_path);
        reject_symlink(&self.collection_root(kind))?;
        reject_symlink(&self.collection_root(kind).join(BODY_DIRECTORY_NAME))?;
        reject_symlink(&body_path)?;
        let maximum = maximum_document_bytes(kind);
        if usize::try_from(fs::metadata(&body_path)?.len()).map_or(true, |size| size > maximum) {
            return Err(RepositoryError::DocumentTooLarge);
        }
        let raw_body = fs::read(body_path)?;
        let actual_hash = content_hash(&raw_body);
        if actual_hash != entry.content_hash {
            return Err(RepositoryError::HashMismatch);
        }
        let document = JsonDocument::parse(raw_body)?;
        if document.schema_version() != entry.schema_version {
            return Err(RepositoryError::SchemaVersionMismatch);
        }
        Ok(StoredDocument {
            version: entry.document_version,
            document,
            access: if entry.inspectable_read_only {
                DocumentAccessMode::InspectableReadOnly
            } else {
                DocumentAccessMode::Editable
            },
        })
    }

    fn save_document(
        &self,
        kind: DocumentKind,
        document_id: &str,
        expected_version: Option<u64>,
        document: &JsonDocument,
        inert: bool,
    ) -> Result<StoredDocument, RepositoryError> {
        validate_document_id(document_id)?;
        if document.raw_json().len() > maximum_document_bytes(kind) {
            return Err(RepositoryError::DocumentTooLarge);
        }
        if inert {
            validate_inert_document(document)?;
        } else {
            validate_editable_document(kind, document)?;
        }

        let _maintenance = self.gate.shared()?;
        let _lock = self.lock_collection(kind)?;
        let mut manifest = self.load_manifest(kind)?;
        let current_version = manifest
            .documents
            .get(document_id)
            .map(|entry| entry.document_version);
        if current_version != expected_version {
            return Err(RepositoryError::Conflict(DocumentConflict {
                expected_version,
                actual_version: current_version,
            }));
        }

        let digest = content_hash(document.raw_json());
        let relative_path = format!("{BODY_DIRECTORY_NAME}/{digest}.json");
        let body_path = self.collection_root(kind).join(&relative_path);
        reject_symlink(&self.collection_root(kind).join(BODY_DIRECTORY_NAME))?;
        reject_symlink(&body_path)?;
        if body_path.exists() {
            let existing = fs::read(&body_path)?;
            if content_hash(&existing) != digest || existing != document.raw_json() {
                return Err(RepositoryError::HashMismatch);
            }
        } else {
            write_and_sync_atomic(&body_path, document.raw_json())?;
        }

        let version = current_version
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(RepositoryError::VersionExhausted)?;
        manifest.schema_version = MANIFEST_SCHEMA_VERSION;
        manifest.documents.insert(
            document_id.to_owned(),
            ManifestEntry {
                kind,
                document_version: version,
                schema_version: document.schema_version(),
                content_hash: digest,
                relative_path,
                committed_generation: version,
                inspectable_read_only: inert
                    && document.schema_version().0
                        > crate::document_policy::SUPPORTED_DOCUMENT_SCHEMA_VERSION,
            },
        );
        self.save_manifest(kind, &manifest)?;
        Ok(StoredDocument {
            version,
            document: document.clone(),
            access: if inert
                && document.schema_version().0
                    > crate::document_policy::SUPPORTED_DOCUMENT_SCHEMA_VERSION
            {
                DocumentAccessMode::InspectableReadOnly
            } else {
                DocumentAccessMode::Editable
            },
        })
    }
}

impl DocumentRepository for RepositoryRoot {
    fn load(
        &self,
        kind: DocumentKind,
        document_id: &str,
    ) -> Result<Option<StoredDocument>, RepositoryError> {
        validate_document_id(document_id)?;
        let _maintenance = self.gate.shared()?;
        let manifest = self.load_manifest(kind)?;
        manifest
            .documents
            .get(document_id)
            .map(|entry| self.load_entry(kind, entry))
            .transpose()
    }

    fn save(
        &self,
        kind: DocumentKind,
        document_id: &str,
        expected_version: Option<u64>,
        document: &JsonDocument,
    ) -> Result<StoredDocument, RepositoryError> {
        self.save_document(kind, document_id, expected_version, document, false)
    }

    fn list(
        &self,
        kind: DocumentKind,
    ) -> Result<Vec<(String, u64, SchemaVersion)>, RepositoryError> {
        let _maintenance = self.gate.shared()?;
        let manifest = self.load_manifest(kind)?;
        Ok(manifest
            .documents
            .into_iter()
            .filter_map(|(id, entry)| {
                (entry.kind == kind).then_some((id, entry.document_version, entry.schema_version))
            })
            .collect())
    }

    fn import_inert(
        &self,
        kind: DocumentKind,
        document_id: &str,
        expected_version: Option<u64>,
        document: &JsonDocument,
    ) -> Result<StoredDocument, RepositoryError> {
        self.save_document(kind, document_id, expected_version, document, true)
    }
}

const fn maximum_document_bytes(kind: DocumentKind) -> usize {
    match kind {
        DocumentKind::Configuration => MAX_CONFIGURATION_BYTES,
        DocumentKind::Workflow => MAX_WORKFLOW_BYTES,
    }
}

/// Releases the operating-system advisory lock when a save completes.
struct CollectionLock {
    file: File,
}

impl Drop for CollectionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn validate_document_id(document_id: &str) -> Result<(), RepositoryError> {
    let valid = !document_id.is_empty()
        && document_id.len() <= 128
        && document_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(RepositoryError::InvalidDocumentId)
    }
}

fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

fn reject_symlink(path: &Path) -> Result<(), RepositoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(RepositoryError::UnsafeRepositorySymlink)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RepositoryError::Io(error)),
    }
}

/// Failures that prevent a canonical repository read or save.
#[derive(Debug, Error)]
pub enum RepositoryError {
    /// Filesystem operations could not durably complete.
    #[error("repository filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    /// The manifest cannot be parsed as its supported JSON format.
    #[error("repository manifest is invalid JSON: {0}")]
    InvalidManifest(#[from] JsonError),
    /// The body is malformed or lacks a schema version.
    #[error(transparent)]
    InvalidDocument(#[from] DocumentValidationError),
    #[error(transparent)]
    DocumentPolicy(#[from] DocumentPolicyError),
    /// Document IDs are logical identifiers, never paths.
    #[error("document IDs must be 1-128 ASCII letters, digits, '.', '_' or '-')")]
    InvalidDocumentId,
    /// A compare-and-swap save observed a newer or missing document.
    #[error("document version conflict")]
    Conflict(DocumentConflict),
    /// The stored index is newer than this binary understands.
    #[error("unsupported manifest schema version {0}")]
    UnsupportedManifestSchema(u32),
    /// The body no longer matches the index's immutable content hash.
    #[error("canonical document body does not match its indexed content hash")]
    HashMismatch,
    /// The body schema disagrees with its durable manifest metadata.
    #[error("canonical document body schema version does not match its manifest entry")]
    SchemaVersionMismatch,
    #[error("manifest entry belongs to another document collection")]
    ManifestKindMismatch,
    #[error("manifest body path is not its canonical content-addressed path")]
    UnsafeManifestPath,
    #[error("repository paths may not traverse symbolic links")]
    UnsafeRepositorySymlink,
    /// Canonical document bodies are bounded to protect local resources.
    #[error("document exceeds the 16 MiB repository limit")]
    DocumentTooLarge,
    /// The per-document optimistic generation cannot advance further.
    #[error("document version counter is exhausted")]
    VersionExhausted,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn repository() -> RepositoryRoot {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        RepositoryRoot::open(std::env::temp_dir().join(format!("aworkit-local-store-{nonce}")))
            .expect("repository")
    }

    fn document(body: &str) -> JsonDocument {
        JsonDocument::parse(body.as_bytes()).expect("document")
    }

    #[test]
    fn workflow_unknown_fields_round_trip_as_exact_bytes() {
        let repository = repository();
        let raw = r#"{ "schemaVersion": 1, "workflowId": "wf_01", "futureNode": {"kind":"future","layout":[1,2]} }"#;
        let saved = repository
            .save(DocumentKind::Workflow, "wf_01", None, &document(raw))
            .expect("save");
        let loaded = repository
            .load(DocumentKind::Workflow, "wf_01")
            .expect("load")
            .expect("stored document");
        assert_eq!(saved.version, 1);
        assert_eq!(loaded.document.raw_json(), raw.as_bytes());
        fs::remove_dir_all(repository.path()).expect("cleanup");
    }

    #[test]
    fn stale_save_cannot_overwrite_newer_configuration() {
        let repository = repository();
        repository
            .save(
                DocumentKind::Configuration,
                "settings",
                None,
                &document(r#"{"schemaVersion":1,"theme":"system"}"#),
            )
            .expect("initial save");
        repository
            .save(
                DocumentKind::Configuration,
                "settings",
                Some(1),
                &document(r#"{"schemaVersion":1,"theme":"dark"}"#),
            )
            .expect("newer save");
        let error = repository
            .save(
                DocumentKind::Configuration,
                "settings",
                Some(1),
                &document(r#"{"schemaVersion":1,"theme":"light"}"#),
            )
            .expect_err("stale save must conflict");
        assert!(matches!(
            error,
            RepositoryError::Conflict(DocumentConflict {
                expected_version: Some(1),
                actual_version: Some(2)
            })
        ));
        assert_eq!(
            repository
                .load(DocumentKind::Configuration, "settings")
                .expect("load")
                .expect("document")
                .document
                .raw_json(),
            br#"{"schemaVersion":1,"theme":"dark"}"#
        );
        fs::remove_dir_all(repository.path()).expect("cleanup");
    }

    #[test]
    fn editable_bodies_require_a_positive_schema_version() {
        assert!(matches!(
            JsonDocument::parse(br#"{"future":true}"#.to_vec()),
            Err(DocumentValidationError::MissingSchemaVersion)
        ));
    }

    #[test]
    fn forward_schema_import_is_lossless_but_inspectable_read_only() {
        let repository = repository();
        let raw = br#"{ "schemaVersion": 9, "futureGraph": {"unknown": [1,2,3]} }"#;
        let future = JsonDocument::parse(raw.to_vec()).expect("future document");
        assert!(matches!(
            repository.save(DocumentKind::Workflow, "future_01", None, &future),
            Err(RepositoryError::DocumentPolicy(
                DocumentPolicyError::ForwardSchema(9)
            ))
        ));
        let imported = repository
            .import_inert(DocumentKind::Workflow, "future_01", None, &future)
            .expect("inert import");
        assert_eq!(imported.access, DocumentAccessMode::InspectableReadOnly);
        assert_eq!(
            repository
                .export_lossless(DocumentKind::Workflow, "future_01")
                .expect("export")
                .expect("bytes"),
            raw
        );
        assert!(matches!(
            repository.save(
                DocumentKind::Workflow,
                "future_01",
                Some(imported.version),
                &future
            ),
            Err(RepositoryError::DocumentPolicy(
                DocumentPolicyError::ForwardSchema(9)
            ))
        ));
        fs::remove_dir_all(repository.path()).expect("cleanup");
    }

    #[test]
    fn documents_reject_secret_values_but_allow_opaque_credential_references() {
        let repository = repository();
        let accepted =
            document(r#"{"schemaVersion":1,"credentialRef":"credential_01","tokenCount":42}"#);
        repository
            .save(DocumentKind::Configuration, "safe", None, &accepted)
            .expect("credential reference");
        let rejected = document(r#"{"schemaVersion":1,"accessToken":"plaintext"}"#);
        assert!(matches!(
            repository.save(DocumentKind::Configuration, "unsafe", None, &rejected),
            Err(RepositoryError::DocumentPolicy(
                DocumentPolicyError::SecretMaterial(_)
            ))
        ));
        fs::remove_dir_all(repository.path()).expect("cleanup");
    }

    #[test]
    fn configuration_size_bound_is_checked_before_writing() {
        let repository = repository();
        let padding = "x".repeat(MAX_CONFIGURATION_BYTES);
        let oversized = JsonDocument::parse(
            format!(r#"{{"schemaVersion":1,"padding":"{padding}"}}"#).into_bytes(),
        )
        .expect("valid oversized JSON");
        assert!(matches!(
            repository.save(DocumentKind::Configuration, "large", None, &oversized),
            Err(RepositoryError::DocumentTooLarge)
        ));
        assert!(
            repository
                .list(DocumentKind::Configuration)
                .expect("list")
                .is_empty()
        );
        fs::remove_dir_all(repository.path()).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn repository_rejects_symlinked_canonical_directories() {
        use std::os::unix::fs::symlink;

        let repository = repository();
        let outside = repository.path().with_extension("outside");
        fs::create_dir_all(&outside).expect("outside");
        fs::create_dir_all(repository.path().join("workflows")).expect("collection");
        symlink(&outside, repository.path().join("workflows/bodies")).expect("symlink");
        assert!(matches!(
            repository.save(
                DocumentKind::Workflow,
                "wf_unsafe",
                None,
                &document(r#"{"schemaVersion":1,"name":"unsafe"}"#)
            ),
            Err(RepositoryError::UnsafeRepositorySymlink)
        ));
        assert!(
            fs::read_dir(&outside)
                .expect("outside entries")
                .next()
                .is_none()
        );
        fs::remove_dir_all(repository.path()).expect("cleanup");
        fs::remove_dir_all(outside).expect("outside cleanup");
    }
}
