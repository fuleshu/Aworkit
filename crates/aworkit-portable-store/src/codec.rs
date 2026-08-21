//! Canonical JSONL encoding and SHA-256 identities for portable facts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// One sanitized semantic event in a portable branch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortableEvent {
    pub event_id: String,
    pub chat_id: String,
    pub branch_id: String,
    pub ordinal: u64,
    pub kind: String,
    pub payload: Value,
}

/// A bounded immutable segment of contiguous portable events.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortableSegment {
    pub parent_segment_hash: Option<String>,
    pub base_checkpoint_hash: Option<String>,
    pub first_ordinal: u64,
    pub events: Vec<PortableEvent>,
}

/// A logical reducer checkpoint that never contains native runtime state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortableCheckpoint {
    pub last_event_id: Option<String>,
    pub aggregate_version: u64,
    pub snapshot_hash: Option<String>,
    pub state_hash: String,
}

/// Deterministic portable record encoder.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalCodec;

impl CanonicalCodec {
    /// Serializes a portable record as canonical UTF-8 JSON followed by LF.
    pub fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, CodecError> {
        let value = serde_json::to_value(value)?;
        let bytes = canonical_json(&value)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(CodecError::RecordTooLarge);
        }
        let mut line = bytes;
        line.push(b'\n');
        Ok(line)
    }

    /// Decodes exactly one canonical JSONL record and validates its byte form.
    pub fn decode<T: for<'a> Deserialize<'a>>(&self, bytes: &[u8]) -> Result<T, CodecError> {
        if bytes.len() > MAX_RECORD_BYTES + 1 || !bytes.ends_with(b"\n") {
            return Err(CodecError::InvalidFraming);
        }
        let value: Value = serde_json::from_slice(&bytes[..bytes.len() - 1])?;
        let expected = self.encode(&value)?;
        if expected != bytes {
            return Err(CodecError::NonCanonical);
        }
        Ok(serde_json::from_value(value)?)
    }

    /// Validates contiguous ordinals before a segment can be published.
    pub fn encode_segment(&self, segment: &PortableSegment) -> Result<Vec<u8>, CodecError> {
        if segment.events.is_empty() {
            return Err(CodecError::EmptySegment);
        }
        if segment.events.len() > 64 {
            return Err(CodecError::TooManyEvents);
        }
        for (index, event) in segment.events.iter().enumerate() {
            if event.ordinal != segment.first_ordinal + u64::try_from(index).expect("bounded") {
                return Err(CodecError::OrdinalGap);
            }
        }
        self.encode(segment)
    }
}

/// Produces canonical JSON with ordered object keys and no non-finite values.
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, CodecError> {
    fn normalize(value: &Value) -> Result<Value, CodecError> {
        match value {
            Value::Array(values) => values
                .iter()
                .map(normalize)
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Object(values) => {
                let mut ordered = BTreeMap::new();
                for (key, item) in values {
                    ordered.insert(key.clone(), normalize(item)?);
                }
                Ok(Value::Object(ordered.into_iter().collect()))
            }
            Value::Number(number) if number.as_f64().is_some_and(|number| !number.is_finite()) => {
                Err(CodecError::AmbiguousNumber)
            }
            _ => Ok(value.clone()),
        }
    }
    Ok(serde_json::to_vec(&normalize(value)?)?)
}

/// Returns a domain-separated, explicit SHA-256 content identity.
#[must_use]
pub fn digest(domain: &str, bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    format!("sha256:{:x}", hash.finalize())
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("portable records must be LF-terminated canonical JSON")]
    InvalidFraming,
    #[error("portable record is not canonical")]
    NonCanonical,
    #[error("portable record exceeds its bounded size")]
    RecordTooLarge,
    #[error("portable segment must contain one to 64 events")]
    EmptySegment,
    #[error("portable segment exceeds 64 events")]
    TooManyEvents,
    #[error("portable event ordinals must be contiguous")]
    OrdinalGap,
    #[error("portable JSON contains an ambiguous number")]
    AmbiguousNumber,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
