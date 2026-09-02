//! Bounded non-streaming adapter for Google Gemini `generateContent`.

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
    provider_tools::{gemini_tool_request, normalize_gemini_tool_response},
    provider_transport::{BoundedJsonClient, BoundedJsonError, validate_base_url, validate_limits},
};

const MAX_IDENTITY_BYTES: usize = 256;
const MAX_MODEL_BYTES: usize = 256;
const MAX_API_KEY_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoogleGeminiLimitsV1 {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub maximum_response_bytes: usize,
}

impl Default for GoogleGeminiLimitsV1 {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(300),
            maximum_response_bytes: 1024 * 1024,
        }
    }
}

impl GoogleGeminiLimitsV1 {
    fn validate(self) -> Result<Self, GoogleGeminiProviderError> {
        if !validate_limits(
            self.connect_timeout,
            self.request_timeout,
            self.maximum_response_bytes,
        ) {
            return Err(GoogleGeminiProviderError::InvalidLimits);
        }
        Ok(self)
    }
}

pub struct GoogleGeminiProviderConfig {
    binding_id: String,
    version_hash: String,
    base_url: Url,
    model: String,
    model_path_id: String,
    api_key: Option<Zeroizing<String>>,
    limits: GoogleGeminiLimitsV1,
}

impl GoogleGeminiProviderConfig {
    pub fn new(
        binding_id: impl Into<String>,
        version_hash: impl Into<String>,
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
        limits: GoogleGeminiLimitsV1,
    ) -> Result<Self, GoogleGeminiProviderError> {
        let binding_id = binding_id.into();
        let version_hash = version_hash.into();
        let model = model.into();
        if !valid_identity(&binding_id) {
            return Err(GoogleGeminiProviderError::InvalidBindingId);
        }
        if !valid_identity(&version_hash) {
            return Err(GoogleGeminiProviderError::InvalidVersionHash);
        }
        let model_path_id = normalized_model_id(&model)
            .ok_or(GoogleGeminiProviderError::InvalidModel)?
            .to_owned();
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
            model_path_id,
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

impl fmt::Debug for GoogleGeminiProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoogleGeminiProviderConfig")
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
pub struct GoogleGeminiModelV1 {
    pub id: String,
    pub name: String,
    pub input_token_limit: Option<u64>,
    pub output_token_limit: Option<u64>,
    pub supported_generation_methods: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoogleGeminiConnectionTestV1 {
    pub models: Vec<GoogleGeminiModelV1>,
    pub configured_model_available: bool,
}

pub struct GoogleGeminiProvider {
    config: GoogleGeminiProviderConfig,
    transport: BoundedJsonClient,
    models_url: Url,
    completion_url: Url,
}

impl GoogleGeminiProvider {
    pub fn new(config: GoogleGeminiProviderConfig) -> Result<Self, GoogleGeminiProviderError> {
        let transport = BoundedJsonClient::new(
            config.limits.connect_timeout,
            config.limits.request_timeout,
            config.limits.maximum_response_bytes,
        )
        .map_err(map_transport_configuration_error)?;
        let mut models_url = gemini_endpoint(&config.base_url, "models")?;
        models_url.query_pairs_mut().append_pair("pageSize", "100");
        let completion_url = gemini_endpoint(
            &config.base_url,
            &format!("models/{}:generateContent", config.model_path_id),
        )?;
        Ok(Self {
            config,
            transport,
            models_url,
            completion_url,
        })
    }

    #[must_use]
    pub fn config(&self) -> &GoogleGeminiProviderConfig {
        &self.config
    }

    pub fn test_connection(
        &self,
    ) -> Result<GoogleGeminiConnectionTestV1, GoogleGeminiProviderError> {
        let request = self.authorize(self.transport.get(self.models_url.clone()))?;
        let catalog: ModelsResponse = self.transport.send(request).map_err(map_transport_error)?;
        let configured = self.config.model_path_id.as_str();
        let mut models = catalog
            .models
            .into_iter()
            .filter_map(|model| {
                let id = normalized_model_id(&model.name)?.to_owned();
                if !model
                    .supported_generation_methods
                    .iter()
                    .any(|method| method == "generateContent")
                {
                    return None;
                }
                Some(GoogleGeminiModelV1 {
                    name: model.display_name.unwrap_or_else(|| id.clone()),
                    id,
                    input_token_limit: model.input_token_limit,
                    output_token_limit: model.output_token_limit,
                    supported_generation_methods: model.supported_generation_methods,
                })
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models.dedup_by(|left, right| left.id == right.id);
        let configured_model_available = models.iter().any(|model| model.id == configured);
        Ok(GoogleGeminiConnectionTestV1 {
            models,
            configured_model_available,
        })
    }

    fn complete(
        &self,
        request: &ModelRequestV1,
    ) -> Result<NormalizedCompletion, GoogleGeminiProviderError> {
        let (system_instruction, contents) = normalize_messages(&request.input)?;
        let body = GenerateContentRequest {
            system_instruction,
            contents,
            generation_config: GenerationConfig { candidate_count: 1 },
        };
        let request = self.authorize(
            self.transport
                .post(self.completion_url.clone())
                .header(CONTENT_TYPE, "application/json")
                .json(&body),
        )?;
        let mut completion: GenerateContentResponse =
            self.transport.send(request).map_err(map_transport_error)?;
        if completion.candidates.len() != 1 {
            return Err(GoogleGeminiProviderError::InvalidCompletionResponse);
        }
        let candidate = completion.candidates.remove(0);
        let text = candidate
            .content
            .parts
            .into_iter()
            .filter_map(|part| part.text)
            .collect::<String>();
        if text.trim().is_empty() {
            return Err(GoogleGeminiProviderError::InvalidCompletionResponse);
        }
        let output_tokens = completion
            .usage_metadata
            .candidates_token_count
            .checked_add(completion.usage_metadata.thoughts_token_count)
            .ok_or(GoogleGeminiProviderError::InvalidCompletionResponse)?;
        Ok(NormalizedCompletion {
            text,
            input_tokens: completion.usage_metadata.prompt_token_count,
            output_tokens,
        })
    }

    fn complete_tool_turn(
        &self,
        request: &ModelToolRequestV1,
    ) -> Result<Vec<ModelToolEventV1>, ProviderError> {
        validate_tool_request(request)?;
        let body = gemini_tool_request(request)?;
        let transport_request = self
            .authorize(
                self.transport
                    .post(self.completion_url.clone())
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
            normalize_gemini_tool_response(completion, &request.tools, &self.config.binding_id)?;
        validate_tool_events(request, &events)?;
        Ok(events)
    }

    fn authorize(
        &self,
        request: RequestBuilder,
    ) -> Result<RequestBuilder, GoogleGeminiProviderError> {
        let request = request.header(ACCEPT, "application/json");
        let Some(api_key) = &self.config.api_key else {
            return Ok(request);
        };
        let mut header = HeaderValue::from_str(api_key.as_str())
            .map_err(|_| GoogleGeminiProviderError::InvalidApiKey)?;
        header.set_sensitive(true);
        Ok(request.header(HeaderName::from_static("x-goog-api-key"), header))
    }
}

impl fmt::Debug for GoogleGeminiProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoogleGeminiProvider")
            .field("config", &self.config)
            .field("models_url", &self.models_url)
            .field("completion_url", &self.completion_url)
            .finish_non_exhaustive()
    }
}

impl ProviderEnginePortV1 for GoogleGeminiProvider {
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

impl From<GoogleGeminiProviderError> for ProviderError {
    fn from(error: GoogleGeminiProviderError) -> Self {
        match error {
            GoogleGeminiProviderError::RequestTimedOut => Self::RequestTimedOut,
            other => Self::Failed(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GoogleGeminiProviderError {
    #[error("Gemini provider binding id is invalid")]
    InvalidBindingId,
    #[error("Gemini provider version hash is invalid")]
    InvalidVersionHash,
    #[error("Gemini base URL must be absolute HTTP(S) without credentials, query, or fragment")]
    InvalidBaseUrl,
    #[error("Gemini model name is invalid")]
    InvalidModel,
    #[error("Gemini API key is invalid")]
    InvalidApiKey,
    #[error("Gemini provider limits are invalid")]
    InvalidLimits,
    #[error("Gemini request messages are invalid")]
    InvalidRequest,
    #[error("Gemini completion response is incomplete or unsupported")]
    InvalidCompletionResponse,
    #[error("Gemini HTTP client could not be constructed")]
    ClientConstruction,
    #[error("Gemini request timed out")]
    RequestTimedOut,
    #[error("Gemini provider transport failed")]
    Transport,
    #[error("Gemini provider returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("Gemini response exceeds the configured size bound")]
    ResponseTooLarge,
    #[error("Gemini response is not valid JSON")]
    InvalidJson,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsResponse {
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelEntry {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    input_token_limit: Option<u64>,
    #[serde(default)]
    output_token_limit: Option<u64>,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    contents: Vec<GeminiContent>,
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    candidate_count: u8,
}

#[derive(Deserialize, Serialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Deserialize, Serialize)]
struct GeminiPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    candidates: Vec<Candidate>,
    usage_metadata: UsageMetadata,
}

#[derive(Deserialize)]
struct Candidate {
    content: GeminiContent,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    prompt_token_count: u64,
    candidates_token_count: u64,
    #[serde(default)]
    thoughts_token_count: u64,
}

struct NormalizedCompletion {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
}

fn normalize_messages(
    input: &Value,
) -> Result<(Option<GeminiContent>, Vec<GeminiContent>), GoogleGeminiProviderError> {
    let messages = input_messages(input)?;
    let mut system = Vec::new();
    let mut contents = Vec::new();
    let mut saw_conversation = false;
    for message in messages {
        if message.content.is_empty() {
            return Err(GoogleGeminiProviderError::InvalidRequest);
        }
        match message.role.as_str() {
            "system" if !saw_conversation => system.push(message.content),
            "user" | "assistant" => {
                saw_conversation = true;
                contents.push(GeminiContent {
                    role: Some(if message.role == "assistant" {
                        "model".to_owned()
                    } else {
                        "user".to_owned()
                    }),
                    parts: vec![GeminiPart {
                        text: Some(message.content),
                    }],
                });
            }
            _ => return Err(GoogleGeminiProviderError::InvalidRequest),
        }
    }
    if contents.is_empty()
        || contents.last().and_then(|content| content.role.as_deref()) != Some("user")
    {
        return Err(GoogleGeminiProviderError::InvalidRequest);
    }
    let system_instruction = (!system.is_empty()).then(|| GeminiContent {
        role: None,
        parts: vec![GeminiPart {
            text: Some(system.join("\n\n")),
        }],
    });
    Ok((system_instruction, contents))
}

fn input_messages(input: &Value) -> Result<Vec<InputMessage>, GoogleGeminiProviderError> {
    let value = match input {
        Value::String(text) => serde_json::json!([{"role":"user","content":text}]),
        Value::Array(_) => input.clone(),
        Value::Object(object) if object.contains_key("messages") => object
            .get("messages")
            .cloned()
            .ok_or(GoogleGeminiProviderError::InvalidRequest)?,
        Value::Object(object) if object.contains_key("role") && object.contains_key("content") => {
            Value::Array(vec![input.clone()])
        }
        _ => return Err(GoogleGeminiProviderError::InvalidRequest),
    };
    serde_json::from_value(value).map_err(|_| GoogleGeminiProviderError::InvalidRequest)
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES && !value.chars().any(char::is_control)
}

fn gemini_endpoint(base_url: &Url, route: &str) -> Result<Url, GoogleGeminiProviderError> {
    let path = base_url.path().trim_end_matches('/');
    let prefix = if path.ends_with("/v1beta") || path.ends_with("/v1") {
        String::new()
    } else {
        "v1beta/".to_owned()
    };
    base_url
        .join(&format!("{prefix}{route}"))
        .map_err(|_| GoogleGeminiProviderError::InvalidBaseUrl)
}

fn normalized_model_id(value: &str) -> Option<&str> {
    let value = value.strip_prefix("models/").unwrap_or(value);
    (!value.is_empty()
        && value.len() <= MAX_MODEL_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')))
    .then_some(value)
}

fn validate_api_key(
    value: Zeroizing<String>,
) -> Result<Zeroizing<String>, GoogleGeminiProviderError> {
    if value.is_empty()
        || value.len() > MAX_API_KEY_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(GoogleGeminiProviderError::InvalidApiKey);
    }
    Ok(value)
}

fn map_transport_configuration_error(error: BoundedJsonError) -> GoogleGeminiProviderError {
    match error {
        BoundedJsonError::InvalidBaseUrl => GoogleGeminiProviderError::InvalidBaseUrl,
        BoundedJsonError::InvalidLimits => GoogleGeminiProviderError::InvalidLimits,
        other => map_transport_error(other),
    }
}

fn map_transport_error(error: BoundedJsonError) -> GoogleGeminiProviderError {
    match error {
        BoundedJsonError::InvalidBaseUrl => GoogleGeminiProviderError::InvalidBaseUrl,
        BoundedJsonError::InvalidLimits => GoogleGeminiProviderError::InvalidLimits,
        BoundedJsonError::ClientConstruction => GoogleGeminiProviderError::ClientConstruction,
        BoundedJsonError::RequestTimedOut => GoogleGeminiProviderError::RequestTimedOut,
        BoundedJsonError::Transport => GoogleGeminiProviderError::Transport,
        BoundedJsonError::HttpStatus(status) => GoogleGeminiProviderError::HttpStatus(status),
        BoundedJsonError::ResponseTooLarge => GoogleGeminiProviderError::ResponseTooLarge,
        BoundedJsonError::InvalidJson => GoogleGeminiProviderError::InvalidJson,
    }
}
