//! Stable, provider-neutral protocol primitives for Aworkit process boundaries.
//!
//! Domain DTO families belong to their respective core, worker, host, UI,
//! portable, or bootstrap module. This crate owns only envelopes, stable IDs,
//! versioning, framing, and the deliberately small base payloads.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use thiserror::Error;

mod extension;
mod history;
mod runtime;

pub use extension::*;
pub use history::*;
pub use runtime::*;

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
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
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

impl<'de> Deserialize<'de> for StableId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl Display for StableId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonically increasing process generation used to fence stale senders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessGeneration(pub u64);

/// Largest integer that every supported JavaScript runtime represents exactly.
pub const MAX_SAFE_WIRE_INTEGER: u64 = 9_007_199_254_740_991;

impl Serialize for ProcessGeneration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.0 > MAX_SAFE_WIRE_INTEGER {
            return Err(serde::ser::Error::custom(
                "process generation exceeds the exact cross-language wire range",
            ));
        }
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for ProcessGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        if value > MAX_SAFE_WIRE_INTEGER {
            return Err(serde::de::Error::custom(
                "process generation exceeds the exact cross-language wire range",
            ));
        }
        Ok(Self(value))
    }
}

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

/// Associates a payload family with the only envelope kind that may carry it.
pub trait WirePayload {
    const KIND: EnvelopeKind;

    fn validate(&self) -> Result<(), ProtocolError> {
        Ok(())
    }
}

impl<T: WirePayload> Envelope<T> {
    /// Creates a V1 envelope whose kind is derived from the payload contract.
    #[must_use]
    pub fn typed(message_id: StableId, generation: ProcessGeneration, payload: T) -> Self {
        Self::v1(message_id, generation, T::KIND, payload)
    }

    /// Validates both the common header and its payload-kind contract.
    pub fn validate_typed(&self) -> Result<(), ProtocolError> {
        validate_envelope(self)?;
        if self.kind != T::KIND {
            return Err(ProtocolError::EnvelopeKindMismatch {
                expected: T::KIND,
                actual: self.kind,
            });
        }
        self.payload.validate()
    }
}

/// Base command metadata; command-specific data remains in the owning DTO family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseCommand {
    pub name: String,
    pub target_id: StableId,
}

impl WirePayload for BaseCommand {
    const KIND: EnvelopeKind = EnvelopeKind::Command;

    fn validate(&self) -> Result<(), ProtocolError> {
        validate_bounded_text(&self.name, 256)
    }
}

/// Base event metadata; event-specific data remains in the owning DTO family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseEvent {
    pub name: String,
    pub sequence: u64,
}

impl Serialize for BaseEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_safe_wire_integer(self.sequence).map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            name: &'a str,
            sequence: u64,
        }
        Wire {
            name: &self.name,
            sequence: self.sequence,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BaseEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            name: String,
            sequence: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        validate_safe_wire_integer(wire.sequence).map_err(serde::de::Error::custom)?;
        Ok(Self {
            name: wire.name,
            sequence: wire.sequence,
        })
    }
}

impl WirePayload for BaseEvent {
    const KIND: EnvelopeKind = EnvelopeKind::Event;

    fn validate(&self) -> Result<(), ProtocolError> {
        validate_bounded_text(&self.name, 256)?;
        validate_safe_wire_integer(self.sequence)
    }
}

/// Base request metadata used for a bounded request/result correlation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseRequest {
    pub operation: String,
    pub correlation_id: StableId,
}

impl WirePayload for BaseRequest {
    const KIND: EnvelopeKind = EnvelopeKind::Request;

    fn validate(&self) -> Result<(), ProtocolError> {
        validate_bounded_text(&self.operation, 256)
    }
}

/// Base result metadata used for a bounded request/result correlation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseResult {
    pub correlation_id: StableId,
    pub accepted: bool,
}

impl WirePayload for BaseResult {
    const KIND: EnvelopeKind = EnvelopeKind::Result;
}

/// Normalized protocol error body, never an implementation-native error object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl WirePayload for BaseError {
    const KIND: EnvelopeKind = EnvelopeKind::Error;

    fn validate(&self) -> Result<(), ProtocolError> {
        validate_bounded_text(&self.code, 256)?;
        validate_bounded_text(&self.message, 4096)
    }
}

fn validate_bounded_text(value: &str, maximum: usize) -> Result<(), ProtocolError> {
    if value.is_empty() || value.chars().count() > maximum {
        Err(ProtocolError::InvalidText)
    } else {
        Ok(())
    }
}

fn validate_safe_wire_integer(value: u64) -> Result<(), ProtocolError> {
    if value > MAX_SAFE_WIRE_INTEGER {
        Err(ProtocolError::InvalidWireInteger)
    } else {
        Ok(())
    }
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
    #[error("envelope kind mismatch: expected {expected:?}, found {actual:?}")]
    EnvelopeKindMismatch {
        expected: EnvelopeKind,
        actual: EnvelopeKind,
    },
    #[error("process generation exceeds the exact cross-language wire range")]
    InvalidProcessGeneration,
    #[error("integer exceeds the exact cross-language wire range")]
    InvalidWireInteger,
    #[error("wire text field is empty or exceeds its schema bound")]
    InvalidText,
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

/// Validates a typed envelope before applying the generic bounded framing.
pub fn encode_typed_frame<T: Serialize + WirePayload>(
    envelope: &Envelope<T>,
) -> Result<Vec<u8>, ProtocolError> {
    envelope.validate_typed()?;
    encode_frame(envelope)
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
        let mut messages = Vec::new();
        let mut offset = 0;
        while offset < chunk.len() {
            if self.buffer.len() < FRAME_PREFIX_BYTES {
                let prefix_bytes =
                    (FRAME_PREFIX_BYTES - self.buffer.len()).min(chunk.len() - offset);
                self.buffer
                    .extend_from_slice(&chunk[offset..offset + prefix_bytes]);
                offset += prefix_bytes;
                if self.buffer.len() < FRAME_PREFIX_BYTES {
                    continue;
                }
            }
            let body_length = u32::from_be_bytes(
                self.buffer[..FRAME_PREFIX_BYTES]
                    .try_into()
                    .expect("prefix has four bytes"),
            );
            let body_length =
                usize::try_from(body_length).expect("u32 fits usize on supported platforms");
            if body_length > MAX_FRAME_BYTES {
                self.buffer.clear();
                return Err(ProtocolError::FrameTooLarge);
            }
            let frame_length = FRAME_PREFIX_BYTES + body_length;
            let body_bytes = (frame_length - self.buffer.len()).min(chunk.len() - offset);
            self.buffer
                .extend_from_slice(&chunk[offset..offset + body_bytes]);
            offset += body_bytes;
            if self.buffer.len() != frame_length {
                continue;
            }
            let decoded = decode_frame(&self.buffer);
            self.buffer.clear();
            messages.push(decoded?);
        }
        Ok(messages)
    }
}

/// Decodes one envelope and enforces version and payload-kind compatibility.
pub fn decode_typed_frame<T: DeserializeOwned + WirePayload>(
    frame: &[u8],
) -> Result<Envelope<T>, ProtocolError> {
    let envelope: Envelope<T> = decode_frame(frame)?;
    envelope.validate_typed()?;
    Ok(envelope)
}

/// Validates an envelope header before dispatching it to an owning domain DTO.
pub fn validate_envelope<T>(envelope: &Envelope<T>) -> Result<(), ProtocolError> {
    if envelope.schema_version != SchemaVersion::V1 {
        return Err(ProtocolError::UnsupportedSchemaVersion(
            envelope.schema_version.0,
        ));
    }
    if envelope.generation.0 > MAX_SAFE_WIRE_INTEGER {
        return Err(ProtocolError::InvalidProcessGeneration);
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

    #[test]
    fn deserialization_cannot_bypass_stable_id_validation() {
        let invalid = r#"{"schemaVersion":1,"messageId":"../escape","generation":1,"kind":"command","payload":{"name":"test","targetId":"core"}}"#;
        assert!(serde_json::from_str::<Envelope<BaseCommand>>(invalid).is_err());
        let invalid_target = r#"{"schemaVersion":1,"messageId":"message","generation":1,"kind":"command","payload":{"name":"test","targetId":"💥"}}"#;
        assert!(serde_json::from_str::<Envelope<BaseCommand>>(invalid_target).is_err());
    }

    #[test]
    fn typed_decoder_rejects_kind_payload_mismatch() {
        let mut mismatched = command();
        mismatched.kind = EnvelopeKind::Event;
        let frame = encode_frame(&mismatched).expect("frame");
        assert!(matches!(
            decode_typed_frame::<BaseCommand>(&frame),
            Err(ProtocolError::EnvelopeKindMismatch {
                expected: EnvelopeKind::Command,
                actual: EnvelopeKind::Event
            })
        ));
    }

    #[test]
    fn wire_generation_must_be_exact_in_javascript() {
        let invalid = format!(
            "{{\"schemaVersion\":1,\"messageId\":\"message\",\"generation\":{},\"kind\":\"command\",\"payload\":{{\"name\":\"test\",\"targetId\":\"core\"}}}}",
            MAX_SAFE_WIRE_INTEGER + 1
        );
        assert!(serde_json::from_str::<Envelope<BaseCommand>>(&invalid).is_err());
        assert!(serde_json::to_string(&ProcessGeneration(MAX_SAFE_WIRE_INTEGER + 1)).is_err());
    }

    #[test]
    fn decoder_handles_many_coalesced_frames_larger_than_one_frame_limit() {
        let payload = BaseEvent {
            name: "x".repeat(400_000),
            sequence: 1,
        };
        let envelope = Envelope::typed(
            StableId::parse("large.event").expect("id"),
            ProcessGeneration(1),
            payload,
        );
        let frame = encode_frame(&envelope).expect("frame");
        let chunk: Vec<u8> = frame
            .iter()
            .copied()
            .cycle()
            .take(frame.len() * 3)
            .collect();
        assert!(chunk.len() > MAX_FRAME_BYTES);
        let decoded = FrameDecoder::default()
            .push::<Envelope<BaseEvent>>(&chunk)
            .expect("coalesced frames");
        assert_eq!(decoded.len(), 3);
        assert!(decoded.iter().all(|item| item == &envelope));
    }

    #[test]
    fn typed_encoding_enforces_payload_bounds_and_exact_wire_integers() {
        let unicode = Envelope::typed(
            StableId::parse("unicode.event").expect("id"),
            ProcessGeneration(1),
            BaseEvent {
                name: "🦀".repeat(256),
                sequence: MAX_SAFE_WIRE_INTEGER,
            },
        );
        encode_typed_frame(&unicode).expect("256 Unicode scalar values");
        let too_long = Envelope {
            payload: BaseEvent {
                name: "🦀".repeat(257),
                sequence: 1,
            },
            ..unicode.clone()
        };
        assert!(matches!(
            encode_typed_frame(&too_long),
            Err(ProtocolError::InvalidText)
        ));
        let unsafe_integer = BaseEvent {
            name: "event".into(),
            sequence: MAX_SAFE_WIRE_INTEGER + 1,
        };
        assert!(serde_json::to_string(&unsafe_integer).is_err());
    }

    #[test]
    fn incremental_decoder_recovers_after_rejected_oversize_prefix() {
        let mut decoder = FrameDecoder::default();
        let oversized = u32::try_from(MAX_FRAME_BYTES + 1)
            .expect("frame limit fits u32")
            .to_be_bytes();
        assert!(matches!(
            decoder.push::<Envelope<BaseCommand>>(&oversized),
            Err(ProtocolError::FrameTooLarge)
        ));
        let valid = encode_typed_frame(&command()).expect("valid frame");
        assert_eq!(
            decoder
                .push::<Envelope<BaseCommand>>(&valid)
                .expect("decoder reset"),
            [command()]
        );
    }
}
