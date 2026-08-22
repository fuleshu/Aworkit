//! Store construction, physical layout, and forward-schema mode.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use rusqlite::Connection;

use crate::maintenance::MaintenanceGate;

use super::{
    common::{CAPTURE_SCHEMA_VERSION, ensure_schema},
    model::{CaptureError, CaptureStoreMode},
};

/// `SQLite` manifest and bounded chunk-file repository.
#[derive(Clone)]
pub struct DebugCaptureStore {
    pub(super) root: Arc<PathBuf>,
    pub(super) gate: MaintenanceGate,
    pub(super) connection: Arc<Mutex<Connection>>,
    pub(super) mode: CaptureStoreMode,
}

impl DebugCaptureStore {
    /// Opens the standard capture store below one local-store root.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be initialized, the manifest is
    /// corrupt, or the schema cannot be opened safely.
    pub fn for_store_root(root: impl AsRef<Path>) -> Result<Self, CaptureError> {
        let root = absolute_directory(root.as_ref())?;
        let gate = MaintenanceGate::for_root(&root)?;
        let _lease = gate.shared()?;
        let payload_root = root.join("debug-captures");
        fs::create_dir_all(&payload_root)?;
        let connection = Connection::open(root.join("debug-capture.sqlite"))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA busy_timeout=5000;",
        )?;
        let found: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let mode = if found > CAPTURE_SCHEMA_VERSION {
            connection.execute_batch("PRAGMA query_only=ON;")?;
            CaptureStoreMode::InspectableReadOnly {
                found_schema: u32::try_from(found).unwrap_or(u32::MAX),
            }
        } else {
            ensure_schema(&connection)?;
            CaptureStoreMode::ReadWrite
        };
        Ok(Self {
            root: Arc::new(root),
            gate,
            connection: Arc::new(Mutex::new(connection)),
            mode,
        })
    }

    #[must_use]
    pub fn mode(&self) -> CaptureStoreMode {
        self.mode
    }

    pub(super) fn capture_directory(&self, capture_id: &str) -> PathBuf {
        self.root.join("debug-captures").join(capture_id)
    }

    pub(super) fn chunk_path(&self, capture_id: &str, ordinal: u64) -> PathBuf {
        self.capture_directory(capture_id)
            .join(format!("{ordinal:020}.rle"))
    }

    pub(super) fn require_writable(&self) -> Result<(), CaptureError> {
        match self.mode {
            CaptureStoreMode::ReadWrite => Ok(()),
            CaptureStoreMode::InspectableReadOnly { found_schema } => {
                Err(CaptureError::ForwardSchema { found_schema })
            }
        }
    }

    pub(super) fn lock(&self) -> Result<MutexGuard<'_, Connection>, CaptureError> {
        self.connection.lock().map_err(|_| CaptureError::Poisoned)
    }
}

fn absolute_directory(path: &Path) -> Result<PathBuf, CaptureError> {
    fs::create_dir_all(path)?;
    Ok(fs::canonicalize(path)?)
}
