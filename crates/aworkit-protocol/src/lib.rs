//! Stable, provider-neutral protocol primitives for Aworkit process boundaries.
//!
//! Domain DTO families belong to their respective core, worker, host, UI,
//! portable, or bootstrap module. This crate owns only envelopes, stable IDs,
//! versioning, framing, and the deliberately small base payloads.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

/// Largest permitted encoded JSON message body (one MiB).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
const FRAME_PREFIX_BYTES: usize = 4;

/// The current wire-protocol version. A new incompatible shape requires a new tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaVersion(pub u16);

impl SchemaVersion {
    /// The only protocol schema version supported by this scaffold.
    pub const V1: Self = Self(1);
}

/// A validated, opaque identifier shared between protocol headers.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StableId(String);

impl StableId {
    /// Creates an ID composed of ASCII letters, digits, `_`, `-`, or `.`.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
        if valid {
            Ok(Self(value))
        } else {
            Err(ProtocolError::InvalidStableId)
        }
    }

    /// Returns the stable wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for StableId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonically increasing process generation used to fence stale senders.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessGeneration(pub u64);

/// The disjoint envelope types allowed on an Aworkit boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    Command,
    Event,
    Request,
    Result,
    Error,
}

/// The common, versioned header for all boundary messages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope<T> {
    pub schema_version: SchemaVersion,
    pub message_id: StableId,
    pub generation: ProcessGeneration,
    pub kind: EnvelopeKind,
    pub payload: T,
}

impl<T> Envelope<T> {
    /// Creates a V1 boundary envelope after callers select its explicit kind.
    #[must_use]
    pub fn v1(
        message_id: StableId,
        generation: ProcessGeneration,
        kind: EnvelopeKind,
        payload: T,
    ) -> Self {
        Self {
            schema_version: SchemaVersion::V1,
            message_id,
            generation,
            kind,
            payload,
        }
    }
}

/// Base command metadata; command-specific data remains in the owning DTO family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseCommand {
    pub name: String,
    pub target_id: StableId,
}

/// Base event metadata; event-specific data remains in the owning DTO family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseEvent {
    pub name: String,
    pub sequence: u64,
}

/// Base request metadata used for a bounded request/result correlation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseRequest {
    pub operation: String,
    pub correlation_id: StableId,
}

/// Base result metadata used for a bounded request/result correlation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseResult {
    pub correlation_id: StableId,
    pub accepted: bool,
}

/// Normalized protocol error body, never an implementation-native error object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Errors that occur before an owning domain can process an incoming message.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("stable IDs must be 1-128 ASCII letters, digits, '.', '_' or '-'")]
    InvalidStableId,
    #[error("frame body exceeds the {MAX_FRAME_BYTES}-byte limit")]
    FrameTooLarge,
    #[error("frame is truncated")]
    TruncatedFrame,
    #[error("frame contains trailing bytes")]
    TrailingBytes,
    #[error("unsupported schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("invalid JSON message: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Encodes a serializable boundary message with a four-byte big-endian length prefix.
pub fn encode_frame<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtocolError> {
    let body = serde_json::to_vec(message)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let length = u32::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + body.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Decodes exactly one complete bounded frame and rejects extra bytes.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, ProtocolError> {
    if frame.len() < FRAME_PREFIX_BYTES {
        return Err(ProtocolError::TruncatedFrame);
    }
    let body_length = u32::from_be_bytes(
        frame[..FRAME_PREFIX_BYTES]
            .try_into()
            .expect("prefix has four bytes"),
    );
    let body_length = usize::try_from(body_length).expect("u32 fits usize on supported platforms");
    if body_length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let expected_length = FRAME_PREFIX_BYTES + body_length;
    if frame.len() < expected_length {
        return Err(ProtocolError::TruncatedFrame);
    }
    if frame.len() > expected_length {
        return Err(ProtocolError::TrailingBytes);
    }
    serde_json::from_slice(&frame[FRAME_PREFIX_BYTES..]).map_err(ProtocolError::InvalidJson)
}

/// Incremental bounded decoder for stream-oriented local IPC.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// Adds a byte chunk and returns every whole frame now available.
    pub fn push<T: DeserializeOwned>(&mut self, chunk: &[u8]) -> Result<Vec<T>, ProtocolError> {
        if self.buffer.len().saturating_add(chunk.len()) > MAX_FRAME_BYTES + FRAME_PREFIX_BYTES {
            return Err(ProtocolError::FrameTooLarge);
        }
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            if self.buffer.len() < FRAME_PREFIX_BYTES {
                break;
            }
            let body_length = u32::from_be_bytes(
                self.buffer[..FRAME_PREFIX_BYTES]
                    .try_into()
                    .expect("prefix has four bytes"),
            );
            let body_length =
                usize::try_from(body_length).expect("u32 fits usize on supported platforms");
            if body_length > MAX_FRAME_BYTES {
                return Err(ProtocolError::FrameTooLarge);
            }
            let frame_length = FRAME_PREFIX_BYTES + body_length;
            if self.buffer.len() < frame_length {
                break;
            }
            let frame: Vec<u8> = self.buffer.drain(..frame_length).collect();
            messages.push(decode_frame(&frame)?);
        }
        Ok(messages)
    }
}

/// Validates an envelope header before dispatching it to an owning domain DTO.
pub fn validate_envelope<T>(envelope: &Envelope<T>) -> Result<(), ProtocolError> {
    if envelope.schema_version != SchemaVersion::V1 {
        return Err(ProtocolError::UnsupportedSchemaVersion(
            envelope.schema_version.0,
        ));
    }
    Ok(())
}

/// Canonical JSON Schema for cross-language runtime adapters and fixture tooling.
#[must_use]
pub fn envelope_schema_v1() -> &'static str {
    include_str!("../../../protocol/schema/aworkit-envelope.v1.schema.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> Envelope<BaseCommand> {
        Envelope::v1(
            StableId::parse("msg_01").expect("stable ID"),
            ProcessGeneration(7),
            EnvelopeKind::Command,
            BaseCommand {
                name: "smoke.handshake".to_owned(),
                target_id: StableId::parse("trusted-core").expect("stable ID"),
            },
        )
    }
    #[test]
    fn frame_round_trip_is_lossless() {
        let framed = encode_frame(&command()).expect("frame");
        let decoded: Envelope<BaseCommand> = decode_frame(&framed).expect("decode");
        assert_eq!(decoded, command());
    }
    #[test]
    fn decoder_handles_split_frames() {
        let frame = encode_frame(&command()).expect("frame");
        let mut decoder = FrameDecoder::default();
        assert!(
            decoder
                .push::<Envelope<BaseCommand>>(&frame[..3])
                .expect("partial")
                .is_empty()
        );
        assert_eq!(
            decoder
                .push::<Envelope<BaseCommand>>(&frame[3..])
                .expect("whole"),
            vec![command()]
        );
    }
    #[test]
    fn rejects_oversized_declared_frame() {
        let frame = (u32::try_from(MAX_FRAME_BYTES + 1).expect("fits")).to_be_bytes();
        assert!(matches!(
            decode_frame::<Envelope<BaseCommand>>(&frame),
            Err(ProtocolError::FrameTooLarge)
        ));
    }

    #[test]
    fn rejects_unknown_schema_version_before_dispatch() {
        let mut unsupported = command();
        unsupported.schema_version = SchemaVersion(2);
        assert!(matches!(
            validate_envelope(&unsupported),
            Err(ProtocolError::UnsupportedSchemaVersion(2))
        ));
    }
}
