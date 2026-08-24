//! Startup compatibility, integrity, migration, backup, restore, and maintenance gates.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, MAIN_DB, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    StoreError,
    database::{HISTORY_SCHEMA_VERSION, open_history_database, schema_version, sqlite_integrity},
    document::JsonDocument,
    filesystem::write_and_sync_atomic,
    maintenance::MaintenanceGate,
    manifest::{MANIFEST_SCHEMA_VERSION, Manifest},
};

static MAINTENANCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const STORAGE_FORMAT_VERSION: u32 = 2;
const STORAGE_SCHEMA_FILE: &str = "storage-schema.json";
const BACKUP_MANIFEST_FILE: &str = "backup-manifest.json";
const MIGRATION_JOURNAL_FILE: &str = "migration-journal.json";
const LAST_MIGRATION_FILE: &str = "migration-last.json";

/// The safe startup mode selected before any canonical writer is released.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageMode {
    ReadWrite,
    MigrationRequired { from: u32, to: u32 },
    InspectableReadOnly { reason: String },
}

/// Explicit startup validation facts for recovery coordination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityReport {
    pub format_version: u32,
    pub mode: StorageMode,
    pub checked: Vec<String>,
}

/// Successful restore facts. The prior root is deliberately retained so the
/// replacement remains recoverable until a later explicit retention cleanup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreReceipt {
    pub restored_from: PathBuf,
    pub previous_store_quarantine: PathBuf,
}

/// Successful migration facts, including the mandatory pre-migration backup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationReceipt {
    pub from_version: u32,
    pub to_version: u32,
    pub backup_path: PathBuf,
}

/// Coordinates exclusive maintenance ownership across every local-store writer.
#[derive(Clone, Debug)]
pub struct StorageCoordinator {
    root: PathBuf,
    gate: MaintenanceGate,
}

impl StorageCoordinator {
    /// Opens the fixed local-store root. A genuinely empty root is initialized
    /// as the current format; non-empty legacy roots remain migration-required.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = absolute_clean(&root.into())?;
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        let gate = MaintenanceGate::for_root(&root)?;
        let coordinator = Self { root, gate };
        if !coordinator.root.join(STORAGE_SCHEMA_FILE).exists()
            && !has_substantive_content(&coordinator.root)?
        {
            let _lease = coordinator.gate.exclusive()?;
            write_storage_schema(&coordinator.root, STORAGE_FORMAT_VERSION)?;
        }
        Ok(coordinator)
    }

    /// Returns the active root for diagnostics and explicit lifecycle wiring.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validates format compatibility and all canonical evidence before writers
    /// are released. Corruption becomes inspectable read-only, not optimistic use.
    pub fn check_startup(&self) -> Result<IntegrityReport, StoreError> {
        let _lease = self.gate.exclusive()?;
        let mut checked = Vec::new();
        if self.root.join(MIGRATION_JOURNAL_FILE).exists() {
            return Ok(IntegrityReport {
                format_version: storage_version(&self.root).ok().flatten().unwrap_or(0),
                mode: StorageMode::InspectableReadOnly {
                    reason: "an interrupted storage migration requires recovery from its backup"
                        .into(),
                },
                checked: vec![MIGRATION_JOURNAL_FILE.into()],
            });
        }
        let found = match storage_version(&self.root) {
            Ok(found) => found,
            Err(error) => {
                return Ok(IntegrityReport {
                    format_version: 0,
                    mode: StorageMode::InspectableReadOnly {
                        reason: error.to_string(),
                    },
                    checked,
                });
            }
        };
        let Some(found) = found else {
            return Ok(IntegrityReport {
                format_version: 0,
                mode: StorageMode::MigrationRequired {
                    from: 0,
                    to: STORAGE_FORMAT_VERSION,
                },
                checked,
            });
        };
        checked.push(format!("{STORAGE_SCHEMA_FILE}: v{found}"));
        if found > STORAGE_FORMAT_VERSION {
            return Ok(IntegrityReport {
                format_version: found,
                mode: StorageMode::InspectableReadOnly {
                    reason: format!(
                        "storage format v{found} is newer than supported v{STORAGE_FORMAT_VERSION}"
                    ),
                },
                checked,
            });
        }
        if found < STORAGE_FORMAT_VERSION {
            return Ok(IntegrityReport {
                format_version: found,
                mode: StorageMode::MigrationRequired {
                    from: found,
                    to: STORAGE_FORMAT_VERSION,
                },
                checked,
            });
        }
        match inspect_store(&self.root, &mut checked) {
            Ok(()) => Ok(IntegrityReport {
                format_version: found,
                mode: StorageMode::ReadWrite,
                checked,
            }),
            Err(error) => Ok(IntegrityReport {
                format_version: found,
                mode: StorageMode::InspectableReadOnly {
                    reason: error.to_string(),
                },
                checked,
            }),
        }
    }

    /// Migrates a supported legacy root only after a validated crash-consistent
    /// backup. A durable journal makes interrupted upgrades fail closed.
    pub fn migrate(&self, backup_root: impl AsRef<Path>) -> Result<MigrationReceipt, StoreError> {
        let _lease = self.gate.exclusive()?;
        if self.root.join(MIGRATION_JOURNAL_FILE).exists() {
            return Err(StoreError::MigrationRecoveryRequired);
        }
        let from = storage_version(&self.root)?.unwrap_or(0);
        if from > STORAGE_FORMAT_VERSION {
            return Err(StoreError::UnsupportedStorageVersion {
                found: from,
                supported: STORAGE_FORMAT_VERSION,
            });
        }
        if from == STORAGE_FORMAT_VERSION {
            return Err(StoreError::MigrationNotRequired);
        }
        let backup_path = self.create_backup_locked(backup_root.as_ref(), from)?;
        let journal = MigrationJournal {
            from_version: from,
            to_version: STORAGE_FORMAT_VERSION,
            backup_path: backup_path.to_string_lossy().into_owned(),
            phase: "applying".into(),
        };
        write_and_sync_atomic(
            &self.root.join(MIGRATION_JOURNAL_FILE),
            &serde_json::to_vec(&journal)?,
        )?;

        let history = self.root.join("history.sqlite");
        if history.exists() {
            let connection = open_history_database(&history)?;
            drop(connection);
        }
        migrate_legacy_artifact_database(&self.root)?;
        migrate_manifests(&self.root)?;
        write_storage_schema(&self.root, STORAGE_FORMAT_VERSION)?;
        let mut checked = Vec::new();
        inspect_store(&self.root, &mut checked)?;

        let completed = MigrationJournal {
            phase: "complete".into(),
            ..journal
        };
        write_and_sync_atomic(
            &self.root.join(LAST_MIGRATION_FILE),
            &serde_json::to_vec(&completed)?,
        )?;
        fs::remove_file(self.root.join(MIGRATION_JOURNAL_FILE))?;
        sync_directory(&self.root)?;
        Ok(MigrationReceipt {
            from_version: from,
            to_version: STORAGE_FORMAT_VERSION,
            backup_path,
        })
    }

    /// Creates a staged, hashed backup. SQLite files use the online backup API,
    /// never a live WAL file copy.
    pub fn create_backup(&self, backup_root: impl AsRef<Path>) -> Result<PathBuf, StoreError> {
        let _lease = self.gate.exclusive()?;
        let version = storage_version(&self.root)?.unwrap_or(0);
        self.create_backup_locked(backup_root.as_ref(), version)
    }

    fn create_backup_locked(
        &self,
        backup_root: &Path,
        format_version: u32,
    ) -> Result<PathBuf, StoreError> {
        let backup_root = absolute_clean(backup_root)?;
        fs::create_dir_all(&backup_root)?;
        let backup_root = fs::canonicalize(backup_root)?;
        if backup_root.starts_with(&self.root) {
            return Err(StoreError::BackupLocationInsideStore);
        }
        let sequence = MAINTENANCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!("backup-v{format_version}-{}-{sequence}", now_epoch_ms()?);
        let staging = backup_root.join(format!(".{name}.staging"));
        let final_path = backup_root.join(&name);
        if staging.exists() || final_path.exists() {
            return Err(StoreError::InvalidBackup(
                "generated backup path already exists".into(),
            ));
        }
        fs::create_dir(&staging)?;
        let snapshot_result = snapshot_tree(&self.root, &self.root, &staging);
        let mut entries = match snapshot_result {
            Ok(entries) => entries,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        let manifest = BackupManifest {
            schema_version: 1,
            storage_format_version: format_version,
            created_at_epoch_ms: now_epoch_ms()?,
            complete: true,
            files: entries,
        };
        write_and_sync_atomic(
            &staging.join(BACKUP_MANIFEST_FILE),
            &serde_json::to_vec(&manifest)?,
        )?;
        sync_tree(&staging)?;
        validate_backup(&staging)?;
        fs::rename(&staging, &final_path)?;
        sync_directory(&backup_root)?;
        Ok(final_path)
    }

    /// Validates and restores one complete backup through sibling staging. The
    /// prior store is renamed, not deleted, before the new root is promoted.
    pub fn restore_backup(
        &self,
        backup_path: impl AsRef<Path>,
    ) -> Result<RestoreReceipt, StoreError> {
        let _lease = self.gate.exclusive()?;
        let backup_path = fs::canonicalize(backup_path.as_ref())?;
        if backup_path.starts_with(&self.root) || self.root.starts_with(&backup_path) {
            return Err(StoreError::RestoreLocationOverlapsStore);
        }
        validate_backup(&backup_path)?;
        let parent = self.root.parent().ok_or(StoreError::InvalidStorePath)?;
        let root_name = self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(StoreError::InvalidStorePath)?;
        let sequence = MAINTENANCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staging = parent.join(format!(".{root_name}.restore-staging-{sequence}"));
        let quarantine = parent.join(format!(
            ".{root_name}.pre-restore-{}-{sequence}",
            now_epoch_ms()?
        ));
        if staging.exists() || quarantine.exists() {
            return Err(StoreError::InvalidBackup(
                "restore staging path already exists".into(),
            ));
        }
        fs::create_dir(&staging)?;
        if let Err(error) = copy_validated_backup(&backup_path, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        let mut checked = Vec::new();
        inspect_store(&staging, &mut checked)?;
        sync_tree(&staging)?;

        fs::rename(&self.root, &quarantine)?;
        if let Err(error) = fs::rename(&staging, &self.root) {
            let rollback = fs::rename(&quarantine, &self.root);
            if rollback.is_err() {
                return Err(StoreError::RestorePromotionFailed(format!(
                    "promotion failed ({error}); rollback also failed"
                )));
            }
            return Err(StoreError::RestorePromotionFailed(error.to_string()));
        }
        sync_directory(parent)?;
        Ok(RestoreReceipt {
            restored_from: backup_path,
            previous_store_quarantine: quarantine,
        })
    }

    /// Validates a backup without mutating the active store.
    pub fn validate_backup(&self, backup_path: impl AsRef<Path>) -> Result<(), StoreError> {
        let _lease = self.gate.shared()?;
        validate_backup(&fs::canonicalize(backup_path.as_ref())?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StorageSchema {
    schema_version: u32,
    minimum_reader_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MigrationJournal {
    from_version: u32,
    to_version: u32,
    backup_path: String,
    phase: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupManifest {
    schema_version: u32,
    storage_format_version: u32,
    created_at_epoch_ms: u64,
    complete: bool,
    files: Vec<BackupFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupFile {
    relative_path: String,
    byte_size: u64,
    sha256: String,
    sqlite_snapshot: bool,
}

fn write_storage_schema(root: &Path, version: u32) -> Result<(), StoreError> {
    let schema = StorageSchema {
        schema_version: version,
        minimum_reader_version: 1,
    };
    write_and_sync_atomic(
        &root.join(STORAGE_SCHEMA_FILE),
        &serde_json::to_vec(&schema)?,
    )?;
    Ok(())
}

fn storage_version(root: &Path) -> Result<Option<u32>, StoreError> {
    let path = root.join(STORAGE_SCHEMA_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let schema: StorageSchema = serde_json::from_slice(&fs::read(path)?)?;
    if schema.schema_version == 0
        || schema.minimum_reader_version == 0
        || schema.minimum_reader_version > schema.schema_version
    {
        return Err(StoreError::InvalidBackup(
            "storage schema version bounds are invalid".into(),
        ));
    }
    Ok(Some(schema.schema_version))
}

fn inspect_store(root: &Path, checked: &mut Vec<String>) -> Result<(), StoreError> {
    reject_symlinks(root, root)?;
    for path in collect_files(root)? {
        if path
            .extension()
            .is_some_and(|extension| extension == "sqlite")
        {
            let disposable_projection = path
                .file_name()
                .is_some_and(|name| name == "projection.sqlite");
            let connection = match Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) {
                Ok(connection) => connection,
                Err(_) if disposable_projection => {
                    checked.push("projection.sqlite: disposable rebuild required".into());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let integrity = match sqlite_integrity(&connection) {
                Ok(integrity) => integrity,
                Err(_) if disposable_projection => {
                    checked.push("projection.sqlite: disposable rebuild required".into());
                    continue;
                }
                Err(error) => return Err(error),
            };
            if integrity != ["ok"] {
                if disposable_projection {
                    checked.push("projection.sqlite: disposable rebuild required".into());
                    continue;
                }
                return Err(StoreError::InvalidBackup(format!(
                    "SQLite integrity failed for {}: {}",
                    relative(root, &path)?,
                    integrity.join("; ")
                )));
            }
            let version = match schema_version(&connection) {
                Ok(version) => version,
                Err(_) if disposable_projection => {
                    checked.push("projection.sqlite: disposable rebuild required".into());
                    continue;
                }
                Err(error) => return Err(error),
            };
            if path
                .file_name()
                .is_some_and(|name| name == "history.sqlite")
                && version > HISTORY_SCHEMA_VERSION
            {
                return Err(StoreError::UnsupportedStorageVersion {
                    found: u32::try_from(version).unwrap_or(u32::MAX),
                    supported: u32::try_from(HISTORY_SCHEMA_VERSION).expect("positive version"),
                });
            }
            checked.push(format!(
                "{}: SQLite integrity/user_version={version}",
                relative(root, &path)?
            ));
        }
    }
    inspect_manifests(root, checked)?;
    inspect_artifact_objects(root, checked)?;
    Ok(())
}

fn inspect_manifests(root: &Path, checked: &mut Vec<String>) -> Result<(), StoreError> {
    for (directory, expected_kind) in [
        ("configuration", crate::DocumentKind::Configuration),
        ("workflows", crate::DocumentKind::Workflow),
    ] {
        let collection = root.join(directory);
        let manifest_path = collection.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(StoreError::InvalidBackup(format!(
                "{directory} manifest schema {} requires migration",
                manifest.schema_version
            )));
        }
        for (document_id, entry) in &manifest.documents {
            if entry.kind != expected_kind {
                return Err(StoreError::InvalidBackup(format!(
                    "manifest kind mismatch for {document_id}"
                )));
            }
            let expected_path = format!("bodies/{}.json", entry.content_hash);
            if entry.relative_path != expected_path || !safe_relative(&entry.relative_path) {
                return Err(StoreError::InvalidBackup(format!(
                    "unsafe body path for {document_id}"
                )));
            }
            let body_path = collection.join(&entry.relative_path);
            let bytes = fs::read(&body_path)?;
            if sha256_file_bytes(&bytes) != entry.content_hash {
                return Err(StoreError::InvalidBackup(format!(
                    "document body hash mismatch for {document_id}"
                )));
            }
            let document = JsonDocument::parse(bytes).map_err(|error| {
                StoreError::InvalidBackup(format!("invalid body for {document_id}: {error}"))
            })?;
            if document.schema_version() != entry.schema_version {
                return Err(StoreError::InvalidBackup(format!(
                    "document schema mismatch for {document_id}"
                )));
            }
        }
        checked.push(format!("{directory}: manifest/body hashes"));
    }
    Ok(())
}

fn inspect_artifact_objects(root: &Path, checked: &mut Vec<String>) -> Result<(), StoreError> {
    let history = root.join("history.sqlite");
    if !history.exists() {
        return Ok(());
    }
    let connection = Connection::open_with_flags(
        history,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let has_artifacts: i64 = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='artifacts')",
        [],
        |row| row.get(0),
    )?;
    if has_artifacts == 0 {
        return Ok(());
    }
    let mut statement = connection.prepare(
        "SELECT content_hash, byte_size FROM artifacts
         UNION SELECT content_hash, byte_size FROM prepared_artifacts",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (hash, size) in &objects {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StoreError::CorruptArtifact);
        }
        let path = root.join("objects").join(&hash[..2]).join(hash);
        let metadata = fs::metadata(&path).map_err(|_| StoreError::CorruptArtifact)?;
        if metadata.len() != u64::try_from(*size).map_err(|_| StoreError::CorruptArtifact)?
            || sha256_file(&path)? != *hash
        {
            return Err(StoreError::CorruptArtifact);
        }
    }
    if !objects.is_empty() {
        checked.push(format!("objects: {} immutable hashes", objects.len()));
    }
    Ok(())
}

fn migrate_manifests(root: &Path) -> Result<(), StoreError> {
    for directory in ["configuration", "workflows"] {
        let path = root.join(directory).join("manifest.json");
        if !path.exists() {
            continue;
        }
        let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path)?)?;
        if manifest.schema_version == 0 || manifest.schema_version > MANIFEST_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedStorageVersion {
                found: manifest.schema_version,
                supported: MANIFEST_SCHEMA_VERSION,
            });
        }
        for entry in manifest.documents.values_mut() {
            if entry.committed_generation == 0 {
                entry.committed_generation = entry.document_version;
            }
            entry.inspectable_read_only = entry.schema_version.0 > 1;
        }
        manifest.schema_version = MANIFEST_SCHEMA_VERSION;
        write_and_sync_atomic(&path, &serde_json::to_vec(&manifest)?)?;
    }
    Ok(())
}

fn migrate_legacy_artifact_database(root: &Path) -> Result<(), StoreError> {
    let legacy_path = root.join("artifacts.sqlite");
    if !legacy_path.exists() {
        return Ok(());
    }
    let legacy = Connection::open_with_flags(
        &legacy_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let prepared = {
        let mut statement = legacy.prepare(
            "SELECT token_id, artifact_id, content_hash, byte_size, media_type,
                    logical_name, finalized_event_id
             FROM prepared_artifacts",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let artifacts = {
        let mut statement = legacy.prepare(
            "SELECT artifact_id, content_hash, byte_size, media_type, logical_name,
                    origin_event_id FROM artifacts",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    drop(legacy);

    let mut destination = open_history_database(&root.join("history.sqlite"))?;
    let transaction = destination.transaction()?;
    for (token_id, artifact_id, hash, size, media_type, name, finalized) in prepared {
        transaction.execute(
            "INSERT OR IGNORE INTO prepared_artifacts(
               token_id, artifact_id, content_hash, byte_size, media_type, logical_name,
               staging_generation, prepared_at_epoch_ms, finalized_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0, ?7)",
            rusqlite::params![
                token_id,
                artifact_id,
                hash,
                size,
                media_type,
                name,
                finalized
            ],
        )?;
    }
    for (artifact_id, hash, size, media_type, name, origin_event) in artifacts {
        transaction.execute(
            "INSERT OR IGNORE INTO artifacts(
               artifact_id, content_hash, byte_size, media_type, logical_name,
               created_generation, created_at_epoch_ms, retention_class, availability
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, 'chat', 'available')",
            rusqlite::params![artifact_id, hash, size, media_type, name],
        )?;
        if let Some(origin_event) = origin_event {
            transaction.execute(
                "INSERT OR IGNORE INTO artifact_references(artifact_id, origin_event_id)
                 VALUES (?1, ?2)",
                rusqlite::params![artifact_id, origin_event],
            )?;
        }
    }
    transaction.execute(
        "INSERT INTO store_state(key, value) VALUES ('legacy_artifacts_migrated', 'true')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [],
    )?;
    transaction.commit()?;
    Ok(())
}

fn snapshot_tree(
    source_root: &Path,
    source: &Path,
    destination: &Path,
) -> Result<Vec<BackupFile>, StoreError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let name = entry.file_name();
        if skip_snapshot_entry(&name, file_type.is_dir()) {
            continue;
        }
        if file_type.is_symlink() {
            return Err(StoreError::InvalidBackup(format!(
                "symlink is not allowed in store: {}",
                source_path.display()
            )));
        }
        let relative_path = source_path
            .strip_prefix(source_root)
            .map_err(|_| StoreError::InvalidStorePath)?;
        let destination_path = destination.join(relative_path);
        if file_type.is_dir() {
            fs::create_dir_all(&destination_path)?;
            entries.extend(snapshot_tree(source_root, &source_path, destination)?);
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let sqlite_snapshot = source_path
                .extension()
                .is_some_and(|extension| extension == "sqlite");
            if sqlite_snapshot {
                let connection = Connection::open_with_flags(
                    &source_path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                connection.backup(MAIN_DB, &destination_path, None)?;
                for suffix in ["-wal", "-shm"] {
                    let companion =
                        PathBuf::from(format!("{}{suffix}", destination_path.display()));
                    if companion.exists() {
                        fs::remove_file(companion)?;
                    }
                }
            } else {
                fs::copy(&source_path, &destination_path)?;
            }
            sync_file(&destination_path)?;
            entries.push(BackupFile {
                relative_path: portable_relative(relative_path)?,
                byte_size: fs::metadata(&destination_path)?.len(),
                sha256: sha256_file(&destination_path)?,
                sqlite_snapshot,
            });
        }
    }
    Ok(entries)
}

fn skip_snapshot_entry(name: &std::ffi::OsStr, is_directory: bool) -> bool {
    let name = name.to_string_lossy();
    name == ".repository.lock"
        || name == MIGRATION_JOURNAL_FILE
        || name == "projection.sqlite"
        || name.starts_with("projection.sqlite.corrupt-")
        || name.ends_with("-wal")
        || name.ends_with("-shm")
        || (is_directory && name == ".artifact-staging")
}

fn validate_backup(root: &Path) -> Result<(), StoreError> {
    reject_symlinks(root, root)?;
    let manifest_path = root.join(BACKUP_MANIFEST_FILE);
    let manifest: BackupManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    if manifest.schema_version != 1 || !manifest.complete {
        return Err(StoreError::InvalidBackup(
            "backup manifest is incomplete or unsupported".into(),
        ));
    }
    if manifest.storage_format_version > STORAGE_FORMAT_VERSION {
        return Err(StoreError::UnsupportedStorageVersion {
            found: manifest.storage_format_version,
            supported: STORAGE_FORMAT_VERSION,
        });
    }
    let mut expected = BTreeSet::new();
    for entry in &manifest.files {
        if !safe_relative(&entry.relative_path) || !expected.insert(entry.relative_path.clone()) {
            return Err(StoreError::InvalidBackup(
                "backup manifest contains an unsafe or duplicate path".into(),
            ));
        }
        let path = root.join(&entry.relative_path);
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file()
            || metadata.len() != entry.byte_size
            || sha256_file(&path)? != entry.sha256
        {
            return Err(StoreError::InvalidBackup(format!(
                "backup file hash mismatch: {}",
                entry.relative_path
            )));
        }
        if entry.sqlite_snapshot {
            let connection = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            if sqlite_integrity(&connection)? != ["ok"] {
                return Err(StoreError::InvalidBackup(format!(
                    "backup SQLite integrity failed: {}",
                    entry.relative_path
                )));
            }
        }
    }
    let actual: BTreeSet<String> = collect_files(root)?
        .into_iter()
        .filter_map(|path| {
            let relative = portable_relative(path.strip_prefix(root).ok()?).ok()?;
            (relative != BACKUP_MANIFEST_FILE
                && !relative.ends_with("-wal")
                && !relative.ends_with("-shm"))
            .then_some(relative)
        })
        .collect();
    if actual != expected {
        return Err(StoreError::InvalidBackup(
            "backup contains unlisted or missing files".into(),
        ));
    }
    Ok(())
}

fn copy_validated_backup(source: &Path, destination: &Path) -> Result<(), StoreError> {
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(source.join(BACKUP_MANIFEST_FILE))?)?;
    for entry in manifest.files {
        let source_path = source.join(&entry.relative_path);
        let destination_path = destination.join(&entry.relative_path);
        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source_path, &destination_path)?;
        sync_file(&destination_path)?;
    }
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, StoreError> {
    fn visit(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), StoreError> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(StoreError::InvalidBackup(format!(
                    "symlink is not allowed: {}",
                    entry.path().display()
                )));
            }
            if file_type.is_dir() {
                visit(&entry.path(), files)?;
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, &mut files)?;
    Ok(files)
}

fn reject_symlinks(root: &Path, path: &Path) -> Result<(), StoreError> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(StoreError::InvalidBackup(format!(
            "symlink is not allowed below {}",
            root.display()
        )));
    }
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            reject_symlinks(root, &entry?.path())?;
        }
    }
    Ok(())
}

fn has_substantive_content(root: &Path) -> Result<bool, StoreError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name();
            if name != ".repository.lock" {
                return Ok(true);
            }
        } else if entry.file_type()?.is_dir() && has_substantive_content(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sha256_file(path: &Path) -> Result<String, StoreError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256_file_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sync_tree(root: &Path) -> Result<(), StoreError> {
    for path in collect_files(root)? {
        sync_file(&path)?;
    }
    let directories = collect_directories(root)?;
    for directory in directories.into_iter().rev() {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn collect_directories(root: &Path) -> Result<Vec<PathBuf>, StoreError> {
    fn visit(path: &Path, directories: &mut Vec<PathBuf>) -> Result<(), StoreError> {
        directories.push(path.to_path_buf());
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), directories)?;
            }
        }
        Ok(())
    }
    let mut directories = Vec::new();
    visit(root, &mut directories)?;
    Ok(directories)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Flushes a freshly written backup/staging file to durable storage.
///
/// A read-only handle cannot flush file buffers on Windows (`FlushFileBuffers`
/// requires write access), so the file is opened for write before syncing. This
/// is equivalent to `File::open(path)?.sync_all()` on Unix.
fn sync_file(path: &Path) -> Result<(), StoreError> {
    OpenOptions::new().write(true).open(path)?.sync_all()?;
    Ok(())
}

fn relative(root: &Path, path: &Path) -> Result<String, StoreError> {
    portable_relative(
        path.strip_prefix(root)
            .map_err(|_| StoreError::InvalidStorePath)?,
    )
}

fn portable_relative(path: &Path) -> Result<String, StoreError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| StoreError::InvalidBackup("non-Unicode path".into()))?,
            ),
            _ => return Err(StoreError::InvalidBackup("unsafe relative path".into())),
        }
    }
    Ok(parts.join("/"))
}

fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\0')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn absolute_clean(path: &Path) -> Result<PathBuf, StoreError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => clean.push(prefix.as_os_str()),
            Component::RootDir => clean.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    return Err(StoreError::InvalidStorePath);
                }
            }
            Component::Normal(part) => clean.push(part),
        }
    }
    Ok(clean)
}

fn now_epoch_ms() -> Result<u64, StoreError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StoreError::InvalidStoredData)?
            .as_millis(),
    )
    .map_err(|_| StoreError::InvalidStoredData)
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use serde_json::json;

    use super::*;
    use crate::{
        CommitBatch, DocumentRepository, Event, LocalHistoryStore, ProjectionStore, RepositoryRoot,
    };

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aworkit-{label}-{nonce}"))
    }

    #[test]
    fn startup_initializes_new_root_and_fails_closed_on_newer_or_corrupt_data() {
        let root = temp_root("storage");
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        assert_eq!(
            coordinator.check_startup().expect("check").mode,
            StorageMode::ReadWrite
        );

        write_storage_schema(&root, STORAGE_FORMAT_VERSION + 1).expect("newer");
        assert!(matches!(
            coordinator.check_startup().expect("newer check").mode,
            StorageMode::InspectableReadOnly { .. }
        ));
        write_storage_schema(&root, STORAGE_FORMAT_VERSION).expect("current");
        fs::write(root.join("broken.sqlite"), b"not sqlite").expect("corrupt");
        assert!(matches!(
            coordinator.check_startup().expect("corrupt check").mode,
            StorageMode::InspectableReadOnly { .. }
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn corrupt_disposable_projection_does_not_quarantine_canonical_writers_or_backups() {
        let root = temp_root("disposable-projection");
        let backups = temp_root("disposable-projection-backups");
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        let projection_path = root.join("projection.sqlite");
        fs::write(&projection_path, b"not sqlite").expect("corrupt projection");

        let report = coordinator.check_startup().expect("startup");
        assert_eq!(report.mode, StorageMode::ReadWrite);
        assert!(
            report
                .checked
                .iter()
                .any(|fact| fact.contains("disposable rebuild required"))
        );
        let backup = coordinator.create_backup(&backups).expect("backup");
        assert!(!backup.join("projection.sqlite").exists());

        let projection = ProjectionStore::open(&projection_path).expect("replace projection");
        assert!(projection.health().expect("projection health").healthy);
        drop(projection);
        fs::remove_dir_all(root).expect("cleanup");
        fs::remove_dir_all(backups).expect("backup cleanup");
    }

    #[test]
    fn sqlite_backup_captures_wal_state_and_tampering_is_rejected() {
        let root = temp_root("backup-source");
        let backups = temp_root("backups");
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        history
            .commit(&CommitBatch {
                chat_id: "chat_01".into(),
                branch_id: "main".into(),
                expected_head: 0,
                events: vec![Event {
                    event_id: "event_01".into(),
                    kind: "input.accepted".into(),
                    payload: json!({"schemaVersion": 1}),
                }],
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: vec![],
            })
            .expect("commit");
        let backup = coordinator.create_backup(&backups).expect("backup");
        coordinator.validate_backup(&backup).expect("valid");
        let copied = Connection::open(backup.join("history.sqlite")).expect("copied sqlite");
        let count: i64 = copied
            .query_row("SELECT COUNT(*) FROM semantic_events", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 1);
        fs::write(backup.join(STORAGE_SCHEMA_FILE), b"tampered").expect("tamper");
        assert!(matches!(
            coordinator.validate_backup(&backup),
            Err(StoreError::InvalidBackup(_))
        ));
        drop(copied);
        drop(history);
        fs::remove_dir_all(root).expect("cleanup");
        fs::remove_dir_all(backups).expect("backup cleanup");
    }

    #[test]
    fn restore_is_validated_staged_and_preserves_the_previous_store() {
        let root = temp_root("restore-source");
        let backups = temp_root("restore-backups");
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        fs::write(root.join("evidence.txt"), b"before").expect("source");
        let backup = coordinator.create_backup(&backups).expect("backup");
        fs::write(root.join("evidence.txt"), b"after").expect("mutate");
        let receipt = coordinator.restore_backup(&backup).expect("restore");
        assert_eq!(
            fs::read(root.join("evidence.txt")).expect("restored"),
            b"before"
        );
        assert_eq!(
            fs::read(receipt.previous_store_quarantine.join("evidence.txt")).expect("old"),
            b"after"
        );
        fs::remove_dir_all(&root).expect("cleanup");
        fs::remove_dir_all(receipt.previous_store_quarantine).expect("quarantine cleanup");
        fs::remove_dir_all(backups).expect("backup cleanup");
    }

    #[test]
    fn migration_requires_backup_and_upgrades_legacy_manifest() {
        let root = temp_root("migration");
        let backups = temp_root("migration-backups");
        fs::create_dir_all(root.join("configuration/bodies")).expect("dirs");
        let body =
            serde_json::to_vec(&json!({"schemaVersion": 1, "name": "legacy"})).expect("body");
        let hash = sha256_file_bytes(&body);
        fs::write(
            root.join(format!("configuration/bodies/{hash}.json")),
            &body,
        )
        .expect("body write");
        fs::write(
            root.join("configuration/manifest.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "documents": {"config_01": {
                    "kind": "configuration", "document_version": 1,
                    "schema_version": 1, "content_hash": hash,
                    "relative_path": format!("bodies/{hash}.json")
                }}
            }))
            .expect("manifest"),
        )
        .expect("manifest write");
        write_storage_schema(&root, 1).expect("legacy schema");
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        assert!(matches!(
            coordinator.check_startup().expect("check").mode,
            StorageMode::MigrationRequired { from: 1, to: 2 }
        ));
        let receipt = coordinator.migrate(&backups).expect("migrate");
        assert!(receipt.backup_path.exists());
        assert_eq!(
            coordinator.check_startup().expect("post check").mode,
            StorageMode::ReadWrite
        );
        fs::remove_dir_all(root).expect("cleanup");
        fs::remove_dir_all(backups).expect("backup cleanup");
    }

    #[test]
    fn migration_preserves_legacy_artifact_metadata_and_references() {
        let root = temp_root("artifact-migration");
        let backups = temp_root("artifact-migration-backups");
        fs::create_dir_all(&root).expect("root");
        let bytes = b"legacy\n";
        let content_hash = sha256_file_bytes(bytes);
        fs::create_dir_all(root.join("objects").join(&content_hash[..2])).expect("objects");
        fs::write(
            root.join("objects")
                .join(&content_hash[..2])
                .join(&content_hash),
            bytes,
        )
        .expect("object");
        let legacy = Connection::open(root.join("artifacts.sqlite")).expect("legacy database");
        legacy
            .execute_batch(
                "CREATE TABLE prepared_artifacts (
                   token_id TEXT PRIMARY KEY, artifact_id TEXT NOT NULL,
                   content_hash TEXT NOT NULL, byte_size INTEGER NOT NULL,
                   media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
                   finalized_event_id TEXT UNIQUE
                 ) STRICT;
                 CREATE TABLE artifacts (
                   artifact_id TEXT PRIMARY KEY, content_hash TEXT NOT NULL,
                   byte_size INTEGER NOT NULL, media_type TEXT NOT NULL,
                   logical_name TEXT NOT NULL, origin_event_id TEXT UNIQUE
                 ) STRICT;",
            )
            .expect("legacy schema");
        legacy
            .execute(
                "INSERT INTO prepared_artifacts VALUES (
                   'prepared_legacy', 'artifact_legacy', ?1, 7,
                   'text/plain', 'legacy.txt', 'event_legacy'
                 )",
                [&content_hash],
            )
            .expect("prepared row");
        legacy
            .execute(
                "INSERT INTO artifacts VALUES (
                   'artifact_legacy', ?1, 7, 'text/plain',
                   'legacy.txt', 'event_legacy'
                 )",
                [&content_hash],
            )
            .expect("artifact row");
        drop(legacy);
        write_storage_schema(&root, 1).expect("legacy storage schema");

        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        coordinator.migrate(&backups).expect("migrate");

        let history = open_history_database(&root.join("history.sqlite")).expect("history");
        let prepared: (String, i64, Option<String>) = history
            .query_row(
                "SELECT artifact_id, staging_generation, finalized_event_id
                 FROM prepared_artifacts WHERE token_id = 'prepared_legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("prepared row");
        assert_eq!(
            prepared,
            ("artifact_legacy".into(), 1, Some("event_legacy".into()))
        );
        let references: i64 = history
            .query_row(
                "SELECT COUNT(*) FROM artifact_references
                 WHERE artifact_id = 'artifact_legacy' AND origin_event_id = 'event_legacy'",
                [],
                |row| row.get(0),
            )
            .expect("reference count");
        assert_eq!(references, 1);
        let migrated: String = history
            .query_row(
                "SELECT value FROM store_state WHERE key = 'legacy_artifacts_migrated'",
                [],
                |row| row.get(0),
            )
            .expect("migration marker");
        assert_eq!(migrated, "true");

        drop(history);
        fs::remove_dir_all(root).expect("cleanup");
        fs::remove_dir_all(backups).expect("backup cleanup");
    }

    #[test]
    fn exclusive_maintenance_gate_blocks_repository_writers() {
        let root = temp_root("writer-gate");
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        let repository = RepositoryRoot::open(&root).expect("repository");
        let lease = coordinator.gate.exclusive().expect("exclusive");
        let (sender, receiver) = mpsc::channel();
        let writer = thread::spawn(move || {
            let document = JsonDocument::parse(br#"{"schemaVersion":1,"name":"blocked"}"#.to_vec())
                .expect("document");
            let result = repository.save(
                crate::DocumentKind::Configuration,
                "config_01",
                None,
                &document,
            );
            sender.send(result.is_ok()).expect("send");
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(lease);
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("writer completion")
        );
        writer.join().expect("join");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
