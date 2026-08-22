//! Public detailed-capture contracts and lifecycle states.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{bounded_codec::CodecError, redaction::RedactionError};

/// Process-neutral origin of one optional detailed capture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    Provider,
    Mcp,
    Plugin,
    ExternalAgent,
    InternalStream,
}

impl CaptureSource {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Mcp => "mcp",
            Self::Plugin => "plugin",
            Self::ExternalAgent => "external_agent",
            Self::InternalStream => "internal_stream",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, CaptureError> {
        match value {
            "provider" => Ok(Self::Provider),
            "mcp" => Ok(Self::Mcp),
            "plugin" => Ok(Self::Plugin),
            "external_agent" => Ok(Self::ExternalAgent),
            "internal_stream" => Ok(Self::InternalStream),
            _ => Err(CaptureError::CorruptMetadata),
        }
    }
}

/// Durable availability of capture bytes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Recording,
    Available,
    CorruptDiscarded,
    Expired,
    Purged,
}

impl CaptureState {
    pub(super) fn parse(value: &str) -> Result<Self, CaptureError> {
        match value {
            "recording" => Ok(Self::Recording),
            "available" => Ok(Self::Available),
            "corrupt_discarded" => Ok(Self::CorruptDiscarded),
            "expired" => Ok(Self::Expired),
            "purged" => Ok(Self::Purged),
            _ => Err(CaptureError::CorruptMetadata),
        }
    }
}

/// Optional semantic correlations supplied by the trusted core.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureCorrelation {
    pub chat_id: Option<String>,
    pub event_id: Option<String>,
    pub invocation_id: Option<String>,
    pub attempt_id: Option<String>,
}

/// Hard limits selected before recording begins.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePolicy {
    pub enabled: bool,
    pub generation: u64,
    pub max_capture_bytes: u64,
    pub max_chunk_bytes: u64,
    pub max_chunks: u64,
    pub global_quota_bytes: u64,
    pub ttl_ms: u64,
    pub expired_tombstone_ms: u64,
    pub quota_class: String,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            generation: 0,
            max_capture_bytes: 32 * 1024 * 1024,
            max_chunk_bytes: 1024 * 1024,
            max_chunks: 4_096,
            global_quota_bytes: 256 * 1024 * 1024,
            ttl_ms: 24 * 60 * 60 * 1_000,
            expired_tombstone_ms: 24 * 60 * 60 * 1_000,
            quota_class: "debug".to_owned(),
        }
    }
}

/// Immutable facts required to begin one capture identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    pub capture_id: String,
    pub source: CaptureSource,
    pub correlation: CaptureCorrelation,
    pub created_at_epoch_ms: u64,
}

/// One complete receive-order frame. It is redacted synchronously before any
/// persistence work begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureFrame<'a> {
    pub capture_id: &'a str,
    pub received_at_epoch_ms: u64,
    pub payload: &'a [u8],
}

/// Persisted manifest metadata; it remains queryable after byte removal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureManifest {
    pub capture_id: String,
    pub source: CaptureSource,
    pub correlation: CaptureCorrelation,
    pub policy_generation: u64,
    pub redaction_set_id: String,
    pub quota_class: String,
    pub created_at_epoch_ms: u64,
    pub sealed_at_epoch_ms: Option<u64>,
    pub expires_at_epoch_ms: u64,
    pub expired_at_epoch_ms: Option<u64>,
    pub purge_after_epoch_ms: Option<u64>,
    pub state: CaptureState,
    pub chunk_count: u64,
    pub byte_count: u64,
    pub stored_byte_count: u64,
    pub content_hash: Option<String>,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
    pub redaction_count: u64,
    pub redaction_omissions: u64,
}

/// Metadata acknowledged after one compressed chunk becomes durable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureChunkMetadata {
    pub ordinal: u64,
    pub received_at_epoch_ms: u64,
    pub byte_count: u64,
    pub stored_byte_count: u64,
    pub content_hash: String,
    pub redaction_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CaptureAppendOutcome {
    Appended(CaptureChunkMetadata),
    /// The rejected/quota-exceeding frame was not stored and the manifest was
    /// sealed as truncated so callers can continue semantic processing.
    Truncated(CaptureManifest),
}

/// One verified, decompressed capture chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureChunk {
    pub ordinal: u64,
    pub received_at_epoch_ms: u64,
    pub payload: Vec<u8>,
    pub content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturePage {
    pub manifest: CaptureManifest,
    pub chunks: Vec<CaptureChunk>,
    pub next_ordinal: Option<u64>,
}

/// Forward-schema behavior for the dedicated, noncanonical database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStoreMode {
    ReadWrite,
    InspectableReadOnly { found_schema: u32 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionReport {
    pub expired: u64,
    pub purged: u64,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("capture filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("capture manifest database failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("capture JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("capture redaction failed: {0}")]
    Redaction(#[from] RedactionError),
    #[error("capture compression failed: {0}")]
    Compression(String),
    #[error("capture identifier is invalid")]
    InvalidId,
    #[error("capture policy is invalid")]
    InvalidPolicy,
    #[error("capture reader lease is invalid")]
    InvalidLease,
    #[error("capture page request is invalid")]
    InvalidPage,
    #[error("capture identity was reused with different immutable facts")]
    IdentityConflict,
    #[error("capture does not exist")]
    UnknownCapture,
    #[error("capture does not exist or is already sealed")]
    UnknownOrSealedCapture,
    #[error("capture bytes are unavailable in state {0:?}")]
    Unavailable(CaptureState),
    #[error("capture reader lease does not exist")]
    UnknownReaderLease,
    #[error("capture redaction generation differs from its manifest")]
    RedactionGenerationMismatch,
    #[error("capture redaction set identity differs from its manifest")]
    RedactionIdentityMismatch,
    #[error("capture chunk is missing or corrupt")]
    CorruptChunk,
    #[error("capture manifest contains corrupt data")]
    CorruptMetadata,
    #[error("capture numeric value overflowed")]
    NumericOverflow,
    #[error("capture lock is unavailable after a previous panic")]
    Poisoned,
    #[error("capture schema {found_schema} is newer and inspectable read-only")]
    ForwardSchema { found_schema: u32 },
}

impl From<CodecError> for CaptureError {
    fn from(error: CodecError) -> Self {
        Self::Compression(error.to_string())
    }
}
