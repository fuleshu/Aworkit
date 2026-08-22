//! SQLite persistence for core-governed trusted-extension facts.
//!
//! This adapter accepts already parsed and evaluated metadata. It has no API
//! for filesystem discovery, hashing package contents, loading libraries, or
//! starting extension processes.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use aworkit_protocol::{
    ExtensionAuditEntryV1, ExtensionAuditKindV1, ExtensionIdentityV1, ExtensionInventoryPort,
    ExtensionInventoryPortErrorV1, ExtensionInventoryWriteV1, ExtensionRecordV1, StableId,
    is_canonical_sha256,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::maintenance::MaintenanceGate;

const INVENTORY_SCHEMA_VERSION: i32 = 1;
const MAX_RECORD_BYTES: usize = 512 * 1024;
const MAX_CONTRIBUTIONS: usize = 256;
const MAX_DEPENDENCIES: usize = 256;
const MAX_AUDIT_DETAIL_BYTES: usize = 4096;
const MAX_AUDIT_PAGE: u32 = 256;

/// Whether the inventory can accept writes with the current schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionInventoryMode {
    ReadWrite,
    InspectableReadOnly { found_schema: u32 },
}

/// Persisted extension inventory and immutable mutation audit.
#[derive(Clone)]
pub struct ExtensionInventory {
    path: Arc<PathBuf>,
    gate: MaintenanceGate,
    connection: Arc<Mutex<Connection>>,
    mode: ExtensionInventoryMode,
}

impl ExtensionInventory {
    /// Opens the standard inventory database below a local-store root.
    pub fn for_store_root(root: impl AsRef<Path>) -> Result<Self, ExtensionInventoryError> {
        Self::open(root.as_ref().join("extension-inventory.sqlite"))
    }

    /// Opens or creates the metadata-only inventory. A newer schema is opened
    /// with SQLite `query_only` enabled instead of being downgraded.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExtensionInventoryError> {
        let path = absolute_file_path(path.as_ref())?;
        let root = path
            .parent()
            .ok_or(ExtensionInventoryError::InvalidPath)?
            .to_path_buf();
        fs::create_dir_all(&root)?;
        let gate = MaintenanceGate::for_root(&root)?;
        let _lease = gate.shared()?;
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA busy_timeout=5000;",
        )?;
        let found: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let mode = if found > INVENTORY_SCHEMA_VERSION {
            connection.execute_batch("PRAGMA query_only=ON;")?;
            ExtensionInventoryMode::InspectableReadOnly {
                found_schema: u32::try_from(found).unwrap_or(u32::MAX),
            }
        } else {
            ensure_schema(&connection)?;
            ExtensionInventoryMode::ReadWrite
        };
        Ok(Self {
            path: Arc::new(path),
            gate,
            connection: Arc::new(Mutex::new(connection)),
            mode,
        })
    }

    /// Returns the physical database path for backup and integrity tooling.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.path.as_ref()
    }

    /// Reports whether forward-schema protection disabled mutation.
    #[must_use]
    pub fn mode(&self) -> ExtensionInventoryMode {
        self.mode
    }

    /// Loads one exact version/hash identity and verifies its stored record hash.
    pub fn load_record(
        &self,
        identity: &ExtensionIdentityV1,
    ) -> Result<Option<ExtensionRecordV1>, ExtensionInventoryError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_record(&connection, identity)
    }

    /// Lists deterministic exact identities, optionally filtered by extension ID.
    pub fn list_records(
        &self,
        extension_id: Option<&StableId>,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionInventoryError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut records = if let Some(extension_id) = extension_id {
            let mut statement = connection.prepare(
                "SELECT record_json, record_hash FROM extension_records
                 WHERE extension_id=?1 ORDER BY version, content_hash",
            )?;
            statement
                .query_map([extension_id.as_str()], read_record_columns)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut statement = connection.prepare(
                "SELECT record_json, record_hash FROM extension_records
                 ORDER BY extension_id, version, content_hash",
            )?;
            statement
                .query_map([], read_record_columns)?
                .collect::<Result<Vec<_>, _>>()?
        };
        for record in &records {
            validate_record(record)?;
        }
        records.sort_by(|left, right| left.identity().cmp(right.identity()));
        Ok(records)
    }

    /// Finds every exact installed identity declaring a contribution ID.
    pub fn records_for_contribution(
        &self,
        contribution_id: &StableId,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionInventoryError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT r.record_json, r.record_hash
             FROM extension_records r
             JOIN extension_contributions c
               ON c.extension_id=r.extension_id
              AND c.version=r.version
              AND c.content_hash=r.content_hash
             WHERE c.contribution_id=?1
             ORDER BY r.extension_id, r.version, r.content_hash",
        )?;
        let records = statement
            .query_map([contribution_id.as_str()], read_record_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        for record in &records {
            validate_record(record)?;
        }
        Ok(records)
    }

    /// Atomically applies one expected-version record and its immutable audit row.
    pub fn write_record(
        &self,
        request: &ExtensionInventoryWriteV1,
    ) -> Result<ExtensionRecordV1, ExtensionInventoryError> {
        if let ExtensionInventoryMode::InspectableReadOnly { found_schema } = self.mode {
            return Err(ExtensionInventoryError::ForwardSchema { found_schema });
        }
        validate_write(request)?;
        let request_hash = canonical_hash(request)?;
        let record_json = canonical_json(&request.record)?;
        let record_hash = hash_bytes(&record_json);
        let identity = request.record.identity();
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some((existing_hash, existing_record)) = transaction
            .query_row(
                "SELECT request_hash, record_json FROM extension_audit
                 WHERE operation_id=?1",
                [request.operation_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if existing_hash != request_hash {
                return Err(ExtensionInventoryError::OperationConflict);
            }
            let record: ExtensionRecordV1 = serde_json::from_str(&existing_record)?;
            validate_record(&record)?;
            transaction.commit()?;
            return Ok(record);
        }

        let current = load_record(&transaction, identity)?;
        let prior_hash = current
            .as_ref()
            .map(|record| canonical_hash(record))
            .transpose()?;
        match (&current, request.expected_version) {
            (None, None) if request.record.record_version == 1 => {}
            (None, Some(_)) => return Err(ExtensionInventoryError::Missing),
            (None, None) => return Err(ExtensionInventoryError::InvalidRecordVersion),
            (Some(_), None) => return Err(ExtensionInventoryError::AlreadyExists),
            (Some(current), Some(expected)) => {
                if current.record_version != expected {
                    return Err(ExtensionInventoryError::VersionConflict {
                        expected,
                        actual: current.record_version,
                    });
                }
                if request.record.record_version != expected.saturating_add(1) {
                    return Err(ExtensionInventoryError::InvalidRecordVersion);
                }
                if current.manifest != request.record.manifest {
                    return Err(ExtensionInventoryError::IdentityMutation);
                }
            }
        }

        transaction.execute(
            "INSERT INTO extension_records(
                extension_id, version, content_hash, record_version, record_json, record_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(extension_id, version, content_hash) DO UPDATE SET
                record_version=excluded.record_version,
                record_json=excluded.record_json,
                record_hash=excluded.record_hash",
            params![
                identity.extension_id.as_str(),
                identity.version,
                identity.content_hash,
                checked_i64(request.record.record_version)?,
                std::str::from_utf8(&record_json).map_err(|_| ExtensionInventoryError::Encoding)?,
                record_hash,
            ],
        )?;
        transaction.execute(
            "DELETE FROM extension_contributions
             WHERE extension_id=?1 AND version=?2 AND content_hash=?3",
            params![
                identity.extension_id.as_str(),
                identity.version,
                identity.content_hash,
            ],
        )?;
        for contribution in &request.record.manifest.contributions {
            transaction.execute(
                "INSERT INTO extension_contributions(
                    extension_id, version, content_hash, contribution_id,
                    capability_id, descriptor_hash
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    identity.extension_id.as_str(),
                    identity.version,
                    identity.content_hash,
                    contribution.contribution_id.as_str(),
                    contribution.descriptor.capability_id.as_str(),
                    contribution.descriptor.descriptor_hash,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO extension_audit(
                operation_id, request_hash, extension_id, version, content_hash,
                record_version, kind, prior_record_hash, record_hash, detail, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                request.operation_id.as_str(),
                request_hash,
                identity.extension_id.as_str(),
                identity.version,
                identity.content_hash,
                checked_i64(request.record.record_version)?,
                audit_kind_name(request.audit_kind),
                prior_hash,
                record_hash,
                request.detail,
                std::str::from_utf8(&record_json).map_err(|_| ExtensionInventoryError::Encoding)?,
            ],
        )?;
        transaction.commit()?;
        Ok(request.record.clone())
    }

    /// Reads immutable audit entries after a durable sequence cursor.
    pub fn audit(
        &self,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<ExtensionAuditEntryV1>, ExtensionInventoryError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT sequence, operation_id, extension_id, version, content_hash,
                    record_version, kind, prior_record_hash, record_hash, detail, record_json
             FROM extension_audit WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let entries = statement
            .query_map(
                params![
                    checked_i64(after_sequence)?,
                    i64::from(limit.clamp(1, MAX_AUDIT_PAGE)),
                ],
                |row| {
                    let sequence = checked_u64_column(row.get(0)?, 0)?;
                    let operation_id = parse_id_column(row.get(1)?, 1)?;
                    let extension_id = parse_id_column(row.get(2)?, 2)?;
                    let version = row.get(3)?;
                    let content_hash = row.get(4)?;
                    let record_version = checked_u64_column(row.get(5)?, 5)?;
                    let kind_text: String = row.get(6)?;
                    let kind =
                        parse_audit_kind(&kind_text).map_err(|_| rusqlite::Error::InvalidQuery)?;
                    let record_json: String = row.get(10)?;
                    let record = serde_json::from_str(&record_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            10,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(ExtensionAuditEntryV1 {
                        sequence,
                        operation_id,
                        identity: ExtensionIdentityV1 {
                            extension_id,
                            version,
                            content_hash,
                        },
                        record_version,
                        kind,
                        prior_record_hash: row.get(7)?,
                        record_hash: row.get(8)?,
                        detail: row.get(9)?,
                        record,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        for entry in &entries {
            validate_record(&entry.record)?;
            if canonical_hash(&entry.record)? != entry.record_hash
                || entry.record.record_version != entry.record_version
                || entry.record.identity() != &entry.identity
            {
                return Err(ExtensionInventoryError::Corrupt);
            }
        }
        Ok(entries)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, ExtensionInventoryError> {
        self.connection
            .lock()
            .map_err(|_| ExtensionInventoryError::Poisoned)
    }
}

impl ExtensionInventoryPort for ExtensionInventory {
    fn load(
        &self,
        identity: &ExtensionIdentityV1,
    ) -> Result<Option<ExtensionRecordV1>, ExtensionInventoryPortErrorV1> {
        self.load_record(identity).map_err(port_error)
    }

    fn list(
        &self,
        extension_id: Option<&StableId>,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionInventoryPortErrorV1> {
        self.list_records(extension_id).map_err(port_error)
    }

    fn find_by_contribution(
        &self,
        contribution_id: &StableId,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionInventoryPortErrorV1> {
        self.records_for_contribution(contribution_id)
            .map_err(port_error)
    }

    fn write(
        &self,
        request: &ExtensionInventoryWriteV1,
    ) -> Result<ExtensionRecordV1, ExtensionInventoryPortErrorV1> {
        self.write_record(request).map_err(port_error)
    }

    fn audit_entries(
        &self,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<ExtensionAuditEntryV1>, ExtensionInventoryPortErrorV1> {
        self.audit(after_sequence, limit).map_err(port_error)
    }
}

fn ensure_schema(connection: &Connection) -> Result<(), ExtensionInventoryError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS extension_records (
             extension_id TEXT NOT NULL,
             version TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             record_version INTEGER NOT NULL CHECK(record_version > 0),
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             PRIMARY KEY(extension_id, version, content_hash)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS extension_contributions (
             extension_id TEXT NOT NULL,
             version TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             contribution_id TEXT NOT NULL,
             capability_id TEXT NOT NULL,
             descriptor_hash TEXT NOT NULL,
             PRIMARY KEY(extension_id, version, content_hash, contribution_id),
             FOREIGN KEY(extension_id, version, content_hash)
               REFERENCES extension_records(extension_id, version, content_hash)
               ON DELETE CASCADE
         ) STRICT;
         CREATE INDEX IF NOT EXISTS extension_contribution_lookup
             ON extension_contributions(contribution_id, capability_id);
         CREATE TABLE IF NOT EXISTS extension_audit (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             operation_id TEXT NOT NULL UNIQUE,
             request_hash TEXT NOT NULL,
             extension_id TEXT NOT NULL,
             version TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             record_version INTEGER NOT NULL CHECK(record_version > 0),
             kind TEXT NOT NULL,
             prior_record_hash TEXT,
             record_hash TEXT NOT NULL,
             detail TEXT NOT NULL,
             record_json TEXT NOT NULL,
             UNIQUE(extension_id, version, content_hash, record_version)
         ) STRICT;
         CREATE TRIGGER IF NOT EXISTS extension_audit_immutable_update
         BEFORE UPDATE ON extension_audit BEGIN
             SELECT RAISE(ABORT, 'extension audit is immutable');
         END;
         CREATE TRIGGER IF NOT EXISTS extension_audit_immutable_delete
         BEFORE DELETE ON extension_audit BEGIN
             SELECT RAISE(ABORT, 'extension audit is immutable');
         END;
         PRAGMA user_version=1;",
    )?;
    Ok(())
}

fn load_record(
    connection: &Connection,
    identity: &ExtensionIdentityV1,
) -> Result<Option<ExtensionRecordV1>, ExtensionInventoryError> {
    let row = connection
        .query_row(
            "SELECT record_json, record_hash FROM extension_records
             WHERE extension_id=?1 AND version=?2 AND content_hash=?3",
            params![
                identity.extension_id.as_str(),
                identity.version,
                identity.content_hash,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|(json, hash)| decode_record(&json, &hash))
        .transpose()
}

fn read_record_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExtensionRecordV1> {
    let json: String = row.get(0)?;
    let expected_hash: String = row.get(1)?;
    decode_record(&json, &expected_hash).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn decode_record(
    json: &str,
    expected_hash: &str,
) -> Result<ExtensionRecordV1, ExtensionInventoryError> {
    let record: ExtensionRecordV1 = serde_json::from_str(json)?;
    validate_record(&record)?;
    if canonical_hash(&record)? != expected_hash {
        return Err(ExtensionInventoryError::Corrupt);
    }
    Ok(record)
}

fn validate_write(request: &ExtensionInventoryWriteV1) -> Result<(), ExtensionInventoryError> {
    validate_record(&request.record)?;
    if request.detail.is_empty()
        || request.detail.len() > MAX_AUDIT_DETAIL_BYTES
        || request.detail.contains('\0')
    {
        return Err(ExtensionInventoryError::InvalidAuditDetail);
    }
    Ok(())
}

fn validate_record(record: &ExtensionRecordV1) -> Result<(), ExtensionInventoryError> {
    let manifest = &record.manifest;
    if record.record_version == 0
        || record.record_version > i64::MAX as u64
        || manifest.identity.version.is_empty()
        || manifest.identity.version.len() > 128
        || !is_canonical_sha256(&manifest.identity.content_hash)
        || manifest.entry_point_identity.is_empty()
        || manifest.entry_point_identity.len() > 4096
        || manifest.entry_point_identity.contains('\0')
        || manifest.contributions.len() > MAX_CONTRIBUTIONS
        || manifest.dependencies.len() > MAX_DEPENDENCIES
        || (!record.installed && record.enabled)
    {
        return Err(ExtensionInventoryError::InvalidRecord);
    }
    if let Some(hash) = &manifest.configuration_schema_hash {
        if !is_canonical_sha256(hash) {
            return Err(ExtensionInventoryError::InvalidRecord);
        }
    }
    if let Some(quarantine) = &record.quarantine {
        if !valid_text(&quarantine.code, 128) || !valid_text(&quarantine.message, 4096) {
            return Err(ExtensionInventoryError::InvalidRecord);
        }
    }
    if let Some(attestation) = &record.last_attestation {
        if attestation.host_generation.0 == 0
            || !is_canonical_sha256(&attestation.handshake_hash)
            || !is_canonical_sha256(&attestation.descriptor_set_hash)
            || !is_canonical_sha256(&attestation.dependency_snapshot_hash)
        {
            return Err(ExtensionInventoryError::InvalidRecord);
        }
    }
    let mut prior_contribution: Option<&str> = None;
    for contribution in &manifest.contributions {
        let contribution_id = contribution.contribution_id.as_str();
        if prior_contribution.is_some_and(|prior| prior >= contribution_id)
            || contribution.descriptor.maximum_concurrency == 0
            || contribution.descriptor.max_input_bytes == 0
            || contribution.descriptor.max_output_bytes == 0
            || !is_canonical_sha256(&contribution.descriptor.descriptor_hash)
            || !strictly_sorted_unique(&contribution.descriptor.allowed_scopes)
            || !strictly_sorted_unique(&contribution.descriptor.secret_slots)
            || !strictly_sorted_unique(&contribution.descriptor.supported_platforms)
        {
            return Err(ExtensionInventoryError::InvalidRecord);
        }
        for hash in [
            contribution.descriptor.input_schema_hash.as_deref(),
            contribution.descriptor.output_schema_hash.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !is_canonical_sha256(hash) {
                return Err(ExtensionInventoryError::InvalidRecord);
            }
        }
        prior_contribution = Some(contribution_id);
    }
    let encoded = canonical_json(record)?;
    if encoded.len() > MAX_RECORD_BYTES {
        return Err(ExtensionInventoryError::RecordTooLarge);
    }
    Ok(())
}

fn strictly_sorted_unique(values: &[String]) -> bool {
    values.iter().all(|value| valid_text(value, 256))
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.contains('\0')
}

fn canonical_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ExtensionInventoryError> {
    serde_jcs::to_vec(value).map_err(|_| ExtensionInventoryError::Encoding)
}

fn canonical_hash<T: serde::Serialize>(value: &T) -> Result<String, ExtensionInventoryError> {
    Ok(hash_bytes(&canonical_json(value)?))
}

fn hash_bytes(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn checked_i64(value: u64) -> Result<i64, ExtensionInventoryError> {
    i64::try_from(value).map_err(|_| ExtensionInventoryError::InvalidRecordVersion)
}

fn checked_u64_column(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn parse_id_column(value: String, column: usize) -> rusqlite::Result<StableId> {
    StableId::parse(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn audit_kind_name(kind: ExtensionAuditKindV1) -> &'static str {
    match kind {
        ExtensionAuditKindV1::Registered => "registered",
        ExtensionAuditKindV1::EnablementChanged => "enablement_changed",
        ExtensionAuditKindV1::IntegrityEvaluated => "integrity_evaluated",
        ExtensionAuditKindV1::CompatibilityEvaluated => "compatibility_evaluated",
        ExtensionAuditKindV1::Quarantined => "quarantined",
        ExtensionAuditKindV1::Attested => "attested",
        ExtensionAuditKindV1::Removed => "removed",
    }
}

fn parse_audit_kind(value: &str) -> Result<ExtensionAuditKindV1, ExtensionInventoryError> {
    match value {
        "registered" => Ok(ExtensionAuditKindV1::Registered),
        "enablement_changed" => Ok(ExtensionAuditKindV1::EnablementChanged),
        "integrity_evaluated" => Ok(ExtensionAuditKindV1::IntegrityEvaluated),
        "compatibility_evaluated" => Ok(ExtensionAuditKindV1::CompatibilityEvaluated),
        "quarantined" => Ok(ExtensionAuditKindV1::Quarantined),
        "attested" => Ok(ExtensionAuditKindV1::Attested),
        "removed" => Ok(ExtensionAuditKindV1::Removed),
        _ => Err(ExtensionInventoryError::Corrupt),
    }
}

fn absolute_file_path(path: &Path) -> Result<PathBuf, ExtensionInventoryError> {
    if path.as_os_str().is_empty() || path.file_name().is_none() {
        return Err(ExtensionInventoryError::InvalidPath);
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn port_error(error: ExtensionInventoryError) -> ExtensionInventoryPortErrorV1 {
    let (code, retryable, inspectable_read_only) = match error {
        ExtensionInventoryError::VersionConflict { .. } => ("version_conflict", true, false),
        ExtensionInventoryError::ForwardSchema { .. } => ("forward_schema", false, true),
        ExtensionInventoryError::Poisoned => ("inventory_unavailable", true, false),
        ExtensionInventoryError::Sqlite(_) | ExtensionInventoryError::Io(_) => {
            ("inventory_storage", true, false)
        }
        ExtensionInventoryError::Corrupt => ("inventory_corrupt", false, true),
        _ => ("invalid_inventory_mutation", false, false),
    };
    ExtensionInventoryPortErrorV1 {
        code: code.into(),
        message: error.to_string(),
        retryable,
        inspectable_read_only,
    }
}

/// Inventory persistence and compare-and-swap failures.
#[derive(Debug, Error)]
pub enum ExtensionInventoryError {
    #[error("extension inventory filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("extension inventory database operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("extension inventory JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("extension inventory path is invalid")]
    InvalidPath,
    #[error("extension inventory connection is unavailable after a panic")]
    Poisoned,
    #[error("extension inventory schema {found_schema} is inspectable read-only")]
    ForwardSchema { found_schema: u32 },
    #[error("extension inventory record is malformed")]
    InvalidRecord,
    #[error("extension inventory record exceeds its bounded size")]
    RecordTooLarge,
    #[error("extension inventory audit detail is malformed")]
    InvalidAuditDetail,
    #[error("extension inventory record version is invalid")]
    InvalidRecordVersion,
    #[error("extension inventory record already exists")]
    AlreadyExists,
    #[error("extension inventory record does not exist")]
    Missing,
    #[error("extension inventory version conflict: expected {expected}, found {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("an exact extension identity cannot change its manifest")]
    IdentityMutation,
    #[error("extension inventory operation ID was reused for different facts")]
    OperationConflict,
    #[error("extension inventory contains corrupt or inconsistent facts")]
    Corrupt,
    #[error("extension inventory canonical encoding failed")]
    Encoding,
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use aworkit_protocol::{
        CapabilityDescriptorV1, CapabilityKindV1, CapabilitySideEffectV1, CapabilityVisibilityV1,
        ExtensionCompatibilityRangeV1, ExtensionCompatibilityStatusV1, ExtensionContributionV1,
        ExtensionIntegrityStatusV1, ExtensionManifestV1, ExtensionProvenanceV1,
        capability_descriptor_hash_v1,
    };

    use super::*;

    fn id(value: &str) -> StableId {
        StableId::parse(value).expect("stable ID")
    }

    fn path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("aworkit-extension-inventory-{nonce}"))
            .join("inventory.sqlite")
    }

    fn record() -> ExtensionRecordV1 {
        let mut descriptor = CapabilityDescriptorV1 {
            capability_id: id("tool.search"),
            adapter_version: "1.0.0".into(),
            kind: CapabilityKindV1::FileSearch,
            side_effect: CapabilitySideEffectV1::ReadOnly,
            guarantees_same_id_deduplication: true,
            supports_streaming: true,
            supports_cancellation: true,
            supports_continuation: false,
            supports_sessions: false,
            supports_approval_forwarding: false,
            supports_mcp_forwarding: false,
            allowed_scopes: vec!["project.read".into(), "project.search".into()],
            secret_slots: Vec::new(),
            input_schema_hash: None,
            output_schema_hash: None,
            requires_workspace: true,
            required_isolation: None,
            maximum_concurrency: 2,
            max_input_bytes: 1024,
            max_output_bytes: 4096,
            supported_platforms: vec!["linux".into()],
            visibility: CapabilityVisibilityV1::Mediated,
            descriptor_hash: String::new(),
        };
        descriptor.descriptor_hash =
            capability_descriptor_hash_v1(&descriptor).expect("descriptor hash");
        ExtensionRecordV1 {
            manifest: ExtensionManifestV1 {
                schema_version: 1,
                identity: ExtensionIdentityV1 {
                    extension_id: id("extension.search"),
                    version: "1.0.0".into(),
                    content_hash: format!("sha256:{}", "a".repeat(64)),
                },
                compatibility: ExtensionCompatibilityRangeV1 {
                    minimum_aworkit_version: "0.1.0".into(),
                    maximum_aworkit_version_exclusive: Some("1.0.0".into()),
                    minimum_host_protocol: 1,
                    maximum_host_protocol: 1,
                },
                entry_point_identity: "bin/search".into(),
                configuration_schema_hash: None,
                contributions: vec![ExtensionContributionV1 {
                    contribution_id: id("contribution.search"),
                    descriptor,
                }],
                dependencies: Vec::new(),
                provenance: ExtensionProvenanceV1 {
                    source: "local-test".into(),
                    publisher: None,
                    signature_status: "unavailable".into(),
                    signature_identity: None,
                },
            },
            installed: true,
            enabled: false,
            integrity: ExtensionIntegrityStatusV1::Verified,
            compatibility: ExtensionCompatibilityStatusV1::Compatible {
                aworkit_version: "0.1.0".into(),
                host_protocol: 1,
            },
            quarantine: None,
            record_version: 1,
            last_attestation: None,
        }
    }

    #[test]
    fn record_and_audit_commit_atomically_with_idempotent_operation() {
        let path = path();
        let inventory = ExtensionInventory::open(&path).expect("inventory");
        let request = ExtensionInventoryWriteV1 {
            operation_id: id("operation.register"),
            expected_version: None,
            record: record(),
            audit_kind: ExtensionAuditKindV1::Registered,
            detail: "registered inert manifest".into(),
        };
        let first = inventory.write_record(&request).expect("write");
        let replay = inventory.write_record(&request).expect("idempotent replay");
        assert_eq!(first, replay);
        assert_eq!(
            inventory
                .records_for_contribution(&id("contribution.search"))
                .expect("lookup"),
            vec![first.clone()]
        );
        let audit = inventory.audit(0, 10).expect("audit");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].record, first);
        assert!(audit[0].prior_record_hash.is_none());

        let direct = Connection::open(&path).expect("direct");
        assert!(direct.execute("DELETE FROM extension_audit", []).is_err());
        drop(direct);
        drop(inventory);
        remove_sqlite_files(&path);
    }

    #[test]
    fn expected_versions_preserve_manifest_identity_and_audit_chain() {
        let path = path();
        let inventory = ExtensionInventory::open(&path).expect("inventory");
        let initial = ExtensionInventoryWriteV1 {
            operation_id: id("operation.initial"),
            expected_version: None,
            record: record(),
            audit_kind: ExtensionAuditKindV1::Registered,
            detail: "initial".into(),
        };
        inventory.write_record(&initial).expect("initial");
        let mut enabled = initial.record.clone();
        enabled.enabled = true;
        enabled.record_version = 2;
        inventory
            .write_record(&ExtensionInventoryWriteV1 {
                operation_id: id("operation.enable"),
                expected_version: Some(1),
                record: enabled.clone(),
                audit_kind: ExtensionAuditKindV1::EnablementChanged,
                detail: "explicitly enabled".into(),
            })
            .expect("enable");
        let mut stale = enabled.clone();
        stale.record_version = 3;
        assert!(matches!(
            inventory.write_record(&ExtensionInventoryWriteV1 {
                operation_id: id("operation.stale"),
                expected_version: Some(1),
                record: stale,
                audit_kind: ExtensionAuditKindV1::EnablementChanged,
                detail: "stale".into(),
            }),
            Err(ExtensionInventoryError::VersionConflict { actual: 2, .. })
        ));
        let audit = inventory.audit(0, 10).expect("audit");
        assert_eq!(audit.len(), 2);
        assert_eq!(
            audit[1].prior_record_hash.as_deref(),
            Some(audit[0].record_hash.as_str())
        );
        drop(inventory);
        remove_sqlite_files(&path);
    }

    #[test]
    fn newer_schema_is_inspectable_but_not_writable() {
        let path = path();
        let inventory = ExtensionInventory::open(&path).expect("inventory");
        inventory
            .write_record(&ExtensionInventoryWriteV1 {
                operation_id: id("operation.seed"),
                expected_version: None,
                record: record(),
                audit_kind: ExtensionAuditKindV1::Registered,
                detail: "seed".into(),
            })
            .expect("seed");
        drop(inventory);
        let future = Connection::open(&path).expect("future");
        future
            .execute_batch("PRAGMA user_version=99;")
            .expect("future schema");
        drop(future);
        let inventory = ExtensionInventory::open(&path).expect("read-only inventory");
        assert_eq!(
            inventory.mode(),
            ExtensionInventoryMode::InspectableReadOnly { found_schema: 99 }
        );
        assert_eq!(inventory.list_records(None).expect("inspect").len(), 1);
        assert!(matches!(
            inventory.write_record(&ExtensionInventoryWriteV1 {
                operation_id: id("operation.blocked"),
                expected_version: Some(1),
                record: {
                    let mut record = record();
                    record.record_version = 2;
                    record
                },
                audit_kind: ExtensionAuditKindV1::EnablementChanged,
                detail: "blocked".into(),
            }),
            Err(ExtensionInventoryError::ForwardSchema { found_schema: 99 })
        ));
        drop(inventory);
        remove_sqlite_files(&path);
    }

    fn remove_sqlite_files(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{}{suffix}", path.display()));
            let _ = fs::remove_file(candidate);
        }
        if let Some(root) = path.parent() {
            let lock = root.parent().and_then(|parent| {
                root.file_name().map(|name| {
                    parent.join(format!(
                        ".{}.aworkit-maintenance.lock",
                        name.to_string_lossy()
                    ))
                })
            });
            let _ = fs::remove_dir(root);
            if let Some(lock) = lock {
                let _ = fs::remove_file(lock);
            }
        }
    }
}
