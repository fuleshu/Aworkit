//! Durable content-addressed local evidence objects and staged commit tokens.
//!
//! Object bytes are promoted and synced before a semantic commit. Metadata and
//! event references become canonical only through `LocalHistoryStore::commit_v1`,
//! which owns the shared SQLite transaction.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_protocol::{PreparedArtifactRefV1, StableId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{StoreError, database::open_history_database, maintenance::MaintenanceGate};

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PREPARED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RANGE_BYTES: usize = 8 * 1024 * 1024;

/// A durable staging identity that must be echoed exactly into a semantic commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactToken {
    pub token_id: String,
    pub artifact_id: String,
    pub content_hash: String,
    pub byte_size: u64,
    pub staging_generation: u64,
}

impl ArtifactToken {
    /// Creates the process-neutral reference admitted by the history commit port.
    pub fn reference_for(
        &self,
        origin_event_id: &str,
    ) -> Result<PreparedArtifactRefV1, StoreError> {
        Ok(PreparedArtifactRefV1 {
            token_id: stable(&self.token_id)?,
            artifact_id: stable(&self.artifact_id)?,
            content_hash: self.content_hash.clone(),
            byte_size: self.byte_size,
            staging_generation: self.staging_generation,
            origin_event_id: stable(origin_event_id)?,
        })
    }
}

/// Immutable artifact metadata and its first stable semantic reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactMetadata {
    pub artifact_id: String,
    pub content_hash: String,
    pub byte_size: u64,
    pub media_type: String,
    pub logical_name: String,
    pub origin_event_id: Option<String>,
    pub availability: String,
    pub retention_class: String,
}

/// Local content store sharing the canonical `history.sqlite` transaction log.
#[derive(Clone)]
pub struct ArtifactStore {
    root: Arc<PathBuf>,
    gate: MaintenanceGate,
    connection: Arc<Mutex<Connection>>,
}

impl ArtifactStore {
    /// Opens the evidence root and the shared local-history metadata database.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = absolute(&root.into())?;
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join(".artifact-staging"))?;
        let gate = MaintenanceGate::for_root(&root)?;
        let _lease = gate.shared()?;
        let connection = open_history_database(&root.join("history.sqlite"))?;
        Ok(Self {
            root: Arc::new(root),
            gate,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Stages a bounded immutable byte slice.
    pub fn prepare(
        &self,
        artifact_id: &str,
        media_type: &str,
        logical_name: &str,
        bytes: &[u8],
    ) -> Result<ArtifactToken, StoreError> {
        self.prepare_reader(artifact_id, media_type, logical_name, bytes)
    }

    /// Streams, hashes, bounds, syncs, and promotes an immutable object before
    /// recording its opaque staging token.
    pub fn prepare_reader(
        &self,
        artifact_id: &str,
        media_type: &str,
        logical_name: &str,
        mut reader: impl Read,
    ) -> Result<ArtifactToken, StoreError> {
        validate_identifier(artifact_id)?;
        validate_metadata(media_type)?;
        validate_metadata(logical_name)?;
        let _lease = self.gate.shared()?;

        let temporary = self.root.join(".artifact-staging").join(format!(
            ".object-{}-{}.tmp",
            std::process::id(),
            now_nanos()?
        ));
        let streamed = stream_temporary(&temporary, &mut reader);
        let (content_hash, byte_size) = match streamed {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        let result = (|| {
            // Holding the immediate transaction across object promotion prevents
            // a concurrent orphan sweep from observing the normal object-before-
            // metadata window. A crash can still leave an unindexed object, which
            // `collect_orphans` deliberately discovers from the filesystem.
            let mut connection = self.lock_connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let prepared_bytes: i64 = transaction.query_row(
                "SELECT COALESCE(SUM(byte_size), 0) FROM prepared_artifacts
                 WHERE finalized_event_id IS NULL",
                [],
                |row| row.get(0),
            )?;
            let total = from_i64(prepared_bytes)?
                .checked_add(byte_size)
                .ok_or(StoreError::ArtifactQuotaExceeded)?;
            if total > MAX_PREPARED_BYTES {
                return Err(StoreError::ArtifactQuotaExceeded);
            }
            let generation: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(staging_generation), 0) + 1
                 FROM prepared_artifacts WHERE artifact_id = ?1",
                [artifact_id],
                |row| row.get(0),
            )?;
            let generation = from_i64(generation)?;

            let object_path = self.object_path(&content_hash);
            if object_path.exists() {
                verify_object(&object_path, &content_hash, byte_size)?;
                fs::remove_file(&temporary)?;
            } else {
                let parent = object_path.parent().ok_or(StoreError::InvalidStorePath)?;
                let parent_was_missing = !parent.exists();
                fs::create_dir_all(parent)?;
                if parent_was_missing {
                    sync_directory(&self.root.join("objects"))?;
                }
                fs::rename(&temporary, &object_path)?;
                sync_directory(parent)?;
            }

            let token_id = format!("prepared_{}_{}", std::process::id(), now_nanos()?);
            transaction.execute(
                "INSERT INTO prepared_artifacts(
                   token_id, artifact_id, content_hash, byte_size, media_type, logical_name,
                   staging_generation, prepared_at_epoch_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    token_id,
                    artifact_id,
                    content_hash,
                    to_i64(byte_size)?,
                    media_type,
                    logical_name,
                    to_i64(generation)?,
                    to_i64(now_epoch_ms()?)?,
                ],
            )?;
            transaction.commit()?;
            Ok(ArtifactToken {
                token_id,
                artifact_id: artifact_id.to_owned(),
                content_hash,
                byte_size,
                staging_generation: generation,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// Returns finalized metadata. An uncommitted token cannot be finalized by
    /// this API because doing so would create an independent canonical writer.
    pub fn finalize(
        &self,
        token: &ArtifactToken,
        origin_event_id: &str,
    ) -> Result<ArtifactMetadata, StoreError> {
        validate_identifier(origin_event_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let prepared =
            load_prepared(&connection, &token.token_id)?.ok_or(StoreError::UnknownArtifactToken)?;
        if prepared.artifact_id != token.artifact_id
            || prepared.content_hash != token.content_hash
            || prepared.byte_size != token.byte_size
            || prepared.staging_generation != token.staging_generation
        {
            return Err(StoreError::ArtifactTokenMismatch);
        }
        match prepared.finalized_event_id.as_deref() {
            None => Err(StoreError::ArtifactFinalizationRequiresCommit),
            Some(existing) if existing != origin_event_id => {
                Err(StoreError::ArtifactTokenAlreadyFinalized)
            }
            Some(_) => {
                load_metadata(&connection, &token.artifact_id)?.ok_or(StoreError::UnknownArtifact)
            }
        }
    }

    /// Reads immutable metadata including an explicit availability state.
    pub fn metadata(&self, artifact_id: &str) -> Result<ArtifactMetadata, StoreError> {
        validate_identifier(artifact_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        load_metadata(&connection, artifact_id)?.ok_or(StoreError::UnknownArtifact)
    }

    /// Lists every semantic event referencing one immutable artifact.
    pub fn references(&self, artifact_id: &str) -> Result<Vec<String>, StoreError> {
        validate_identifier(artifact_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT origin_event_id FROM artifact_references
             WHERE artifact_id = ?1 ORDER BY origin_event_id",
        )?;
        Ok(statement
            .query_map([artifact_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Reads a bounded byte range after verifying the complete immutable object.
    pub fn read_range(
        &self,
        artifact_id: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, StoreError> {
        validate_identifier(artifact_id)?;
        if length > MAX_RANGE_BYTES {
            return Err(StoreError::ArtifactRangeTooLarge);
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let metadata =
            load_metadata(&connection, artifact_id)?.ok_or(StoreError::UnknownArtifact)?;
        drop(connection);
        let object_path = self.object_path(&metadata.content_hash);
        if verify_object(&object_path, &metadata.content_hash, metadata.byte_size).is_err() {
            let connection = self.lock_connection()?;
            connection.execute(
                "UPDATE artifacts SET availability = 'corrupt' WHERE artifact_id = ?1",
                [artifact_id],
            )?;
            return Err(StoreError::CorruptArtifact);
        }
        let available = metadata.byte_size.saturating_sub(offset);
        let bounded =
            available.min(u64::try_from(length).map_err(|_| StoreError::InvalidStoredData)?);
        let bounded = usize::try_from(bounded).map_err(|_| StoreError::InvalidStoredData)?;
        let mut object = File::open(object_path)?;
        object.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; bounded];
        object.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Deletes expired unfinalized staging records and content objects that are
    /// not referenced by any finalized or still-prepared metadata row.
    pub fn collect_orphans(&self, older_than_epoch_ms: u64) -> Result<u64, StoreError> {
        let _lease = self.gate.shared()?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT token_id, content_hash FROM prepared_artifacts
             WHERE finalized_event_id IS NULL AND prepared_at_epoch_ms < ?1",
        )?;
        let expired = statement
            .query_map([to_i64(older_than_epoch_ms)?], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (token_id, _) in &expired {
            transaction.execute(
                "DELETE FROM prepared_artifacts WHERE token_id = ?1",
                [token_id],
            )?;
        }
        transaction.commit()?;

        let expired_hashes = expired
            .into_iter()
            .map(|(_, hash)| hash)
            .collect::<BTreeSet<_>>();
        // A second immediate transaction provides a stable liveness view while
        // unindexed crash remnants and stale temporary files are removed.
        let cleanup = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = collect_unreferenced_objects(
            &self.root,
            &cleanup,
            &expired_hashes,
            older_than_epoch_ms,
        )? + collect_stale_temporary_files(&self.root, older_than_epoch_ms)?;
        cleanup.commit()?;
        Ok(removed)
    }

    fn object_path(&self, content_hash: &str) -> PathBuf {
        self.root
            .join("objects")
            .join(&content_hash[..2])
            .join(content_hash)
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)
    }
}

fn collect_unreferenced_objects(
    root: &Path,
    transaction: &Transaction<'_>,
    expired_hashes: &BTreeSet<String>,
    older_than_epoch_ms: u64,
) -> Result<u64, StoreError> {
    let objects_root = root.join("objects");
    if !objects_root.exists() {
        return Ok(0);
    }
    let mut removed = 0_u64;
    for prefix_entry in fs::read_dir(&objects_root)? {
        let prefix_entry = prefix_entry?;
        let prefix_type = prefix_entry.file_type()?;
        if prefix_type.is_symlink() || !prefix_type.is_dir() {
            return Err(StoreError::CorruptArtifact);
        }
        let prefix = prefix_entry
            .file_name()
            .to_str()
            .ok_or(StoreError::CorruptArtifact)?
            .to_owned();
        for object_entry in fs::read_dir(prefix_entry.path())? {
            let object_entry = object_entry?;
            let object_type = object_entry.file_type()?;
            if object_type.is_symlink() || !object_type.is_file() {
                return Err(StoreError::CorruptArtifact);
            }
            let hash = object_entry
                .file_name()
                .to_str()
                .ok_or(StoreError::CorruptArtifact)?
                .to_owned();
            if !valid_content_hash(&hash) || prefix != hash[..2] {
                return Err(StoreError::CorruptArtifact);
            }
            let still_used: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM prepared_artifacts WHERE content_hash = ?1)
                        OR EXISTS(SELECT 1 FROM artifacts WHERE content_hash = ?1)",
                [hash.as_str()],
                |row| row.get(0),
            )?;
            let old_unindexed = modified_epoch_ms(&object_entry.metadata()?)? < older_than_epoch_ms;
            if still_used == 0 && (expired_hashes.contains(&hash) || old_unindexed) {
                fs::remove_file(object_entry.path())?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn collect_stale_temporary_files(root: &Path, older_than_epoch_ms: u64) -> Result<u64, StoreError> {
    let staging = root.join(".artifact-staging");
    if !staging.exists() {
        return Ok(0);
    }
    let mut removed = 0_u64;
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::CorruptArtifact);
        }
        if modified_epoch_ms(&entry.metadata()?)? < older_than_epoch_ms {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn modified_epoch_ms(metadata: &fs::Metadata) -> Result<u64, StoreError> {
    let elapsed = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidStoredData)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| StoreError::InvalidStoredData)
}

fn valid_content_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

struct PreparedRow {
    artifact_id: String,
    content_hash: String,
    byte_size: u64,
    staging_generation: u64,
    finalized_event_id: Option<String>,
}

fn load_prepared(
    connection: &Connection,
    token_id: &str,
) -> Result<Option<PreparedRow>, StoreError> {
    connection
        .query_row(
            "SELECT artifact_id, content_hash, byte_size, staging_generation,
                    finalized_event_id
             FROM prepared_artifacts WHERE token_id = ?1",
            [token_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .map(
            |(artifact_id, content_hash, byte_size, generation, finalized_event_id)| {
                Ok(PreparedRow {
                    artifact_id,
                    content_hash,
                    byte_size: from_i64(byte_size)?,
                    staging_generation: from_i64(generation)?,
                    finalized_event_id,
                })
            },
        )
        .transpose()
}

fn load_metadata(
    connection: &Connection,
    artifact_id: &str,
) -> Result<Option<ArtifactMetadata>, StoreError> {
    connection
        .query_row(
            "SELECT a.artifact_id, a.content_hash, a.byte_size, a.media_type,
                    a.logical_name, a.availability, a.retention_class,
                    (SELECT MIN(origin_event_id) FROM artifact_references
                     WHERE artifact_id = a.artifact_id)
             FROM artifacts a WHERE a.artifact_id = ?1",
            [artifact_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?
        .map(
            |(
                artifact_id,
                content_hash,
                size,
                media_type,
                logical_name,
                availability,
                retention_class,
                origin_event_id,
            )| {
                Ok(ArtifactMetadata {
                    artifact_id,
                    content_hash,
                    byte_size: from_i64(size)?,
                    media_type,
                    logical_name,
                    origin_event_id,
                    availability,
                    retention_class,
                })
            },
        )
        .transpose()
}

fn stream_temporary(path: &Path, reader: &mut impl Read) -> Result<(String, u64), StoreError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| StoreError::InvalidStoredData)?)
            .ok_or(StoreError::ArtifactTooLarge)?;
        if total > MAX_ARTIFACT_BYTES {
            return Err(StoreError::ArtifactTooLarge);
        }
        digest.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
    }
    file.sync_all()?;
    Ok((format!("{:x}", digest.finalize()), total))
}

fn verify_object(path: &Path, expected_hash: &str, expected_size: u64) -> Result<(), StoreError> {
    let mut file = File::open(path).map_err(|_| StoreError::CorruptArtifact)?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| StoreError::CorruptArtifact)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| StoreError::InvalidStoredData)?)
            .ok_or(StoreError::CorruptArtifact)?;
        digest.update(&buffer[..read]);
    }
    if total != expected_size || format!("{:x}", digest.finalize()) != expected_hash {
        return Err(StoreError::CorruptArtifact);
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), StoreError> {
    stable(value).map(|_| ())
}

fn validate_metadata(value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        Err(StoreError::InvalidText)
    } else {
        Ok(())
    }
}

fn stable(value: &str) -> Result<StableId, StoreError> {
    StableId::parse(value.to_owned()).map_err(|_| StoreError::InvalidId)
}

fn absolute(path: &Path) -> Result<PathBuf, StoreError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn now_nanos() -> Result<u128, StoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidStoredData)?
        .as_nanos())
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

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidStoredData)
}

fn from_i64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStoredData)
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

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use aworkit_protocol::{CommitBatchV1, DedupV1, EventV1, HistoryBackendV1};
    use serde_json::json;

    use super::*;
    use crate::{LocalHistoryStore, StoreError};

    fn root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aworkit-artifacts-{nonce}"))
    }

    fn id(value: &str) -> StableId {
        StableId::parse(value).expect("stable ID")
    }

    #[test]
    fn artifact_reference_and_event_finalize_in_one_transaction() {
        let root = root();
        let artifacts = ArtifactStore::open(&root).expect("artifacts");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        let token = artifacts
            .prepare("artifact_01", "text/plain", "result.txt", b"evidence")
            .expect("prepare");
        assert!(matches!(
            artifacts.finalize(&token, "event_01"),
            Err(StoreError::ArtifactFinalizationRequiresCommit)
        ));
        history
            .commit_v1(&CommitBatchV1 {
                backend: HistoryBackendV1::LocalSqlite,
                chat_id: id("chat_01"),
                run_id: id("run_01"),
                branch_id: id("main"),
                expected_head: 0,
                expected_aggregate_version: 0,
                events: vec![EventV1 {
                    event_id: id("event_01"),
                    schema_version: 1,
                    kind: "evidence.created".into(),
                    payload: json!({"schemaVersion": 1}),
                }],
                attempts: vec![],
                checkpoint: None,
                deduplication: Some(DedupV1 {
                    key_type: "command".into(),
                    key: id("command_01"),
                }),
                outbox: vec![],
                prepared_artifacts: vec![token.reference_for("event_01").expect("reference")],
            })
            .expect("atomic commit");
        let metadata = artifacts.finalize(&token, "event_01").expect("finalized");
        assert_eq!(metadata.origin_event_id.as_deref(), Some("event_01"));
        assert_eq!(
            artifacts.read_range("artifact_01", 2, 4).expect("range"),
            b"iden"
        );
        assert!(matches!(
            artifacts.read_range("artifact_01", 0, MAX_RANGE_BYTES + 1),
            Err(StoreError::ArtifactRangeTooLarge)
        ));
        fs::write(artifacts.object_path(&token.content_hash), b"corrupt").expect("tamper");
        assert!(matches!(
            artifacts.read_range("artifact_01", 0, 1),
            Err(StoreError::CorruptArtifact)
        ));
        assert_eq!(
            artifacts
                .metadata("artifact_01")
                .expect("corrupt metadata")
                .availability,
            "corrupt"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn invalid_token_rolls_back_the_semantic_event() {
        let root = root();
        let artifacts = ArtifactStore::open(&root).expect("artifacts");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        let mut token = artifacts
            .prepare("artifact_01", "text/plain", "result.txt", b"evidence")
            .expect("prepare");
        token.content_hash = "0".repeat(64);
        let result = history.commit_v1(&CommitBatchV1 {
            backend: HistoryBackendV1::LocalSqlite,
            chat_id: id("chat_01"),
            run_id: id("run_01"),
            branch_id: id("main"),
            expected_head: 0,
            expected_aggregate_version: 0,
            events: vec![EventV1 {
                event_id: id("event_01"),
                schema_version: 1,
                kind: "evidence.created".into(),
                payload: json!({"schemaVersion": 1}),
            }],
            attempts: vec![],
            checkpoint: None,
            deduplication: None,
            outbox: vec![],
            prepared_artifacts: vec![token.reference_for("event_01").expect("reference")],
        });
        assert!(matches!(result, Err(StoreError::ArtifactTokenMismatch)));
        assert!(
            history
                .event_ids("chat_01", "main")
                .expect("events")
                .is_empty()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn bounded_reader_rejects_oversize_input_without_a_token() {
        let root = root();
        let store = ArtifactStore::open(&root).expect("artifacts");
        let bytes = vec![0_u8; usize::try_from(MAX_ARTIFACT_BYTES + 1).expect("test size")];
        assert!(matches!(
            store.prepare(
                "artifact_01",
                "application/octet-stream",
                "large.bin",
                &bytes
            ),
            Err(StoreError::ArtifactTooLarge)
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn orphan_collection_removes_expired_staging_but_preserves_shared_finalized_bytes() {
        let root = root();
        let artifacts = ArtifactStore::open(&root).expect("artifacts");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        let finalized = artifacts
            .prepare(
                "artifact_final",
                "text/plain",
                "final.txt",
                b"shared evidence",
            )
            .expect("finalized prepare");
        let orphan = artifacts
            .prepare(
                "artifact_orphan",
                "text/plain",
                "orphan.txt",
                b"shared evidence",
            )
            .expect("orphan prepare");
        let unique_orphan = artifacts
            .prepare(
                "artifact_unique",
                "text/plain",
                "unique.txt",
                b"unique orphan",
            )
            .expect("unique orphan prepare");
        history
            .commit_v1(&CommitBatchV1 {
                backend: HistoryBackendV1::LocalSqlite,
                chat_id: id("chat_01"),
                run_id: id("run_01"),
                branch_id: id("main"),
                expected_head: 0,
                expected_aggregate_version: 0,
                events: vec![EventV1 {
                    event_id: id("event_01"),
                    schema_version: 1,
                    kind: "evidence.created".into(),
                    payload: json!({"schemaVersion": 1}),
                }],
                attempts: vec![],
                checkpoint: None,
                deduplication: None,
                outbox: vec![],
                prepared_artifacts: vec![finalized.reference_for("event_01").expect("reference")],
            })
            .expect("commit");
        assert_eq!(
            artifacts
                .collect_orphans(now_epoch_ms().expect("time") + 1)
                .expect("gc"),
            1
        );
        assert!(matches!(
            artifacts.finalize(&orphan, "event_02"),
            Err(StoreError::UnknownArtifactToken)
        ));
        assert!(matches!(
            artifacts.finalize(&unique_orphan, "event_03"),
            Err(StoreError::UnknownArtifactToken)
        ));
        assert_eq!(
            artifacts
                .read_range("artifact_final", 0, 64)
                .expect("shared finalized bytes"),
            b"shared evidence"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn orphan_collection_discovers_unindexed_crash_remnants() {
        let root = root();
        let artifacts = ArtifactStore::open(&root).expect("artifacts");
        let bytes = b"promoted-before-metadata";
        let hash = format!("{:x}", Sha256::digest(bytes));
        let object = artifacts.object_path(&hash);
        fs::create_dir_all(object.parent().expect("object parent")).expect("object directory");
        fs::write(&object, bytes).expect("unindexed object");
        let temporary = root.join(".artifact-staging/crashed.tmp");
        fs::write(&temporary, b"partial").expect("temporary");

        assert_eq!(
            artifacts
                .collect_orphans(now_epoch_ms().expect("time") + 1_000)
                .expect("gc"),
            2
        );
        assert!(!object.exists());
        assert!(!temporary.exists());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
