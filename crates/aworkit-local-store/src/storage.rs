//! Startup compatibility, integrity, backup, and writer-maintenance gates.

use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs2::FileExt;
use rusqlite::Connection;

use crate::StoreError;

static BACKUP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const STORAGE_FORMAT_VERSION: u32 = 1;

/// The safe storage mode selected by startup validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageMode {
    /// All required format and integrity checks passed.
    ReadWrite,
    /// Evidence is inspectable but canonical writes and continuation are blocked.
    InspectableReadOnly { reason: String },
}

/// Explicit startup validation facts for the trusted core recovery coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityReport {
    /// Format version understood by the running binary.
    pub format_version: u32,
    /// The resulting safe mode, never a request to replay effects.
    pub mode: StorageMode,
    /// Bounded record of the stores that were checked.
    pub checked: Vec<String>,
}

/// Coordinates maintenance ownership across local SQLite, JSON, and artifact roots.
#[derive(Clone, Debug)]
pub struct StorageCoordinator {
    root: PathBuf,
}

impl StorageCoordinator {
    /// Opens the fixed local-store root; it does not select a history backend.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Validates supported files before canonical writers are released.
    pub fn check_startup(&self) -> Result<IntegrityReport, StoreError> {
        let _lock = self.maintenance_lock()?;
        let mut checked = vec![format!("storage-format-v{STORAGE_FORMAT_VERSION}")];
        let history = self.root.join("history.sqlite");
        if history.exists() {
            let connection = Connection::open(&history)?;
            let integrity: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            checked.push("history.sqlite: integrity_check".into());
            if integrity != "ok" {
                return Ok(IntegrityReport {
                    format_version: STORAGE_FORMAT_VERSION,
                    mode: StorageMode::InspectableReadOnly {
                        reason: format!("SQLite integrity check failed: {integrity}"),
                    },
                    checked,
                });
            }
        }
        for collection in ["configuration", "workflows"] {
            let manifest = self.root.join(collection).join("manifest.json");
            if manifest.exists() {
                serde_json::from_slice::<serde_json::Value>(&fs::read(&manifest)?)?;
                checked.push(format!("{collection}: manifest JSON"));
            }
        }
        Ok(IntegrityReport {
            format_version: STORAGE_FORMAT_VERSION,
            mode: StorageMode::ReadWrite,
            checked,
        })
    }

    /// Creates a staged, last-known-good backup without overwriting evidence.
    pub fn create_backup(&self, backup_root: impl AsRef<Path>) -> Result<PathBuf, StoreError> {
        let _lock = self.maintenance_lock()?;
        let backup_root = backup_root.as_ref();
        if backup_root.starts_with(&self.root) {
            return Err(StoreError::BackupLocationInsideStore);
        }
        fs::create_dir_all(backup_root)?;
        let name = format!(
            "backup-v{STORAGE_FORMAT_VERSION}-{}",
            BACKUP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let staging = backup_root.join(format!(".{name}.staging"));
        let final_path = backup_root.join(&name);
        copy_tree(&self.root, &staging)?;
        fs::write(
            staging.join("backup-manifest.json"),
            serde_json::to_vec(
                &serde_json::json!({"formatVersion": STORAGE_FORMAT_VERSION, "complete": true}),
            )?,
        )?;
        sync_tree(&staging)?;
        fs::rename(&staging, &final_path)?;
        Ok(final_path)
    }

    fn maintenance_lock(&self) -> Result<MaintenanceLock, StoreError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(".maintenance.lock"))?;
        file.lock_exclusive()?;
        Ok(MaintenanceLock { file })
    }
}

/// Releases the cross-process maintenance gate when an operation finishes.
struct MaintenanceLock {
    file: File,
}
impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        if source_path
            .file_name()
            .is_some_and(|name| name == ".maintenance.lock")
        {
            continue;
        }
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(&source_path, destination_path)?;
        }
    }
    Ok(())
}

fn sync_tree(root: &Path) -> Result<(), StoreError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else if entry.file_type()?.is_file() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn startup_gate_and_backup_are_explicit_and_non_destructive() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("aworkit-storage-{nonce}"));
        let coordinator = StorageCoordinator::open(&root).expect("coordinator");
        assert_eq!(
            coordinator.check_startup().expect("check").mode,
            StorageMode::ReadWrite
        );
        fs::write(root.join("evidence.txt"), b"immutable").expect("evidence");
        let backup = coordinator
            .create_backup(root.with_extension("backups"))
            .expect("backup");
        assert_eq!(
            fs::read(backup.join("evidence.txt")).expect("backup data"),
            b"immutable"
        );
        fs::remove_dir_all(root.with_extension("backups")).expect("backup cleanup");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
