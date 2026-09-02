//! Provider-neutral contracts for one model/tool interaction loop.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ProviderError;

const INVALID_TOOL_REQUEST: &str = "provider tool request is invalid";
const INVALID_TOOL_RESPONSE: &str = "provider tool response is invalid or unsupported";

const MAX_TOOL_DEFINITIONS: usize = 128;
const MAX_TOOL_EXCHANGES: usize = 64;
const MAX_RETRY_NOTICE_BYTES: usize = 4 * 1024;
const MAX_TOOL_CALLS_PER_EXCHANGE: usize = 64;
const MAX_ASSISTANT_CONTENT_BLOCKS: usize = 256;
const MAX_TEXT_MESSAGES: usize = 4096;
const MAX_TEXT_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_CAPABILITY_ID_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 512 * 1024;
const MAX_CALL_ID_BYTES: usize = 256;
const MAX_PROVIDER_CONTEXT_BYTES: usize = 512 * 1024;
const MAX_JSON_DEPTH: usize = 32;

/// One authority-bearing Aworkit capability exposed under a provider-safe name.
///
/// `capability_id` is the immutable identifier that the trusted authority path
/// understands. `name` is only the provider-facing alias and is deliberately
/// restricted to the portable OpenAI/Anthropic/Gemini naming intersection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolDefinitionV1 {
    pub capability_id: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Opaque provider-issued context that must be returned unchanged.
///
/// Gemini thought signatures are one example. The value is serializable for
/// crash-safe continuation but redacted from debug output and never interpreted
/// by the workflow or authority layers.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelProviderContextV1(String);

impl ModelProviderContextV1 {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ModelProviderContextV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ModelProviderContextV1([OPAQUE])")
    }
}

/// A normalized function request emitted by a model.
///
/// `call_id` is always present and is the ID Aworkit uses for correlation.
/// `provider_call_id` is absent only for legacy providers that omitted an ID;
/// it must be echoed on the wire when present. `capability_id` is resolved from
/// the frozen definition rather than trusted from model output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolCallV1 {
    pub call_id: String,
    pub provider_call_id: Option<String>,
    pub capability_id: String,
    pub name: String,
    pub arguments: Value,
    pub provider_context: Option<ModelProviderContextV1>,
}

/// One authority-settled result returned to a model on the following turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolResultV1 {
    pub call_id: String,
    pub content: Value,
    pub is_error: bool,
}

/// Ordered assistant content retained between tool turns.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelAssistantContentV1 {
    Text { text: String },
    ToolCall { call: ModelToolCallV1 },
}

/// One completed assistant tool-request turn and all correlated host results.
///
/// Grouping calls and results preserves the exact alternating message shape
/// required by all three supported provider protocols, including parallel calls.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolExchangeV1 {
    pub assistant_content: Vec<ModelAssistantContentV1>,
    pub results: Vec<ModelToolResultV1>,
}

/// A stateless model turn with frozen tools and zero or more prior exchanges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolRequestV1 {
    /// Existing text conversation in the same accepted shapes as `ModelRequestV1`.
    pub input: Value,
    /// Closed request overrides supplied by the active workflow node.
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    pub tools: Vec<ModelToolDefinitionV1>,
    pub exchanges: Vec<ModelToolExchangeV1>,
    /// Transient recovery context appended after completed tool exchanges.
    /// It records a failed provider attempt without fabricating model output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_notice: Option<String>,
}

/// Provider-neutral events emitted by a tool-capable model turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelToolEventV1 {
    AssistantOutput {
        text: String,
    },
    ToolCall {
        call: ModelToolCallV1,
    },
    ReasoningRaw {
        text: String,
    },
    ReasoningSummary {
        text: String,
    },
    Progress {
        text: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelToolDispatchEvidenceV1 {
    pub selected_binding: String,
    pub attempted_bindings: Vec<String>,
    pub events: Vec<ModelToolEventV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextMessageRoleV1 {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextMessageV1 {
    pub role: TextMessageRoleV1,
    pub content: String,
}

pub(crate) fn normalize_text_input(input: &Value) -> Result<Vec<TextMessageV1>, ProviderError> {
    if let Value::String(text) = input {
        if text.is_empty() || text.len() > MAX_TEXT_CONTENT_BYTES {
            return Err(invalid_tool_request());
        }
        return Ok(vec![TextMessageV1 {
            role: TextMessageRoleV1::User,
            content: text.clone(),
        }]);
    }
    let entries = match input {
        Value::Array(entries) => entries.as_slice(),
        Value::Object(object) if object.contains_key("messages") => object
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .ok_or_else(invalid_tool_request)?,
        Value::Object(object) if object.contains_key("role") && object.contains_key("content") => {
            std::slice::from_ref(input)
        }
        _ => return Err(invalid_tool_request()),
    };
    if entries.is_empty() || entries.len() > MAX_TEXT_MESSAGES {
        return Err(invalid_tool_request());
    }
    let mut messages = Vec::with_capacity(entries.len());
    let mut saw_conversation = false;
    let mut text_bytes = 0_usize;
    for entry in entries {
        let object = entry.as_object().ok_or_else(invalid_tool_request)?;
        if object.len() != 2 {
            return Err(invalid_tool_request());
        }
        let role = match object.get("role").and_then(Value::as_str) {
            Some("system") if !saw_conversation => TextMessageRoleV1::System,
            Some("user") => {
                saw_conversation = true;
                TextMessageRoleV1::User
            }
            Some("assistant") => {
                saw_conversation = true;
                TextMessageRoleV1::Assistant
            }
            _ => return Err(invalid_tool_request()),
        };
        let content = object
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.is_empty())
            .ok_or_else(invalid_tool_request)?
            .to_owned();
        text_bytes = text_bytes.saturating_add(content.len());
        if text_bytes > MAX_TEXT_CONTENT_BYTES {
            return Err(invalid_tool_request());
        }
        messages.push(TextMessageV1 { role, content });
    }
    if !saw_conversation
        || messages.last().map(|message| message.role) != Some(TextMessageRoleV1::User)
    {
        return Err(invalid_tool_request());
    }
    Ok(messages)
}

pub(crate) fn validate_tool_request(request: &ModelToolRequestV1) -> Result<(), ProviderError> {
    normalize_text_input(&request.input)?;
    if request.tools.is_empty()
        || request.tools.len() > MAX_TOOL_DEFINITIONS
        || request.exchanges.len() > MAX_TOOL_EXCHANGES
        || request.retry_notice.as_ref().is_some_and(|notice| {
            notice.is_empty() || notice.len() > MAX_RETRY_NOTICE_BYTES || notice.contains('\0')
        })
    {
        return Err(invalid_tool_request());
    }

    let mut names = BTreeSet::new();
    let mut capabilities = BTreeSet::new();
    for tool in &request.tools {
        if !valid_tool_name(&tool.name)
            || !valid_identifier(&tool.capability_id, MAX_CAPABILITY_ID_BYTES)
            || tool.description.is_empty()
            || tool.description.len() > MAX_DESCRIPTION_BYTES
            || tool.description.contains('\0')
            || !tool.input_schema.is_object()
            || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
            || serialized_len(&tool.input_schema)? > MAX_SCHEMA_BYTES
            || json_depth(&tool.input_schema, 0) > MAX_JSON_DEPTH
            || !names.insert(tool.name.as_str())
            || !capabilities.insert(tool.capability_id.as_str())
        {
            return Err(invalid_tool_request());
        }
    }

    let definitions = request
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool.capability_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    for exchange in &request.exchanges {
        validate_exchange(exchange, &definitions)?;
    }
    Ok(())
}

fn validate_exchange(
    exchange: &ModelToolExchangeV1,
    definitions: &BTreeMap<&str, &str>,
) -> Result<(), ProviderError> {
    let calls = exchange
        .assistant_content
        .iter()
        .filter_map(|content| match content {
            ModelAssistantContentV1::ToolCall { call } => Some(call),
            ModelAssistantContentV1::Text { .. } => None,
        })
        .collect::<Vec<_>>();
    if calls.is_empty()
        || calls.len() > MAX_TOOL_CALLS_PER_EXCHANGE
        || exchange.assistant_content.len() > MAX_ASSISTANT_CONTENT_BLOCKS
        || exchange.results.len() != calls.len()
    {
        return Err(invalid_tool_request());
    }

    let mut call_ids = BTreeSet::new();
    for content in &exchange.assistant_content {
        match content {
            ModelAssistantContentV1::Text { text }
                if text.is_empty() || text.len() > MAX_RESULT_BYTES || text.contains('\0') =>
            {
                return Err(invalid_tool_request());
            }
            ModelAssistantContentV1::Text { .. } => {}
            ModelAssistantContentV1::ToolCall { call } => {
                validate_call(call, definitions)?;
                if !call_ids.insert(call.call_id.as_str()) {
                    return Err(invalid_tool_request());
                }
            }
        }
    }

    let mut result_ids = BTreeSet::new();
    for result in &exchange.results {
        if !call_ids.contains(result.call_id.as_str())
            || !result_ids.insert(result.call_id.as_str())
            || serialized_len(&result.content)? > MAX_RESULT_BYTES
            || json_depth(&result.content, 0) > MAX_JSON_DEPTH
        {
            return Err(invalid_tool_request());
        }
    }
    if result_ids != call_ids {
        return Err(invalid_tool_request());
    }
    Ok(())
}

fn validate_call(
    call: &ModelToolCallV1,
    definitions: &BTreeMap<&str, &str>,
) -> Result<(), ProviderError> {
    if !valid_identifier(&call.call_id, MAX_CALL_ID_BYTES)
        || call
            .provider_call_id
            .as_deref()
            .is_some_and(|id| !valid_identifier(id, MAX_CALL_ID_BYTES))
        || definitions.get(call.name.as_str()).copied() != Some(call.capability_id.as_str())
        || !call.arguments.is_object()
        || serialized_len(&call.arguments)? > MAX_ARGUMENT_BYTES
        || json_depth(&call.arguments, 0) > MAX_JSON_DEPTH
        || call.provider_context.as_ref().is_some_and(|context| {
            context.as_str().is_empty() || context.as_str().len() > MAX_PROVIDER_CONTEXT_BYTES
        })
    {
        return Err(invalid_tool_request());
    }
    Ok(())
}

pub(crate) fn normalize_tool_call(
    tools: &[ModelToolDefinitionV1],
    provider_call_id: Option<String>,
    name: String,
    arguments: Value,
    provider_context: Option<String>,
    ordinal: usize,
    provider_namespace: &str,
) -> Result<ModelToolCallV1, ProviderError> {
    let definition = tools
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(invalid_tool_response)?;
    if !arguments.is_object()
        || serialized_len(&arguments)? > MAX_ARGUMENT_BYTES
        || json_depth(&arguments, 0) > MAX_JSON_DEPTH
    {
        return Err(invalid_tool_response());
    }
    if provider_call_id
        .as_deref()
        .is_some_and(|id| !valid_identifier(id, MAX_CALL_ID_BYTES))
        || provider_context
            .as_ref()
            .is_some_and(|context| context.is_empty() || context.len() > MAX_PROVIDER_CONTEXT_BYTES)
    {
        return Err(invalid_tool_response());
    }
    let call_id = provider_call_id.clone().unwrap_or_else(|| {
        let mut digest = Sha256::new();
        digest.update(provider_namespace.as_bytes());
        digest.update([0]);
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(ordinal.to_be_bytes());
        if let Ok(arguments) = serde_json::to_vec(&arguments) {
            digest.update(arguments);
        }
        format!("generated-{}", hex_lower(&digest.finalize()[..16]))
    });
    Ok(ModelToolCallV1 {
        call_id,
        provider_call_id,
        capability_id: definition.capability_id.clone(),
        name,
        arguments,
        provider_context: provider_context.map(ModelProviderContextV1::new),
    })
}

pub(crate) fn validate_tool_events(
    request: &ModelToolRequestV1,
    events: &[ModelToolEventV1],
) -> Result<(), ProviderError> {
    if events
        .iter()
        .filter(|event| matches!(event, ModelToolEventV1::Usage { .. }))
        .count()
        != 1
    {
        return Err(ProviderError::MissingOrDuplicateUsage);
    }
    let definitions = request
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool.capability_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut calls = BTreeSet::new();
    let mut call_count = 0_usize;
    let mut has_content = false;
    for event in events {
        match event {
            ModelToolEventV1::AssistantOutput { text }
            | ModelToolEventV1::ReasoningRaw { text }
            | ModelToolEventV1::ReasoningSummary { text }
            | ModelToolEventV1::Progress { text } => {
                if text.is_empty() || text.contains('\0') {
                    return Err(invalid_tool_response());
                }
                has_content = true;
            }
            ModelToolEventV1::ToolCall { call } => {
                call_count = call_count.saturating_add(1);
                if call_count > MAX_TOOL_CALLS_PER_EXCHANGE {
                    return Err(invalid_tool_response());
                }
                validate_call(call, &definitions).map_err(|_| invalid_tool_response())?;
                if !calls.insert(call.call_id.as_str()) {
                    return Err(invalid_tool_response());
                }
                has_content = true;
            }
            ModelToolEventV1::Usage { .. } => {}
        }
    }
    if !has_content {
        return Err(invalid_tool_response());
    }
    Ok(())
}

pub(crate) fn tool_event_bytes(event: &ModelToolEventV1) -> usize {
    match event {
        ModelToolEventV1::AssistantOutput { text }
        | ModelToolEventV1::ReasoningRaw { text }
        | ModelToolEventV1::ReasoningSummary { text }
        | ModelToolEventV1::Progress { text } => text.len(),
        ModelToolEventV1::ToolCall { call } => {
            serde_json::to_vec(call).map_or(usize::MAX, |v| v.len())
        }
        ModelToolEventV1::Usage { .. } => 0,
    }
}

pub(crate) fn result_text(result: &ModelToolResultV1) -> Result<String, ProviderError> {
    if result.is_error {
        return serde_json::to_string(&serde_json::json!({"error": result.content}))
            .map_err(|_| invalid_tool_request());
    }
    match &result.content {
        Value::String(text) => Ok(text.clone()),
        value => serde_json::to_string(value).map_err(|_| invalid_tool_request()),
    }
}

fn valid_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOOL_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn serialized_len(value: &Value) -> Result<usize, ProviderError> {
    serde_json::to_vec(value)
        .map(|value| value.len())
        .map_err(|_| invalid_tool_request())
}

fn invalid_tool_request() -> ProviderError {
    ProviderError::Failed(INVALID_TOOL_REQUEST.to_owned())
}

fn invalid_tool_response() -> ProviderError {
    ProviderError::Failed(INVALID_TOOL_RESPONSE.to_owned())
}

fn json_depth(value: &Value, depth: usize) -> usize {
    let mut maximum = depth;
    let mut pending = vec![(value, depth)];
    while let Some((value, current)) = pending.pop() {
        maximum = maximum.max(current);
        let child_depth = current.saturating_add(1);
        match value {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, child_depth)));
            }
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, child_depth)));
            }
            _ => {}
        }
    }
    maximum
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}
