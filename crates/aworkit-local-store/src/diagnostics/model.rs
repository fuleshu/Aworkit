//! Public structured contracts for bounded diagnostic logging.

use std::{collections::BTreeMap, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{RedactionError, bounded_codec::CodecError};

pub(super) const MAX_PAGE_SIZE: u32 = 512;
pub(super) const MAX_FIELD_VALUE_BYTES: usize = 256;
pub(super) const MAX_FIELDS: usize = 32;
const HARD_MAX_QUEUE_CAPACITY: usize = 65_536;
pub(super) const HARD_MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const HARD_MAX_SEGMENTS: usize = 64;
const HARD_MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1_000;
const HARD_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// Severity controls bounded-queue eviction only; it is not an audit level.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
}

impl DiagnosticSeverity {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Optional opaque correlations. They never establish canonical ordering.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCorrelation {
    pub chat_id: Option<String>,
    pub event_id: Option<String>,
    pub attempt_id: Option<String>,
    pub invocation_id: Option<String>,
}

/// Caller-owned input. It is synchronously sanitized before the bounded queue
/// can retain an owned copy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticInput {
    pub occurred_at_epoch_ms: u64,
    pub monotonic_offset_ns: u64,
    pub severity: DiagnosticSeverity,
    pub component: String,
    pub code: String,
    pub correlation: DiagnosticCorrelation,
    pub fields: BTreeMap<String, String>,
}

/// A sequence is monotonic only within its writer/process generation.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRecordId {
    pub writer_generation: String,
    pub sequence: u64,
}

impl fmt::Display for DiagnosticRecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.writer_generation, self.sequence)
    }
}

/// Persisted allowlisted, redacted diagnostic data.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRecord {
    pub record_id: DiagnosticRecordId,
    pub occurred_at_epoch_ms: u64,
    pub monotonic_offset_ns: u64,
    pub severity: DiagnosticSeverity,
    pub component: String,
    pub code: String,
    pub message: String,
    pub correlation: DiagnosticCorrelation,
    pub fields: BTreeMap<String, String>,
    pub redaction_count: u64,
    #[serde(default)]
    pub redaction_generation: u64,
    #[serde(default = "legacy_redaction_set_id")]
    pub redaction_set_id: String,
}

/// Rotation and queue policy. Defaults match the formal ten-by-20 MiB / seven
/// day design, while a hard total quota remains independently enforced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLogConfig {
    pub writer_generation: String,
    pub queue_capacity: usize,
    pub max_segment_bytes: u64,
    pub max_segments: usize,
    pub max_age_ms: u64,
    pub max_total_bytes: u64,
}

impl DiagnosticLogConfig {
    #[must_use]
    pub fn standard(writer_generation: impl Into<String>) -> Self {
        Self {
            writer_generation: writer_generation.into(),
            queue_capacity: 2_048,
            max_segment_bytes: 20 * 1024 * 1024,
            max_segments: 10,
            max_age_ms: 7 * 24 * 60 * 60 * 1_000,
            max_total_bytes: 200 * 1024 * 1024,
        }
    }
}

/// Closed generations remain as metadata tombstones after expiry/corruption.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSegmentState {
    Open,
    Available,
    Expired,
    Corrupt,
}

/// Query-safe metadata for one ordered writer-generation range.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSegmentMetadata {
    pub segment_id: u64,
    pub writer_generation: String,
    pub start_sequence: u64,
    pub end_sequence: Option<u64>,
    pub created_at_epoch_ms: u64,
    pub closed_at_epoch_ms: Option<u64>,
    pub raw_byte_count: u64,
    pub stored_byte_count: u64,
    pub content_hash: Option<String>,
    pub state: DiagnosticSegmentState,
    pub unavailable_reason: Option<String>,
    pub(super) file_name: String,
}

/// Cursor ordering is manifest segment order followed by writer-local sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticCursor {
    pub writer_generation: String,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticUnavailableRange {
    pub writer_generation: String,
    pub start_sequence: u64,
    pub end_sequence: Option<u64>,
    pub state: DiagnosticSegmentState,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticPage {
    pub records: Vec<DiagnosticRecord>,
    pub unavailable_ranges: Vec<DiagnosticUnavailableRange>,
    pub next_cursor: Option<DiagnosticCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticDropReason {
    QueueContended,
    QueueFull,
    Repetitive,
    RedactionRejected,
    InvalidRecord,
    StoreClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticWriteOutcome {
    Accepted,
    Dropped(DiagnosticDropReason),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticHealth {
    pub accepted: u64,
    pub dropped: u64,
    pub write_failures: u64,
    pub corrupt_segments: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticRetentionReport {
    pub expired_segments: u64,
    pub removed_bytes: u64,
}

#[derive(Debug, Error)]
pub enum DiagnosticError {
    #[error("diagnostic filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("diagnostic JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("diagnostic redaction failed: {0}")]
    Redaction(#[from] RedactionError),
    #[error("diagnostic compression failed: {0}")]
    Compression(String),
    #[error("diagnostic configuration is invalid")]
    InvalidConfig,
    #[error("diagnostic record contains invalid or non-allowlisted data")]
    InvalidRecord,
    #[error("diagnostic page request is invalid")]
    InvalidPage,
    #[error("diagnostic manifest is corrupt")]
    CorruptManifest,
    #[error("diagnostic segment is corrupt")]
    CorruptSegment,
    #[error("diagnostic writer is closed")]
    Closed,
    #[error("another diagnostic writer already owns this store root")]
    WriterActive,
    #[error("diagnostic worker stopped before acknowledging the request")]
    WorkerStopped,
    #[error("diagnostic worker thread panicked")]
    WorkerPanicked,
    #[error("diagnostic queue lock is unavailable after a previous panic")]
    Poisoned,
    #[error("diagnostic numeric value overflowed")]
    NumericOverflow,
    #[error("diagnostic cursor does not refer to retained manifest history")]
    UnknownCursor,
    #[error("diagnostic store path is invalid")]
    InvalidPath,
}

impl From<CodecError> for DiagnosticError {
    fn from(error: CodecError) -> Self {
        Self::Compression(error.to_string())
    }
}

pub(super) fn validate_config(config: &DiagnosticLogConfig) -> Result<(), DiagnosticError> {
    if !valid_id(&config.writer_generation)
        || config.queue_capacity == 0
        || config.queue_capacity > HARD_MAX_QUEUE_CAPACITY
        || config.max_segment_bytes < 4 * 1024
        || config.max_segment_bytes > HARD_MAX_SEGMENT_BYTES
        || config.max_segments == 0
        || config.max_segments > HARD_MAX_SEGMENTS
        || config.max_age_ms == 0
        || config.max_age_ms > HARD_MAX_AGE_MS
        || config.max_total_bytes < config.max_segment_bytes
        || config.max_total_bytes > HARD_MAX_TOTAL_BYTES
    {
        return Err(DiagnosticError::InvalidConfig);
    }
    Ok(())
}

pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(super) fn manifest_path(root: &std::path::Path) -> PathBuf {
    root.join("manifest.json")
}

fn legacy_redaction_set_id() -> String {
    "legacy-unbound".to_owned()
}
