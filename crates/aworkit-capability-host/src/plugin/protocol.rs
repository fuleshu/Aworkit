//! Bounded, language-neutral framed protocol for trusted plugin subprocesses.

use aworkit_protocol::{
    MAX_FRAME_BYTES, MAX_SAFE_WIRE_INTEGER, ProcessGeneration, ProtocolError, SchemaVersion,
    StableId, decode_frame, encode_frame,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Per-session bounds stricter than the shared one-MiB framing ceiling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginProtocolLimitsV1 {
    pub maximum_frame_bytes: usize,
    pub maximum_text_bytes: usize,
    pub maximum_value_bytes: usize,
    pub maximum_contributions: usize,
}

impl Default for PluginProtocolLimitsV1 {
    fn default() -> Self {
        Self {
            maximum_frame_bytes: 256 * 1024,
            maximum_text_bytes: 16 * 1024,
            maximum_value_bytes: 128 * 1024,
            maximum_contributions: 256,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHandshakeIdentityV1 {
    pub extension_id: StableId,
    pub version: String,
    pub content_hash: String,
    pub protocol_version: u16,
    pub contribution_ids: Vec<StableId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHandshakeRequestV1 {
    pub expected: PluginHandshakeIdentityV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHandshakeResultV1 {
    pub accepted: bool,
    pub observed: PluginHandshakeIdentityV1,
    pub error: Option<PluginProtocolErrorV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInvocationRequestV1 {
    pub invocation_id: StableId,
    pub contribution_id: StableId,
    pub input: Value,
    pub deadline_epoch_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInvocationAcceptedV1 {
    pub invocation_id: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum PluginInvocationEventKindV1 {
    Progress(String),
    StandardOutput(String),
    StandardError(String),
    Output(Value),
    EffectMayHaveStarted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInvocationEventV1 {
    pub invocation_id: StableId,
    pub sequence: u64,
    pub event: PluginInvocationEventKindV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginTerminalStatusV1 {
    Succeeded,
    Failed,
    Cancelled,
}

/// Plugin-supplied effect evidence. Unknown is always handled conservatively.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginEffectStatusV1 {
    DefinitelyNotStarted,
    Started,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInvocationResultV1 {
    pub invocation_id: StableId,
    pub status: PluginTerminalStatusV1,
    pub effect: PluginEffectStatusV1,
    pub output: Option<Value>,
    pub error: Option<PluginProtocolErrorV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHealthRequestV1 {
    pub probe_id: StableId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginHealthStatusV1 {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHealthResultV1 {
    pub probe_id: StableId,
    pub status: PluginHealthStatusV1,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCancelRequestV1 {
    pub invocation_id: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCancelResultV1 {
    pub invocation_id: StableId,
    pub confirmed: bool,
    pub effect: PluginEffectStatusV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginShutdownRequestV1 {
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginShutdownResultV1 {
    pub clean: bool,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginProtocolErrorV1 {
    pub code: String,
    pub message: String,
}

/// Every subprocess request, event, result, and control message has a stable tag.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "messageType", content = "payload")]
pub enum PluginProtocolMessageV1 {
    HandshakeRequest(PluginHandshakeRequestV1),
    HandshakeResult(PluginHandshakeResultV1),
    InvocationRequest(PluginInvocationRequestV1),
    InvocationAccepted(PluginInvocationAcceptedV1),
    InvocationEvent(PluginInvocationEventV1),
    InvocationResult(PluginInvocationResultV1),
    HealthRequest(PluginHealthRequestV1),
    HealthResult(PluginHealthResultV1),
    CancelRequest(PluginCancelRequestV1),
    CancelResult(PluginCancelResultV1),
    ShutdownRequest(PluginShutdownRequestV1),
    ShutdownResult(PluginShutdownResultV1),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginProtocolFrameV1 {
    pub schema_version: SchemaVersion,
    pub message_id: StableId,
    pub host_generation: ProcessGeneration,
    pub message: PluginProtocolMessageV1,
}

/// Encoder/decoder fenced to one authenticated host generation.
#[derive(Clone, Copy, Debug)]
pub struct PluginFrameCodecV1 {
    host_generation: ProcessGeneration,
    limits: PluginProtocolLimitsV1,
}

impl PluginFrameCodecV1 {
    pub fn new(
        host_generation: ProcessGeneration,
        limits: PluginProtocolLimitsV1,
    ) -> Result<Self, PluginFrameError> {
        if host_generation.0 > MAX_SAFE_WIRE_INTEGER
            || limits.maximum_frame_bytes == 0
            || limits.maximum_frame_bytes > MAX_FRAME_BYTES
            || limits.maximum_text_bytes == 0
            || limits.maximum_value_bytes == 0
            || limits.maximum_contributions == 0
        {
            return Err(PluginFrameError::InvalidLimits);
        }
        Ok(Self {
            host_generation,
            limits,
        })
    }

    pub fn frame(
        &self,
        message_id: StableId,
        message: PluginProtocolMessageV1,
    ) -> Result<PluginProtocolFrameV1, PluginFrameError> {
        let frame = PluginProtocolFrameV1 {
            schema_version: SchemaVersion::V1,
            message_id,
            host_generation: self.host_generation,
            message,
        };
        self.validate(&frame)?;
        Ok(frame)
    }

    pub fn encode(&self, frame: &PluginProtocolFrameV1) -> Result<Vec<u8>, PluginFrameError> {
        self.validate(frame)?;
        let encoded = encode_frame(frame)?;
        if encoded.len().saturating_sub(4) > self.limits.maximum_frame_bytes {
            return Err(PluginFrameError::FrameTooLarge);
        }
        Ok(encoded)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<PluginProtocolFrameV1, PluginFrameError> {
        if bytes.len() < 4 {
            return Err(PluginFrameError::Wire(ProtocolError::TruncatedFrame));
        }
        let declared = declared_body_length(bytes)?;
        if declared > self.limits.maximum_frame_bytes {
            return Err(PluginFrameError::FrameTooLarge);
        }
        let frame: PluginProtocolFrameV1 = decode_frame(bytes)?;
        self.validate(&frame)?;
        Ok(frame)
    }

    pub fn decoder(self) -> PluginFrameDecoderV1 {
        PluginFrameDecoderV1 {
            codec: self,
            buffer: Vec::new(),
        }
    }

    fn validate(&self, frame: &PluginProtocolFrameV1) -> Result<(), PluginFrameError> {
        if frame.schema_version != SchemaVersion::V1 {
            return Err(PluginFrameError::UnsupportedSchemaVersion(
                frame.schema_version.0,
            ));
        }
        if frame.host_generation != self.host_generation {
            return Err(PluginFrameError::HostGenerationDrift);
        }
        validate_message(&frame.message, self.limits)
    }
}

/// Incremental decoder for stdout chunks from a persistent plugin process.
#[derive(Debug)]
pub struct PluginFrameDecoderV1 {
    codec: PluginFrameCodecV1,
    buffer: Vec<u8>,
}

impl PluginFrameDecoderV1 {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<PluginProtocolFrameV1>, PluginFrameError> {
        // Bound every transport read before extending the decoder buffer.
        if chunk.len() > self.codec.limits.maximum_frame_bytes.saturating_add(4) {
            return Err(PluginFrameError::ChunkTooLarge);
        }
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            if self.buffer.len() < 4 {
                break;
            }
            let body_length = declared_body_length(&self.buffer)?;
            if body_length > self.codec.limits.maximum_frame_bytes {
                self.buffer.clear();
                return Err(PluginFrameError::FrameTooLarge);
            }
            let frame_length = body_length
                .checked_add(4)
                .ok_or(PluginFrameError::FrameTooLarge)?;
            if self.buffer.len() < frame_length {
                break;
            }
            let trailing = self.buffer.split_off(frame_length);
            let frame = std::mem::replace(&mut self.buffer, trailing);
            messages.push(self.codec.decode(&frame)?);
        }
        if self.buffer.len() > self.codec.limits.maximum_frame_bytes.saturating_add(4) {
            self.buffer.clear();
            return Err(PluginFrameError::FrameTooLarge);
        }
        Ok(messages)
    }

    pub fn finish(self) -> Result<(), PluginFrameError> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(PluginFrameError::Wire(ProtocolError::TruncatedFrame))
        }
    }
}

fn declared_body_length(bytes: &[u8]) -> Result<usize, PluginFrameError> {
    let prefix: [u8; 4] = bytes
        .get(..4)
        .ok_or(PluginFrameError::Wire(ProtocolError::TruncatedFrame))?
        .try_into()
        .expect("four-byte slice");
    Ok(usize::try_from(u32::from_be_bytes(prefix)).expect("u32 fits supported usize"))
}

fn validate_message(
    message: &PluginProtocolMessageV1,
    limits: PluginProtocolLimitsV1,
) -> Result<(), PluginFrameError> {
    match message {
        PluginProtocolMessageV1::HandshakeRequest(value) => {
            validate_handshake_identity(&value.expected, limits)
        }
        PluginProtocolMessageV1::HandshakeResult(value) => {
            validate_handshake_identity(&value.observed, limits)?;
            validate_optional_error(value.error.as_ref(), limits)?;
            if value.accepted == value.error.is_some() {
                return Err(PluginFrameError::InvalidMessage);
            }
            Ok(())
        }
        PluginProtocolMessageV1::InvocationRequest(value) => {
            if value.deadline_epoch_millis == 0
                || value.deadline_epoch_millis > MAX_SAFE_WIRE_INTEGER
            {
                return Err(PluginFrameError::InvalidMessage);
            }
            validate_value(&value.input, limits)
        }
        PluginProtocolMessageV1::InvocationAccepted(_) => Ok(()),
        PluginProtocolMessageV1::InvocationEvent(value) => {
            if value.sequence == 0 || value.sequence > MAX_SAFE_WIRE_INTEGER {
                return Err(PluginFrameError::InvalidMessage);
            }
            match &value.event {
                PluginInvocationEventKindV1::Progress(text)
                | PluginInvocationEventKindV1::StandardOutput(text)
                | PluginInvocationEventKindV1::StandardError(text) => {
                    validate_text(text, limits.maximum_text_bytes, false)
                }
                PluginInvocationEventKindV1::Output(output) => validate_value(output, limits),
                PluginInvocationEventKindV1::EffectMayHaveStarted => Ok(()),
            }
        }
        PluginProtocolMessageV1::InvocationResult(value) => {
            if let Some(output) = &value.output {
                validate_value(output, limits)?;
            }
            validate_optional_error(value.error.as_ref(), limits)?;
            match value.status {
                PluginTerminalStatusV1::Succeeded if value.error.is_some() => {
                    Err(PluginFrameError::InvalidMessage)
                }
                PluginTerminalStatusV1::Failed if value.error.is_none() => {
                    Err(PluginFrameError::InvalidMessage)
                }
                _ => Ok(()),
            }
        }
        PluginProtocolMessageV1::HealthRequest(_) => Ok(()),
        PluginProtocolMessageV1::HealthResult(value) => {
            validate_optional_text(value.detail.as_deref(), limits)
        }
        PluginProtocolMessageV1::CancelRequest(_) => Ok(()),
        PluginProtocolMessageV1::CancelResult(value) => {
            if !value.confirmed && value.effect != PluginEffectStatusV1::Unknown {
                return Err(PluginFrameError::InvalidMessage);
            }
            Ok(())
        }
        PluginProtocolMessageV1::ShutdownRequest(value) => {
            validate_text(&value.reason, limits.maximum_text_bytes, false)
        }
        PluginProtocolMessageV1::ShutdownResult(value) => {
            validate_optional_text(value.detail.as_deref(), limits)
        }
    }
}

fn validate_handshake_identity(
    value: &PluginHandshakeIdentityV1,
    limits: PluginProtocolLimitsV1,
) -> Result<(), PluginFrameError> {
    validate_text(&value.version, limits.maximum_text_bytes, false)?;
    validate_text(&value.content_hash, limits.maximum_text_bytes, false)?;
    if value.protocol_version == 0
        || value.contribution_ids.is_empty()
        || value.contribution_ids.len() > limits.maximum_contributions
    {
        return Err(PluginFrameError::InvalidMessage);
    }
    if value
        .contribution_ids
        .windows(2)
        .any(|pair| pair[0].as_str() >= pair[1].as_str())
    {
        return Err(PluginFrameError::NonCanonicalContributions);
    }
    Ok(())
}

fn validate_optional_error(
    value: Option<&PluginProtocolErrorV1>,
    limits: PluginProtocolLimitsV1,
) -> Result<(), PluginFrameError> {
    if let Some(value) = value {
        validate_text(&value.code, 256.min(limits.maximum_text_bytes), false)?;
        validate_text(&value.message, limits.maximum_text_bytes, false)?;
    }
    Ok(())
}

fn validate_optional_text(
    value: Option<&str>,
    limits: PluginProtocolLimitsV1,
) -> Result<(), PluginFrameError> {
    if let Some(value) = value {
        validate_text(value, limits.maximum_text_bytes, true)?;
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize, allow_empty: bool) -> Result<(), PluginFrameError> {
    if (!allow_empty && value.is_empty()) || value.len() > maximum || value.contains('\0') {
        Err(PluginFrameError::InvalidMessage)
    } else {
        Ok(())
    }
}

fn validate_value(value: &Value, limits: PluginProtocolLimitsV1) -> Result<(), PluginFrameError> {
    if serde_json::to_vec(value)?.len() > limits.maximum_value_bytes || json_depth(value) > 64 {
        Err(PluginFrameError::ValueTooLarge)
    } else {
        Ok(())
    }
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

#[derive(Debug, Error)]
pub enum PluginFrameError {
    #[error("plugin frame encoding or decoding failed: {0}")]
    Wire(#[from] ProtocolError),
    #[error("plugin protocol JSON validation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("plugin protocol limits or host generation are invalid")]
    InvalidLimits,
    #[error("plugin frame exceeds the configured session bound")]
    FrameTooLarge,
    #[error("plugin transport chunk exceeds the configured session bound")]
    ChunkTooLarge,
    #[error("plugin frame uses unsupported schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("plugin frame belongs to a stale host generation")]
    HostGenerationDrift,
    #[error("plugin protocol message violates its bounded contract")]
    InvalidMessage,
    #[error("plugin handshake contribution IDs are not sorted and unique")]
    NonCanonicalContributions,
    #[error("plugin protocol JSON value exceeds its bound")]
    ValueTooLarge,
}
