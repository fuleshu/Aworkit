//! Shared schema, validation, hashing, and manifest row codecs.

use std::{fs, path::Path};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::redaction::RedactionError;

use super::model::{
    CaptureCorrelation, CaptureError, CaptureManifest, CapturePolicy, CaptureRequest,
    CaptureSource, CaptureState,
};

pub(super) const CAPTURE_SCHEMA_VERSION: i32 = 2;
pub(super) const MAX_PAGE_SIZE: u32 = 256;
pub(super) const MAX_READER_LEASE_MS: u64 = 5 * 60 * 1_000;
pub(super) const HARD_MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const HARD_MAX_CHUNK_BYTES: u64 = 1024 * 1024;
pub(super) const HARD_MAX_CHUNKS: u64 = 4_096;
pub(super) const HARD_MAX_GLOBAL_QUOTA_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CAPTURE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1_000;
const MAX_ID_BYTES: usize = 128;
const MAX_QUOTA_CLASS_BYTES: usize = 64;

#[derive(Debug)]
pub(super) struct RecordingLimits {
    pub policy_generation: u64,
    pub redaction_set_id: String,
    pub max_capture_bytes: u64,
    pub max_chunk_bytes: u64,
    pub max_chunks: u64,
    pub global_quota_bytes: u64,
    pub chunk_count: u64,
    pub byte_count: u64,
}

#[derive(Debug)]
pub(super) struct ChunkRow {
    pub ordinal: u64,
    pub received_at_epoch_ms: u64,
    pub byte_count: u64,
    pub stored_byte_count: u64,
    pub content_hash: String,
}

pub(super) fn ensure_schema(connection: &Connection) -> Result<(), CaptureError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS capture_manifests (
             capture_id TEXT PRIMARY KEY,
             request_hash TEXT NOT NULL,
             source TEXT NOT NULL,
             chat_id TEXT,
             event_id TEXT,
             invocation_id TEXT,
             attempt_id TEXT,
             policy_generation INTEGER NOT NULL CHECK(policy_generation >= 0),
             redaction_set_id TEXT NOT NULL,
             quota_class TEXT NOT NULL,
             created_at_epoch_ms INTEGER NOT NULL CHECK(created_at_epoch_ms >= 0),
             sealed_at_epoch_ms INTEGER,
             expires_at_epoch_ms INTEGER NOT NULL CHECK(expires_at_epoch_ms >= 0),
             expired_at_epoch_ms INTEGER,
             purge_after_epoch_ms INTEGER,
             max_capture_bytes INTEGER NOT NULL CHECK(max_capture_bytes > 0),
             max_chunk_bytes INTEGER NOT NULL CHECK(max_chunk_bytes > 0),
             max_chunks INTEGER NOT NULL CHECK(max_chunks > 0),
             global_quota_bytes INTEGER NOT NULL CHECK(global_quota_bytes > 0),
             expired_tombstone_ms INTEGER NOT NULL CHECK(expired_tombstone_ms >= 0),
             state TEXT NOT NULL CHECK(state IN (
                 'recording', 'available', 'corrupt_discarded', 'expired', 'purged'
             )),
             chunk_count INTEGER NOT NULL DEFAULT 0 CHECK(chunk_count >= 0),
             byte_count INTEGER NOT NULL DEFAULT 0 CHECK(byte_count >= 0),
             stored_byte_count INTEGER NOT NULL DEFAULT 0 CHECK(stored_byte_count >= 0),
             content_hash TEXT,
             truncated INTEGER NOT NULL DEFAULT 0 CHECK(truncated IN (0, 1)),
             truncation_reason TEXT,
             redaction_count INTEGER NOT NULL DEFAULT 0 CHECK(redaction_count >= 0),
             redaction_omissions INTEGER NOT NULL DEFAULT 0 CHECK(redaction_omissions >= 0),
             CHECK((state = 'recording' AND sealed_at_epoch_ms IS NULL AND content_hash IS NULL)
                OR state != 'recording')
         ) STRICT;
         CREATE TABLE IF NOT EXISTS capture_chunks (
             capture_id TEXT NOT NULL,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             received_at_epoch_ms INTEGER NOT NULL CHECK(received_at_epoch_ms >= 0),
             byte_count INTEGER NOT NULL CHECK(byte_count >= 0),
             stored_byte_count INTEGER NOT NULL CHECK(stored_byte_count >= 0),
             content_hash TEXT NOT NULL,
             PRIMARY KEY(capture_id, ordinal),
             FOREIGN KEY(capture_id) REFERENCES capture_manifests(capture_id) ON DELETE CASCADE
         ) STRICT;
         CREATE TABLE IF NOT EXISTS capture_reader_leases (
             lease_id TEXT PRIMARY KEY,
             capture_id TEXT NOT NULL,
             expires_at_epoch_ms INTEGER NOT NULL CHECK(expires_at_epoch_ms >= 0),
             FOREIGN KEY(capture_id) REFERENCES capture_manifests(capture_id) ON DELETE CASCADE
         ) STRICT;
         CREATE INDEX IF NOT EXISTS capture_retention
             ON capture_manifests(state, expires_at_epoch_ms, sealed_at_epoch_ms);
         CREATE INDEX IF NOT EXISTS capture_lease_expiry
             ON capture_reader_leases(expires_at_epoch_ms);",
    )?;
    let has_redaction_set_id = {
        let mut statement = connection.prepare("PRAGMA table_info(capture_manifests)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "redaction_set_id")
    };
    if !has_redaction_set_id {
        connection.execute_batch(
            "ALTER TABLE capture_manifests
                 ADD COLUMN redaction_set_id TEXT NOT NULL DEFAULT 'legacy-unbound';",
        )?;
    }
    connection.execute_batch("PRAGMA user_version=2;")?;
    Ok(())
}

pub(super) fn load_manifest_with_hash(
    connection: &Connection,
    capture_id: &str,
) -> Result<Option<(String, CaptureManifest)>, CaptureError> {
    connection
        .query_row(
            "SELECT request_hash, capture_id, source, chat_id, event_id, invocation_id,
                    attempt_id, policy_generation, redaction_set_id, quota_class, created_at_epoch_ms,
                    sealed_at_epoch_ms, expires_at_epoch_ms, expired_at_epoch_ms,
                    purge_after_epoch_ms, state, chunk_count, byte_count, stored_byte_count,
                    content_hash, truncated, truncation_reason, redaction_count,
                    redaction_omissions
             FROM capture_manifests WHERE capture_id=?1",
            [capture_id],
            |row| Ok((row.get(0)?, read_manifest_row_offset(row, 1)?)),
        )
        .optional()
        .map_err(CaptureError::from)
}

pub(super) fn load_manifest(
    connection: &Connection,
    capture_id: &str,
) -> Result<Option<CaptureManifest>, CaptureError> {
    connection
        .query_row(
            "SELECT capture_id, source, chat_id, event_id, invocation_id, attempt_id,
                    policy_generation, redaction_set_id, quota_class, created_at_epoch_ms, sealed_at_epoch_ms,
                    expires_at_epoch_ms, expired_at_epoch_ms, purge_after_epoch_ms, state,
                    chunk_count, byte_count, stored_byte_count, content_hash, truncated,
                    truncation_reason, redaction_count, redaction_omissions
             FROM capture_manifests WHERE capture_id=?1",
            [capture_id],
            read_manifest_row,
        )
        .optional()
        .map_err(CaptureError::from)
}

pub(super) fn read_manifest_row(
    row: &rusqlite::Row<'_>,
) -> Result<CaptureManifest, rusqlite::Error> {
    read_manifest_row_offset(row, 0)
}

fn read_manifest_row_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> Result<CaptureManifest, rusqlite::Error> {
    let source: String = row.get(offset + 1)?;
    let state: String = row.get(offset + 14)?;
    Ok(CaptureManifest {
        capture_id: row.get(offset)?,
        source: CaptureSource::parse(&source).map_err(|_| rusqlite::Error::InvalidQuery)?,
        correlation: CaptureCorrelation {
            chat_id: row.get(offset + 2)?,
            event_id: row.get(offset + 3)?,
            invocation_id: row.get(offset + 4)?,
            attempt_id: row.get(offset + 5)?,
        },
        policy_generation: checked_u64(row.get(offset + 6)?, offset + 6)?,
        redaction_set_id: row.get(offset + 7)?,
        quota_class: row.get(offset + 8)?,
        created_at_epoch_ms: checked_u64(row.get(offset + 9)?, offset + 9)?,
        sealed_at_epoch_ms: checked_optional_u64(row.get(offset + 10)?, offset + 10)?,
        expires_at_epoch_ms: checked_u64(row.get(offset + 11)?, offset + 11)?,
        expired_at_epoch_ms: checked_optional_u64(row.get(offset + 12)?, offset + 12)?,
        purge_after_epoch_ms: checked_optional_u64(row.get(offset + 13)?, offset + 13)?,
        state: CaptureState::parse(&state).map_err(|_| rusqlite::Error::InvalidQuery)?,
        chunk_count: checked_u64(row.get(offset + 15)?, offset + 15)?,
        byte_count: checked_u64(row.get(offset + 16)?, offset + 16)?,
        stored_byte_count: checked_u64(row.get(offset + 17)?, offset + 17)?,
        content_hash: row.get(offset + 18)?,
        truncated: row.get::<_, i64>(offset + 19)? != 0,
        truncation_reason: row.get(offset + 20)?,
        redaction_count: checked_u64(row.get(offset + 21)?, offset + 21)?,
        redaction_omissions: checked_u64(row.get(offset + 22)?, offset + 22)?,
    })
}

pub(super) fn load_recording_limits(
    connection: &Connection,
    capture_id: &str,
) -> Result<RecordingLimits, CaptureError> {
    connection
        .query_row(
            "SELECT policy_generation, redaction_set_id, max_capture_bytes, max_chunk_bytes, max_chunks,
                    global_quota_bytes, chunk_count, byte_count, state
             FROM capture_manifests WHERE capture_id=?1",
            [capture_id],
            |row| {
                let state: String = row.get(8)?;
                if state != "recording" {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(RecordingLimits {
                    policy_generation: checked_u64(row.get(0)?, 0)?,
                    redaction_set_id: row.get(1)?,
                    max_capture_bytes: checked_u64(row.get(2)?, 2)?,
                    max_chunk_bytes: checked_u64(row.get(3)?, 3)?,
                    max_chunks: checked_u64(row.get(4)?, 4)?,
                    global_quota_bytes: checked_u64(row.get(5)?, 5)?,
                    chunk_count: checked_u64(row.get(6)?, 6)?,
                    byte_count: checked_u64(row.get(7)?, 7)?,
                })
            },
        )
        .optional()?
        .ok_or(CaptureError::UnknownOrSealedCapture)
}

pub(super) fn load_state(
    connection: &Connection,
    capture_id: &str,
) -> Result<CaptureState, CaptureError> {
    let state = connection
        .query_row(
            "SELECT state FROM capture_manifests WHERE capture_id=?1",
            [capture_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(CaptureError::UnknownCapture)?;
    CaptureState::parse(&state)
}

pub(super) fn seal_transaction(
    transaction: &Transaction<'_>,
    capture_id: &str,
    sealed_at_epoch_ms: u64,
    truncated: bool,
    reason: Option<&str>,
    redaction_omission: bool,
) -> Result<(), CaptureError> {
    let state = load_state(transaction, capture_id)?;
    if state == CaptureState::Available {
        return Ok(());
    }
    if state != CaptureState::Recording {
        return Err(CaptureError::Unavailable(state));
    }
    let hashes = {
        let mut statement = transaction.prepare(
            "SELECT content_hash FROM capture_chunks
             WHERE capture_id=?1 ORDER BY ordinal",
        )?;
        statement
            .query_map([capture_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let content_hash = aggregate_hash(hashes.iter().map(String::as_str));
    transaction.execute(
        "UPDATE capture_manifests
         SET state='available', sealed_at_epoch_ms=?2, content_hash=?3,
             truncated=MAX(truncated, ?4),
             truncation_reason=COALESCE(truncation_reason, ?5),
             redaction_omissions=redaction_omissions + ?6
         WHERE capture_id=?1 AND state='recording'",
        params![
            capture_id,
            to_i64(sealed_at_epoch_ms)?,
            content_hash,
            i64::from(truncated),
            reason,
            i64::from(redaction_omission),
        ],
    )?;
    Ok(())
}

pub(super) fn validate_request(request: &CaptureRequest) -> Result<(), CaptureError> {
    validate_id(&request.capture_id)?;
    for correlation in [
        request.correlation.chat_id.as_deref(),
        request.correlation.event_id.as_deref(),
        request.correlation.invocation_id.as_deref(),
        request.correlation.attempt_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_id(correlation)?;
    }
    Ok(())
}

pub(super) fn validate_policy(policy: &CapturePolicy) -> Result<(), CaptureError> {
    if policy.quota_class.is_empty()
        || policy.quota_class.len() > MAX_QUOTA_CLASS_BYTES
        || !policy
            .quota_class
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || policy.max_capture_bytes == 0
        || policy.max_capture_bytes > HARD_MAX_CAPTURE_BYTES
        || policy.max_chunk_bytes == 0
        || policy.max_chunk_bytes > HARD_MAX_CHUNK_BYTES
        || policy.max_chunk_bytes > policy.max_capture_bytes
        || policy.max_chunks == 0
        || policy.max_chunks > HARD_MAX_CHUNKS
        || policy.global_quota_bytes < policy.max_chunk_bytes
        || policy.global_quota_bytes > HARD_MAX_GLOBAL_QUOTA_BYTES
        || policy.ttl_ms == 0
        || policy.ttl_ms > MAX_CAPTURE_TTL_MS
        || policy.expired_tombstone_ms > MAX_CAPTURE_TTL_MS
    {
        return Err(CaptureError::InvalidPolicy);
    }
    Ok(())
}

pub(super) fn validate_id(value: &str) -> Result<(), CaptureError> {
    let starts_and_ends_alphanumeric = value
        .as_bytes()
        .first()
        .zip(value.as_bytes().last())
        .is_some_and(|(first, last)| first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric());
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !starts_and_ends_alphanumeric
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CaptureError::InvalidId);
    }
    Ok(())
}

pub(super) fn canonical_hash(value: &impl Serialize) -> Result<String, CaptureError> {
    let bytes = serde_jcs::to_vec(value)?;
    Ok(hash_bytes(&bytes))
}

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(super) fn aggregate_hash<'a>(hashes: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for hash in hashes {
        digest.update(hash.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}

pub(super) fn redaction_reason(error: &RedactionError) -> &'static str {
    match error {
        RedactionError::ForbiddenField(_) => "forbidden_secret_field",
        RedactionError::UnsupportedBinary => "unsupported_binary_payload",
        RedactionError::MalformedStructuredPayload => "malformed_structured_payload",
        RedactionError::NulByte => "nul_payload",
        RedactionError::ResidualSecret => "residual_secret",
        RedactionError::TooManySecretValues
        | RedactionError::TooManyForbiddenFields
        | RedactionError::InvalidSecretValue
        | RedactionError::InvalidFieldName
        | RedactionError::ReplacementOverflow
        | RedactionError::OutputTooLarge => "redaction_failure",
    }
}

pub(super) fn parse_chunk_name(name: &std::ffi::OsStr) -> Option<u64> {
    let text = name.to_str()?;
    text.strip_suffix(".rle")?.parse().ok()
}

pub(super) fn remove_directory_if_present(path: &Path) -> Result<(), CaptureError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn to_i64(value: u64) -> Result<i64, CaptureError> {
    i64::try_from(value).map_err(|_| CaptureError::NumericOverflow)
}

pub(super) fn from_i64(value: i64) -> Result<u64, CaptureError> {
    u64::try_from(value).map_err(|_| CaptureError::CorruptMetadata)
}

fn checked_u64(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

fn checked_optional_u64(value: Option<i64>, column: usize) -> Result<Option<u64>, rusqlite::Error> {
    value.map(|value| checked_u64(value, column)).transpose()
}
