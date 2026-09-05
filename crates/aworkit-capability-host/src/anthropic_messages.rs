//! Bounded non-streaming adapter for Anthropic's Messages API.

use std::{fmt, time::Duration};

use reqwest::{
    Url,
    blocking::RequestBuilder,
    header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    CancellationToken, ModelEventV1, ModelRequestV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
    model_tools::{validate_tool_events, validate_tool_request},
    provider_tools::{anthropic_tool_request, normalize_anthropic_tool_response},
    provider_transport::{BoundedJsonClient, BoundedJsonError, validate_base_url, validate_limits},
};

const MAX_IDENTITY_BYTES: usize = 256;
const MAX_MODEL_BYTES: usize = 256;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 1_000_000;
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnthropicMessagesLimitsV1 {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub maximum_response_bytes: usize,
    pub maximum_output_tokens: u32,
}

impl Default for AnthropicMessagesLimitsV1 {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(300),
            maximum_response_bytes: 1024 * 1024,
            maximum_output_tokens: 4096,
        }
    }
}

impl AnthropicMessagesLimitsV1 {
    fn validate(self) -> Result<Self, AnthropicMessagesProviderError> {
        if !validate_limits(
            self.connect_timeout,
            self.request_timeout,
            self.maximum_response_bytes,
        ) || self.maximum_output_tokens == 0
            || self.maximum_output_tokens > MAX_OUTPUT_TOKENS
        {
            return Err(AnthropicMessagesProviderError::InvalidLimits);
        }
        Ok(self)
    }
}

pub struct AnthropicMessagesProviderConfig {
    binding_id: String,
    version_hash: String,
    base_url: Url,
    model: String,
    api_key: Option<Zeroizing<String>>,
    limits: AnthropicMessagesLimitsV1,
}

impl AnthropicMessagesProviderConfig {
    pub fn new(
        binding_id: impl Into<String>,
        version_hash: impl Into<String>,
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
        limits: AnthropicMessagesLimitsV1,
    ) -> Result<Self, AnthropicMessagesProviderError> {
        let binding_id = binding_id.into();
        let version_hash = version_hash.into();
        let model = model.into();
        if !valid_identity(&binding_id) {
            return Err(AnthropicMessagesProviderError::InvalidBindingId);
        }
        if !valid_identity(&version_hash) {
            return Err(AnthropicMessagesProviderError::InvalidVersionHash);
        }
        if !valid_model(&model) {
            return Err(AnthropicMessagesProviderError::InvalidModel);
        }
        let api_key = api_key
            .map(Zeroizing::new)
            .map(validate_api_key)
            .transpose()?;
        Ok(Self {
            binding_id,
            version_hash,
            base_url: validate_base_url(base_url.as_ref())
                .map_err(map_transport_configuration_error)?,
            model,
            api_key,
            limits: limits.validate()?,
        })
    }

    #[must_use]
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }

    #[must_use]
    pub fn version_hash(&self) -> &str {
        &self.version_hash
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }
}

impl fmt::Debug for AnthropicMessagesProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicMessagesProviderConfig")
            .field("binding_id", &self.binding_id)
            .field("version_hash", &self.version_hash)
            .field("base_url", &self.base_url.as_str())
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("limits", &self.limits)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnthropicModelV1 {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnthropicConnectionTestV1 {
    pub models: Vec<AnthropicModelV1>,
    pub configured_model_available: bool,
}

pub struct AnthropicMessagesProvider {
    config: AnthropicMessagesProviderConfig,
    transport: BoundedJsonClient,
    models_url: Url,
    messages_url: Url,
}

impl AnthropicMessagesProvider {
    pub fn new(
        config: AnthropicMessagesProviderConfig,
    ) -> Result<Self, AnthropicMessagesProviderError> {
        let transport = BoundedJsonClient::new(
            config.limits.connect_timeout,
            config.limits.request_timeout,
            config.limits.maximum_response_bytes,
        )
        .map_err(map_transport_configuration_error)?;
        let mut models_url = anthropic_endpoint(&config.base_url, "models")?;
        models_url.query_pairs_mut().append_pair("limit", "100");
        let messages_url = anthropic_endpoint(&config.base_url, "messages")?;
        Ok(Self {
            config,
            transport,
            models_url,
            messages_url,
        })
    }

    #[must_use]
    pub fn config(&self) -> &AnthropicMessagesProviderConfig {
        &self.config
    }

    pub fn test_connection(
        &self,
    ) -> Result<AnthropicConnectionTestV1, AnthropicMessagesProviderError> {
        let request = self.authorize(self.transport.get(self.models_url.clone()))?;
        let catalog: ModelsResponse = self.transport.send(request).map_err(map_transport_error)?;
        let mut models = catalog
            .data
            .into_iter()
            .filter(|model| valid_model(&model.id))
            .map(|model| AnthropicModelV1 {
                name: model.display_name.unwrap_or_else(|| model.id.clone()),
                id: model.id,
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models.dedup_by(|left, right| left.id == right.id);
        let configured_model_available = models.iter().any(|model| model.id == self.config.model);
        Ok(AnthropicConnectionTestV1 {
            models,
            configured_model_available,
        })
    }

    fn complete(
        &self,
        request: &ModelRequestV1,
    ) -> Result<NormalizedCompletion, AnthropicMessagesProviderError> {
        let (system, messages) = normalize_messages(&request.input)?;
        let body = MessagesRequest {
            model: self.config.model.clone(),
            maximum_output_tokens: self.config.limits.maximum_output_tokens,
            system,
            messages,
            stream: false,
        };
        let request = self.authorize(
            self.transport
                .post(self.messages_url.clone())
                .header(CONTENT_TYPE, "application/json")
                .json(&body),
        )?;
        let completion: MessagesResponse =
            self.transport.send(request).map_err(map_transport_error)?;
        let text = completion
            .content
            .into_iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text)
            .collect::<String>();
        if text.trim().is_empty() {
            return Err(AnthropicMessagesProviderError::InvalidCompletionResponse);
        }
        let input_tokens = completion
            .usage
            .input_tokens
            .checked_add(completion.usage.cache_creation_input_tokens)
            .and_then(|tokens| tokens.checked_add(completion.usage.cache_read_input_tokens))
            .ok_or(AnthropicMessagesProviderError::InvalidCompletionResponse)?;
        Ok(NormalizedCompletion {
            text,
            input_tokens,
            output_tokens: completion.usage.output_tokens,
        })
    }

    fn complete_tool_turn(
        &self,
        request: &ModelToolRequestV1,
    ) -> Result<Vec<ModelToolEventV1>, ProviderError> {
        validate_tool_request(request)?;
        let body = anthropic_tool_request(
            &self.config.model,
            self.config.limits.maximum_output_tokens,
            request,
        )?;
        let transport_request = self
            .authorize(
                self.transport
                    .post(self.messages_url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .json(&body),
            )
            .map_err(ProviderError::from)?;
        let completion: Value = self
            .transport
            .send(transport_request)
            .map_err(map_transport_error)
            .map_err(ProviderError::from)?;
        let events =
            normalize_anthropic_tool_response(completion, &request.tools, &self.config.binding_id)?;
        validate_tool_events(request, &events)?;
        Ok(events)
    }

    fn authorize(
        &self,
        request: RequestBuilder,
    ) -> Result<RequestBuilder, AnthropicMessagesProviderError> {
        let request = request.header(ACCEPT, "application/json").header(
            HeaderName::from_static("anthropic-version"),
            ANTHROPIC_VERSION,
        );
        let Some(api_key) = &self.config.api_key else {
            return Ok(request);
        };
        let mut header = HeaderValue::from_str(api_key.as_str())
            .map_err(|_| AnthropicMessagesProviderError::InvalidApiKey)?;
        header.set_sensitive(true);
        Ok(request.header(HeaderName::from_static("x-api-key"), header))
    }
}

impl fmt::Debug for AnthropicMessagesProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicMessagesProvider")
            .field("config", &self.config)
            .field("models_url", &self.models_url)
            .field("messages_url", &self.messages_url)
            .finish_non_exhaustive()
    }
}

impl ProviderEnginePortV1 for AnthropicMessagesProvider {
    fn binding_id(&self) -> &str {
        self.config.binding_id()
    }

    fn version_hash(&self) -> &str {
        self.config.version_hash()
    }

    fn execute(
        &self,
        request: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.execute_cancellable(request, &CancellationToken::default(), emit)
    }

    fn execute_cancellable(
        &self,
        request: &ModelRequestV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let completion = self.complete(request).map_err(ProviderError::from)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        emit(ModelEventV1::AssistantOutput(completion.text))?;
        emit(ModelEventV1::Usage {
            input_tokens: completion.input_tokens,
            output_tokens: completion.output_tokens,
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }

    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let events = self.complete_tool_turn(request)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        for event in events {
            emit(event)?;
        }
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

impl From<AnthropicMessagesProviderError> for ProviderError {
    fn from(error: AnthropicMessagesProviderError) -> Self {
        match error {
            AnthropicMessagesProviderError::RequestTimedOut => Self::RequestTimedOut,
            other => Self::Failed(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AnthropicMessagesProviderError {
    #[error("Anthropic provider binding id is invalid")]
    InvalidBindingId,
    #[error("Anthropic provider version hash is invalid")]
    InvalidVersionHash,
    #[error("Anthropic base URL must be absolute HTTP(S) without credentials, query, or fragment")]
    InvalidBaseUrl,
    #[error("Anthropic model name is invalid")]
    InvalidModel,
    #[error("Anthropic API key is invalid")]
    InvalidApiKey,
    #[error("Anthropic provider limits are invalid")]
    InvalidLimits,
    #[error("Anthropic request messages are invalid")]
    InvalidRequest,
    #[error("Anthropic completion response is incomplete or unsupported")]
    InvalidCompletionResponse,
    #[error("Anthropic HTTP client could not be constructed")]
    ClientConstruction,
    #[error("Anthropic request timed out")]
    RequestTimedOut,
    #[error("Anthropic provider transport failed")]
    Transport,
    #[error("Anthropic provider returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("Anthropic response exceeds the configured size bound")]
    ResponseTooLarge,
    #[error("Anthropic response is not valid JSON")]
    InvalidJson,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InputMessage {
    role: String,
    content: Value,
}

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    #[serde(rename = "max_tokens")]
    maximum_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<InputMessage>,
    stream: bool,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    usage: UsageResponse,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

struct NormalizedCompletion {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
}

fn normalize_messages(
    input: &Value,
) -> Result<(Option<String>, Vec<InputMessage>), AnthropicMessagesProviderError> {
    use crate::model_tools::{ModelInputRoleV1, normalize_model_input};
    let messages =
        normalize_model_input(input).map_err(|_| AnthropicMessagesProviderError::InvalidRequest)?;
    let mut system = Vec::new();
    let mut wire = Vec::new();
    for message in messages {
        if message.role == ModelInputRoleV1::System {
            system.push(message.content);
            continue;
        }
        wire.push(InputMessage {
            role: if message.role == ModelInputRoleV1::User {
                "user"
            } else {
                "assistant"
            }
            .into(),
            content: crate::model_images::image_content(
                &message.content,
                &message.images,
                "anthropic",
            )
            .map_err(|_| AnthropicMessagesProviderError::InvalidRequest)?,
        });
    }
    Ok(((!system.is_empty()).then(|| system.join("\n\n")), wire))
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES && !value.chars().any(char::is_control)
}

fn anthropic_endpoint(base_url: &Url, route: &str) -> Result<Url, AnthropicMessagesProviderError> {
    let prefix = if base_url.path().trim_end_matches('/').ends_with("/v1") {
        String::new()
    } else {
        "v1/".to_owned()
    };
    base_url
        .join(&format!("{prefix}{route}"))
        .map_err(|_| AnthropicMessagesProviderError::InvalidBaseUrl)
}

fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@')
        })
}

fn validate_api_key(
    value: Zeroizing<String>,
) -> Result<Zeroizing<String>, AnthropicMessagesProviderError> {
    if value.is_empty()
        || value.len() > MAX_API_KEY_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(AnthropicMessagesProviderError::InvalidApiKey);
    }
    Ok(value)
}

fn map_transport_configuration_error(error: BoundedJsonError) -> AnthropicMessagesProviderError {
    match error {
        BoundedJsonError::InvalidBaseUrl => AnthropicMessagesProviderError::InvalidBaseUrl,
        BoundedJsonError::InvalidLimits => AnthropicMessagesProviderError::InvalidLimits,
        other => map_transport_error(other),
    }
}

fn map_transport_error(error: BoundedJsonError) -> AnthropicMessagesProviderError {
    match error {
        BoundedJsonError::InvalidBaseUrl => AnthropicMessagesProviderError::InvalidBaseUrl,
        BoundedJsonError::InvalidLimits => AnthropicMessagesProviderError::InvalidLimits,
        BoundedJsonError::ClientConstruction => AnthropicMessagesProviderError::ClientConstruction,
        BoundedJsonError::RequestTimedOut => AnthropicMessagesProviderError::RequestTimedOut,
        BoundedJsonError::Transport => AnthropicMessagesProviderError::Transport,
        BoundedJsonError::HttpStatus(status) => AnthropicMessagesProviderError::HttpStatus(status),
        BoundedJsonError::ResponseTooLarge => AnthropicMessagesProviderError::ResponseTooLarge,
        BoundedJsonError::InvalidJson => AnthropicMessagesProviderError::InvalidJson,
    }
}
