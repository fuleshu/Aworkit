//! Durable content-addressed local evidence objects and prepared-reference tokens.

use std::{
    fs,
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{StoreError, filesystem::write_and_sync_atomic};

/// A prepared artifact identity that may be finalized once for an event reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactToken {
    /// Opaque prepared-record identity; callers must not synthesize it.
    pub token_id: String,
    /// SHA-256 of the immutable object bytes.
    pub content_hash: String,
    /// Verified byte length of the immutable object.
    pub byte_size: u64,
}

/// Metadata for a durably prepared or event-referenced artifact object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactMetadata {
    /// Stable artifact identity, separate from the deduplicated content hash.
    pub artifact_id: String,
    /// Content-address identity.
    pub content_hash: String,
    /// The verified immutable byte count.
    pub byte_size: u64,
    /// An advisory media type for read-only presentation.
    pub media_type: String,
    /// A user-visible logical name, never a filesystem path.
    pub logical_name: String,
    /// The semantic event that finalized this evidence reference, if any.
    pub origin_event_id: Option<String>,
}

/// Local filesystem evidence store with a durable metadata journal.
#[derive(Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    connection: Arc<Mutex<Connection>>,
}

impl ArtifactStore {
    /// Opens the content root and metadata journal for local evidence.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(root.join("objects"))?;
        let connection = Connection::open(root.join("artifacts.sqlite"))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS prepared_artifacts (
               token_id TEXT PRIMARY KEY, artifact_id TEXT NOT NULL, content_hash TEXT NOT NULL,
               byte_size INTEGER NOT NULL, media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
               finalized_event_id TEXT UNIQUE
             ) STRICT;
             CREATE TABLE IF NOT EXISTS artifacts (
               artifact_id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, byte_size INTEGER NOT NULL,
               media_type TEXT NOT NULL, logical_name TEXT NOT NULL, origin_event_id TEXT UNIQUE
             ) STRICT;",
        )?;
        Ok(Self {
            root,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Promotes immutable bytes before any semantic event may reference them.
    pub fn prepare(
        &self,
        artifact_id: &str,
        media_type: &str,
        logical_name: &str,
        bytes: &[u8],
    ) -> Result<ArtifactToken, StoreError> {
        validate_artifact_identifier(artifact_id)?;
        validate_artifact_metadata(media_type)?;
        validate_artifact_metadata(logical_name)?;
        let byte_size = u64::try_from(bytes.len()).map_err(|_| StoreError::InvalidStoredData)?;
        let content_hash = content_hash(bytes);
        let object_path = self.object_path(&content_hash);
        if !object_path.exists() {
            write_and_sync_atomic(&object_path, bytes)?;
        }
        let token_id = format!("prepared-{}-{}", std::process::id(), now_nanos()?);
        let connection = self.lock_connection()?;
        connection.execute(
            "INSERT INTO prepared_artifacts(token_id, artifact_id, content_hash, byte_size, media_type, logical_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![token_id, artifact_id, content_hash, i64::try_from(byte_size).map_err(|_| StoreError::InvalidStoredData)?, media_type, logical_name],
        )?;
        Ok(ArtifactToken {
            token_id,
            content_hash,
            byte_size,
        })
    }

    /// Finalizes a verified prepared object for exactly one committed semantic event.
    pub fn finalize(
        &self,
        token: &ArtifactToken,
        origin_event_id: &str,
    ) -> Result<ArtifactMetadata, StoreError> {
        validate_artifact_identifier(&token.token_id)?;
        validate_artifact_identifier(origin_event_id)?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let metadata = transaction.query_row(
            "SELECT artifact_id, content_hash, byte_size, media_type, logical_name, finalized_event_id
             FROM prepared_artifacts WHERE token_id = ?1", [token.token_id.as_str()],
            |row| Ok(ArtifactMetadata {
                artifact_id: row.get(0)?, content_hash: row.get(1)?,
                byte_size: u64::try_from(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?,
                media_type: row.get(3)?, logical_name: row.get(4)?, origin_event_id: row.get(5)?,
            }),
        ).optional()?.ok_or(StoreError::UnknownArtifactToken)?;
        if let Some(existing_event_id) = &metadata.origin_event_id {
            if existing_event_id == origin_event_id {
                transaction.commit()?;
                return Ok(metadata);
            }
            return Err(StoreError::ArtifactTokenAlreadyFinalized);
        }
        self.verify_object(&metadata.content_hash, metadata.byte_size)?;
        transaction.execute(
            "UPDATE prepared_artifacts SET finalized_event_id = ?2 WHERE token_id = ?1",
            params![token.token_id, origin_event_id],
        )?;
        transaction.execute(
            "INSERT INTO artifacts(artifact_id, content_hash, byte_size, media_type, logical_name, origin_event_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![metadata.artifact_id, metadata.content_hash, i64::try_from(metadata.byte_size).map_err(|_| StoreError::InvalidStoredData)?, metadata.media_type, metadata.logical_name, origin_event_id],
        )?;
        transaction.commit()?;
        Ok(ArtifactMetadata {
            origin_event_id: Some(origin_event_id.to_owned()),
            ..metadata
        })
    }

    /// Reads a bounded byte range only after verifying the object's immutable hash.
    pub fn read_range(
        &self,
        artifact_id: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, StoreError> {
        validate_artifact_identifier(artifact_id)?;
        let connection = self.lock_connection()?;
        let (content_hash, byte_size): (String, i64) = connection
            .query_row(
                "SELECT content_hash, byte_size FROM artifacts WHERE artifact_id = ?1",
                [artifact_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::UnknownArtifact)?;
        let byte_size = u64::try_from(byte_size).map_err(|_| StoreError::InvalidStoredData)?;
        drop(connection);
        self.verify_object(&content_hash, byte_size)?;
        let available = byte_size.saturating_sub(offset);
        let length = usize::try_from(available.min(u64::try_from(length).unwrap_or(u64::MAX)))
            .map_err(|_| StoreError::InvalidStoredData)?;
        let mut object = fs::File::open(self.object_path(&content_hash))?;
        use std::io::Seek;
        object.seek(std::io::SeekFrom::Start(offset))?;
        let mut bytes = vec![0; length];
        object.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn object_path(&self, content_hash: &str) -> PathBuf {
        self.root
            .join("objects")
            .join(&content_hash[..2])
            .join(content_hash)
    }

    fn verify_object(&self, expected_hash: &str, expected_size: u64) -> Result<(), StoreError> {
        let bytes = fs::read(self.object_path(expected_hash))?;
        if u64::try_from(bytes.len()).map_err(|_| StoreError::InvalidStoredData)? != expected_size
            || content_hash(&bytes) != expected_hash
        {
            return Err(StoreError::CorruptArtifact);
        }
        Ok(())
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)
    }
}

fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_nanos() -> Result<u128, StoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidStoredData)?
        .as_nanos())
}

fn validate_artifact_identifier(value: &str) -> Result<(), StoreError> {
    let valid = !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidText)
    }
}

fn validate_artifact_metadata(value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        Err(StoreError::InvalidText)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn prepared_content_is_verified_before_single_event_finalization() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("aworkit-artifacts-{nonce}"));
        let store = ArtifactStore::open(&root).expect("store");
        let token = store
            .prepare("artifact_01", "text/plain", "result.txt", b"evidence")
            .expect("prepare");
        let metadata = store.finalize(&token, "event_01").expect("finalize");
        assert_eq!(metadata.origin_event_id.as_deref(), Some("event_01"));
        assert_eq!(
            store.read_range("artifact_01", 2, 4).expect("range"),
            b"iden"
        );
        assert!(matches!(
            store.finalize(&token, "event_02"),
            Err(StoreError::ArtifactTokenAlreadyFinalized)
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
