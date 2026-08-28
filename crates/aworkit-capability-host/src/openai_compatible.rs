//! Blocking OpenAI-compatible provider transport with explicit resource bounds.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufReader, Read};
use std::time::Duration;

use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{ACCEPT, HeaderValue};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    CancellationToken, ModelEventV1, ModelRequestV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
    model_tools::validate_tool_request,
    provider_tools::{OpenAiRequestParametersV1, consume_openai_stream, openai_tool_request},
};

const MAX_IDENTITY_BYTES: usize = 256;
const MAX_MODEL_BYTES: usize = 256;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Network and response limits for one OpenAI-compatible provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenAiCompatibleLimitsV1 {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub maximum_response_bytes: usize,
}

impl Default for OpenAiCompatibleLimitsV1 {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(60),
            maximum_response_bytes: 1024 * 1024,
        }
    }
}

impl OpenAiCompatibleLimitsV1 {
    fn validate(self) -> Result<Self, OpenAiCompatibleProviderError> {
        if self.connect_timeout.is_zero()
            || self.connect_timeout > MAX_CONNECT_TIMEOUT
            || self.request_timeout.is_zero()
            || self.request_timeout > MAX_REQUEST_TIMEOUT
            || self.maximum_response_bytes == 0
            || self.maximum_response_bytes > MAX_RESPONSE_BYTES
        {
            return Err(OpenAiCompatibleProviderError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Validated immutable configuration for an OpenAI-compatible provider.
///
/// The API key is private, zeroized on drop, and deliberately excluded from
/// serialization and debug output. Reconstruct the provider when credentials
/// or endpoint settings change.
pub struct OpenAiCompatibleProviderConfig {
    binding_id: String,
    version_hash: String,
    base_url: Url,
    model: String,
    api_key: Option<Zeroizing<String>>,
    limits: OpenAiCompatibleLimitsV1,
    request_parameters: OpenAiRequestParametersV1,
}

impl OpenAiCompatibleProviderConfig {
    pub fn new(
        binding_id: impl Into<String>,
        version_hash: impl Into<String>,
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
        limits: OpenAiCompatibleLimitsV1,
    ) -> Result<Self, OpenAiCompatibleProviderError> {
        let binding_id = binding_id.into();
        let version_hash = version_hash.into();
        let model = model.into();
        if !valid_identity(&binding_id) {
            return Err(OpenAiCompatibleProviderError::InvalidBindingId);
        }
        if !valid_identity(&version_hash) {
            return Err(OpenAiCompatibleProviderError::InvalidVersionHash);
        }
        if !valid_model(&model) {
            return Err(OpenAiCompatibleProviderError::InvalidModel);
        }
        let api_key = api_key
            .map(Zeroizing::new)
            .map(validate_api_key)
            .transpose()?;
        Ok(Self {
            binding_id,
            version_hash,
            base_url: validate_base_url(base_url.as_ref())?,
            model,
            api_key,
            limits: limits.validate()?,
            request_parameters: OpenAiRequestParametersV1::default(),
        })
    }

    /// Freezes the closed, non-secret model Settings consumed by this
    /// OpenAI-compatible adapter.
    pub fn with_request_parameters(
        mut self,
        parameters: &BTreeMap<String, Value>,
    ) -> Result<Self, OpenAiCompatibleProviderError> {
        self.request_parameters = OpenAiRequestParametersV1::from_settings(parameters)
            .map_err(|()| OpenAiCompatibleProviderError::InvalidRequestParameters)?;
        Ok(self)
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

    #[must_use]
    pub fn limits(&self) -> OpenAiCompatibleLimitsV1 {
        self.limits
    }
}

impl fmt::Debug for OpenAiCompatibleProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleProviderConfig")
            .field("binding_id", &self.binding_id)
            .field("version_hash", &self.version_hash)
            .field("base_url", &self.base_url.as_str())
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("limits", &self.limits)
            .finish()
    }
}

/// Result returned by the bounded `GET <base>/models` connection check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenAiConnectionTestV1 {
    pub models: Vec<String>,
    pub configured_model_available: bool,
}

/// Production adapter for streaming OpenAI-compatible chat completions.
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleProviderConfig,
    client: Client,
    models_url: Url,
    completions_url: Url,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        config: OpenAiCompatibleProviderConfig,
    ) -> Result<Self, OpenAiCompatibleProviderError> {
        let client = Client::builder()
            .connect_timeout(config.limits.connect_timeout)
            .timeout(config.limits.request_timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|_| OpenAiCompatibleProviderError::ClientConstruction)?;
        let models_url = config
            .base_url
            .join("models")
            .map_err(|_| OpenAiCompatibleProviderError::InvalidBaseUrl)?;
        let completions_url = config
            .base_url
            .join("chat/completions")
            .map_err(|_| OpenAiCompatibleProviderError::InvalidBaseUrl)?;
        Ok(Self {
            config,
            client,
            models_url,
            completions_url,
        })
    }

    #[must_use]
    pub fn config(&self) -> &OpenAiCompatibleProviderConfig {
        &self.config
    }

    /// Tests connectivity and returns the provider's bounded model catalog.
    pub fn test_connection(&self) -> Result<OpenAiConnectionTestV1, OpenAiCompatibleProviderError> {
        let response = self.send(self.client.get(self.models_url.clone()), "application/json")?;
        let catalog: ModelsResponse = self.decode_success(response)?;
        let mut models = catalog
            .data
            .into_iter()
            .map(|entry| entry.id)
            .filter(|model| valid_model(model))
            .collect::<Vec<_>>();
        models.sort();
        models.dedup();
        let configured_model_available = models.iter().any(|model| model == &self.config.model);
        Ok(OpenAiConnectionTestV1 {
            models,
            configured_model_available,
        })
    }

    fn streaming_completion_response(
        &self,
        request: &ModelRequestV1,
    ) -> Result<Response, OpenAiCompatibleProviderError> {
        let messages = normalize_messages(&request.input)?;
        let mut body = serde_json::to_value(ChatCompletionRequest {
            model: &self.config.model,
            messages: &messages,
            stream: true,
            stream_options: ChatStreamOptions {
                include_usage: true,
            },
        })
        .map_err(|_| OpenAiCompatibleProviderError::InvalidRequest)?;
        self.config.request_parameters.apply(&mut body);
        self.successful_stream(self.send(
            self.client.post(self.completions_url.clone()).json(&body),
            "text/event-stream",
        )?)
    }

    fn streaming_tool_response(
        &self,
        request: &ModelToolRequestV1,
    ) -> Result<Response, ProviderError> {
        validate_tool_request(request)?;
        let body =
            openai_tool_request(&self.config.model, request, &self.config.request_parameters)?;
        let response = self
            .send(
                self.client.post(self.completions_url.clone()).json(&body),
                "text/event-stream",
            )
            .map_err(ProviderError::from)?;
        self.successful_stream(response)
            .map_err(ProviderError::from)
    }

    fn send(
        &self,
        request: RequestBuilder,
        accept: &'static str,
    ) -> Result<Response, OpenAiCompatibleProviderError> {
        let request = request.header(ACCEPT, accept);
        let request = if let Some(api_key) = &self.config.api_key {
            let mut authorization = HeaderValue::from_str(
                Zeroizing::new(format!("Bearer {}", api_key.as_str())).as_str(),
            )
            .map_err(|_| OpenAiCompatibleProviderError::InvalidApiKey)?;
            authorization.set_sensitive(true);
            request.header(reqwest::header::AUTHORIZATION, authorization)
        } else {
            request
        };
        request.send().map_err(|error| {
            if error.is_timeout() {
                OpenAiCompatibleProviderError::RequestTimedOut
            } else {
                OpenAiCompatibleProviderError::Transport
            }
        })
    }

    fn successful_stream(
        &self,
        response: Response,
    ) -> Result<Response, OpenAiCompatibleProviderError> {
        if !response.status().is_success() {
            return Err(OpenAiCompatibleProviderError::HttpStatus(
                response.status().as_u16(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.config.limits.maximum_response_bytes as u64)
        {
            return Err(OpenAiCompatibleProviderError::ResponseTooLarge);
        }
        Ok(response)
    }

    fn decode_success<T: for<'de> Deserialize<'de>>(
        &self,
        response: Response,
    ) -> Result<T, OpenAiCompatibleProviderError> {
        if !response.status().is_success() {
            return Err(OpenAiCompatibleProviderError::HttpStatus(
                response.status().as_u16(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.config.limits.maximum_response_bytes as u64)
        {
            return Err(OpenAiCompatibleProviderError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        response
            .take(self.config.limits.maximum_response_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| OpenAiCompatibleProviderError::Transport)?;
        if bytes.len() > self.config.limits.maximum_response_bytes {
            return Err(OpenAiCompatibleProviderError::ResponseTooLarge);
        }
        serde_json::from_slice(&bytes).map_err(|_| OpenAiCompatibleProviderError::InvalidJson)
    }
}

impl fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleProvider")
            .field("config", &self.config)
            .field("models_url", &self.models_url)
            .field("completions_url", &self.completions_url)
            .finish_non_exhaustive()
    }
}

impl ProviderEnginePortV1 for OpenAiCompatibleProvider {
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
        let response = self
            .streaming_completion_response(request)
            .map_err(ProviderError::from)?;
        let limit = self.config.limits.maximum_response_bytes;
        let mut bridge = |event| match event {
            ModelToolEventV1::AssistantOutput { text } => emit(ModelEventV1::AssistantOutput(text)),
            ModelToolEventV1::ReasoningRaw { text } => emit(ModelEventV1::ReasoningRaw(text)),
            ModelToolEventV1::ReasoningSummary { text } => {
                emit(ModelEventV1::ReasoningSummary(text))
            }
            ModelToolEventV1::Progress { text } => emit(ModelEventV1::Progress(text)),
            ModelToolEventV1::Usage {
                input_tokens,
                output_tokens,
            } => emit(ModelEventV1::Usage {
                input_tokens,
                output_tokens,
            }),
            ModelToolEventV1::ToolCall { .. } => Err(ProviderError::Failed(
                "OpenAI text completion unexpectedly requested a tool".to_owned(),
            )),
        };
        consume_openai_stream(
            BufReader::new(response.take(limit as u64 + 1)),
            limit,
            &[],
            &self.config.binding_id,
            cancellation,
            &mut bridge,
        )?;
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
        let response = self.streaming_tool_response(request)?;
        let limit = self.config.limits.maximum_response_bytes;
        consume_openai_stream(
            BufReader::new(response.take(limit as u64 + 1)),
            limit,
            &request.tools,
            &self.config.binding_id,
            cancellation,
            emit,
        )?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

impl From<OpenAiCompatibleProviderError> for ProviderError {
    fn from(error: OpenAiCompatibleProviderError) -> Self {
        Self::Failed(error.to_string())
    }
}

/// Configuration and bounded transport failures. Messages never include an API
/// key, authorization header, response body, or complete request body.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OpenAiCompatibleProviderError {
    #[error("provider binding id is invalid")]
    InvalidBindingId,
    #[error("provider version hash is invalid")]
    InvalidVersionHash,
    #[error(
        "provider base URL must be an absolute HTTP(S) URL without credentials, query, or fragment"
    )]
    InvalidBaseUrl,
    #[error("provider model name is invalid")]
    InvalidModel,
    #[error("provider API key is invalid")]
    InvalidApiKey,
    #[error("provider limits are invalid")]
    InvalidLimits,
    #[error("provider request parameters are unsupported or invalid")]
    InvalidRequestParameters,
    #[error("provider HTTP client could not be constructed")]
    ClientConstruction,
    #[error("provider request input is invalid")]
    InvalidRequest,
    #[error("provider request timed out")]
    RequestTimedOut,
    #[error("provider transport failed")]
    Transport,
    #[error("provider returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("provider response exceeds the configured size bound")]
    ResponseTooLarge,
    #[error("provider response is not valid JSON")]
    InvalidJson,
    #[error("provider completion response is incomplete or unsupported")]
    InvalidCompletionResponse,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a Value,
    stream: bool,
    stream_options: ChatStreamOptions,
}

#[derive(Serialize)]
struct ChatStreamOptions {
    include_usage: bool,
}

fn validate_base_url(value: &str) -> Result<Url, OpenAiCompatibleProviderError> {
    let mut url = Url::parse(value).map_err(|_| OpenAiCompatibleProviderError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.cannot_be_a_base()
    {
        return Err(OpenAiCompatibleProviderError::InvalidBaseUrl);
    }
    let normalized_path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&normalized_path);
    Ok(url)
}

fn validate_api_key(
    value: Zeroizing<String>,
) -> Result<Zeroizing<String>, OpenAiCompatibleProviderError> {
    if value.is_empty()
        || value.len() > MAX_API_KEY_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(OpenAiCompatibleProviderError::InvalidApiKey);
    }
    Ok(value)
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES && !value.chars().any(char::is_control)
}

fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@')
        })
}

fn normalize_messages(input: &Value) -> Result<Value, OpenAiCompatibleProviderError> {
    let messages = match input {
        Value::String(text) if !text.is_empty() => serde_json::json!([{
            "role": "user",
            "content": text,
        }]),
        Value::Array(messages) if !messages.is_empty() => Value::Array(messages.clone()),
        Value::Object(object) => {
            if let Some(Value::Array(messages)) = object.get("messages") {
                if messages.is_empty() {
                    return Err(OpenAiCompatibleProviderError::InvalidRequest);
                }
                Value::Array(messages.clone())
            } else if object.contains_key("role") && object.contains_key("content") {
                Value::Array(vec![input.clone()])
            } else {
                return Err(OpenAiCompatibleProviderError::InvalidRequest);
            }
        }
        _ => return Err(OpenAiCompatibleProviderError::InvalidRequest),
    };
    let Value::Array(entries) = &messages else {
        return Err(OpenAiCompatibleProviderError::InvalidRequest);
    };
    if entries.iter().any(|entry| {
        !entry.as_object().is_some_and(|entry| {
            entry.get("role").and_then(Value::as_str).is_some() && entry.contains_key("content")
        })
    }) {
        return Err(OpenAiCompatibleProviderError::InvalidRequest);
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};

    use serde_json::json;

    use super::*;

    struct FixtureRequest {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    struct FixtureResponse {
        status: u16,
        body: Vec<u8>,
        delay: Duration,
    }

    fn start_fixture(
        handler: impl FnOnce(FixtureRequest) -> FixtureResponse + Send + 'static,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            let request = read_request(&mut stream);
            let response = handler(request);
            if !response.delay.is_zero() {
                thread::sleep(response.delay);
            }
            let reason = if response.status == 200 {
                "OK"
            } else {
                "Error"
            };
            let header = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                reason,
                response.body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&response.body);
        });
        (format!("http://{address}/v1"), handle)
    }

    fn read_request(stream: &mut TcpStream) -> FixtureRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("fixture read timeout");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).expect("fixture request");
            assert_ne!(read, 0, "request ended before headers");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let header_text = String::from_utf8(bytes[..header_end].to_vec()).expect("request headers");
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().expect("request line");
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().expect("method").to_owned();
        let path = request_parts.next().expect("path").to_owned();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
            .collect::<BTreeMap<_, _>>();
        let content_length = headers
            .get("content-length")
            .map_or(0, |value| value.parse::<usize>().expect("content length"));
        while bytes.len() - header_end < content_length {
            let read = stream.read(&mut buffer).expect("fixture body");
            assert_ne!(read, 0, "request ended before body");
            bytes.extend_from_slice(&buffer[..read]);
        }
        FixtureRequest {
            method,
            path,
            headers,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    fn config(
        base_url: &str,
        api_key: Option<String>,
        limits: OpenAiCompatibleLimitsV1,
    ) -> OpenAiCompatibleProviderConfig {
        OpenAiCompatibleProviderConfig::new(
            "provider.local",
            "config-v1",
            base_url,
            "local/model:latest",
            api_key,
            limits,
        )
        .expect("provider config")
    }

    #[test]
    fn configuration_rejects_unsafe_urls_models_limits_and_redacts_keys() {
        for base_url in [
            "file:///tmp/provider",
            "https://user:secret@example.com/v1",
            "https://example.com/v1?token=secret",
            "https://example.com/v1#fragment",
        ] {
            assert_eq!(
                OpenAiCompatibleProviderConfig::new(
                    "provider.local",
                    "v1",
                    base_url,
                    "model",
                    None,
                    OpenAiCompatibleLimitsV1::default(),
                )
                .expect_err("unsafe URL"),
                OpenAiCompatibleProviderError::InvalidBaseUrl
            );
        }
        assert_eq!(
            OpenAiCompatibleProviderConfig::new(
                "provider.local",
                "v1",
                "https://example.com/v1",
                "bad model",
                None,
                OpenAiCompatibleLimitsV1::default(),
            )
            .expect_err("invalid model"),
            OpenAiCompatibleProviderError::InvalidModel
        );
        assert_eq!(
            OpenAiCompatibleProviderConfig::new(
                "provider.local",
                "v1",
                "https://example.com/v1",
                "model",
                None,
                OpenAiCompatibleLimitsV1 {
                    request_timeout: Duration::ZERO,
                    ..OpenAiCompatibleLimitsV1::default()
                },
            )
            .expect_err("unbounded timeout"),
            OpenAiCompatibleProviderError::InvalidLimits
        );
        let secret = "sk-never-print-this";
        let config = config(
            "https://example.com/v1",
            Some(secret.to_owned()),
            OpenAiCompatibleLimitsV1::default(),
        );
        let debug = format!("{config:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("[REDACTED]"));
        assert!(config.has_api_key());
        let provider = OpenAiCompatibleProvider::new(config).expect("redacted provider");
        assert!(!format!("{provider:?}").contains(secret));
    }

    #[test]
    fn connection_test_gets_bounded_sorted_model_catalog() {
        let (base_url, server) = start_fixture(|request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/v1/models");
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer sk-fixture")
            );
            FixtureResponse {
                status: 200,
                body: serde_json::to_vec(&json!({
                    "object": "list",
                    "data": [
                        {"id": "other-model"},
                        {"id": "local/model:latest"},
                        {"id": "other-model"}
                    ]
                }))
                .expect("catalog JSON"),
                delay: Duration::ZERO,
            }
        });
        let provider = OpenAiCompatibleProvider::new(config(
            &base_url,
            Some("sk-fixture".into()),
            OpenAiCompatibleLimitsV1::default(),
        ))
        .expect("provider");
        assert_eq!(
            provider.test_connection().expect("connection test"),
            OpenAiConnectionTestV1 {
                models: vec!["local/model:latest".into(), "other-model".into()],
                configured_model_available: true,
            }
        );
        server.join().expect("fixture server");
    }

    #[test]
    fn completion_posts_streaming_chat_and_emits_chunks_and_usage() {
        let (base_url, server) = start_fixture(|request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/chat/completions");
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer sk-fixture")
            );
            let body: Value = serde_json::from_slice(&request.body).expect("request JSON");
            assert_eq!(body["model"], "local/model:latest");
            assert_eq!(body["stream"], true);
            assert_eq!(body["stream_options"]["include_usage"], true);
            assert_eq!(body["reasoning_effort"], "medium");
            assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
            assert_eq!(body["chat_template_kwargs"]["preserve_thinking"], false);
            assert_eq!(body["messages"][0]["role"], "user");
            assert_eq!(body["messages"][0]["content"], "hello");
            let body = [
                format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"reasoning":"Checking "},"finish_reason":null}]})),
                format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"reasoning":"the request."},"finish_reason":null}]})),
                format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":"Hello from "},"finish_reason":null}]})),
                format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":"fixture"},"finish_reason":null}]})),
                format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})),
                format!("data: {}\n\n", json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})),
                "data: [DONE]\n\n".to_owned(),
            ].concat();
            FixtureResponse {
                status: 200,
                body: body.into_bytes(),
                delay: Duration::ZERO,
            }
        });
        let provider = OpenAiCompatibleProvider::new(
            config(
                &base_url,
                Some("sk-fixture".into()),
                OpenAiCompatibleLimitsV1::default(),
            )
            .with_request_parameters(&BTreeMap::from([
                ("reasoningEffort".into(), json!("medium")),
                ("enableThinking".into(), json!(true)),
                ("preserveThinking".into(), json!(false)),
            ]))
            .expect("reasoning parameters"),
        )
        .expect("provider");
        let mut events = Vec::new();
        assert_eq!(
            provider
                .execute(
                    &ModelRequestV1 {
                        input: json!("hello")
                    },
                    &mut |event| {
                        events.push(event);
                        Ok(())
                    }
                )
                .expect("completion"),
            ProviderAcceptanceV1::Accepted
        );
        assert_eq!(
            events,
            vec![
                ModelEventV1::ReasoningRaw("Checking ".into()),
                ModelEventV1::ReasoningRaw("the request.".into()),
                ModelEventV1::AssistantOutput("Hello from ".into()),
                ModelEventV1::AssistantOutput("fixture".into()),
                ModelEventV1::Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                },
            ]
        );
        server.join().expect("fixture server");
    }

    #[test]
    fn response_size_and_request_timeout_are_enforced() {
        let limits = OpenAiCompatibleLimitsV1 {
            maximum_response_bytes: 64,
            ..OpenAiCompatibleLimitsV1::default()
        };
        let (base_url, server) = start_fixture(|request| {
            assert!(!request.headers.contains_key("authorization"));
            FixtureResponse {
                status: 200,
                body: vec![b'x'; 65],
                delay: Duration::ZERO,
            }
        });
        let provider = OpenAiCompatibleProvider::new(config(&base_url, None, limits))
            .expect("bounded provider");
        assert_eq!(
            provider.test_connection(),
            Err(OpenAiCompatibleProviderError::ResponseTooLarge)
        );
        server.join().expect("fixture server");

        let timeout_limits = OpenAiCompatibleLimitsV1 {
            request_timeout: Duration::from_millis(100),
            ..OpenAiCompatibleLimitsV1::default()
        };
        let (base_url, server) = start_fixture(|_| FixtureResponse {
            status: 200,
            body: br#"{"data":[]}"#.to_vec(),
            delay: Duration::from_millis(500),
        });
        let provider = OpenAiCompatibleProvider::new(config(&base_url, None, timeout_limits))
            .expect("timeout provider");
        assert_eq!(
            provider.test_connection(),
            Err(OpenAiCompatibleProviderError::RequestTimedOut)
        );
        server.join().expect("fixture server");
    }

    #[test]
    fn invalid_completion_does_not_emit_partial_events() {
        let (base_url, server) = start_fixture(|_| FixtureResponse {
            status: 200,
            body: br#"{"choices":[{"message":{"content":"answer"}}]}"#.to_vec(),
            delay: Duration::ZERO,
        });
        let provider = OpenAiCompatibleProvider::new(config(
            &base_url,
            None,
            OpenAiCompatibleLimitsV1::default(),
        ))
        .expect("provider");
        let mut events = Vec::new();
        assert_eq!(
            provider.execute(
                &ModelRequestV1 {
                    input: json!("hello")
                },
                &mut |event| {
                    events.push(event);
                    Ok(())
                }
            ),
            Err(ProviderError::Failed(
                "OpenAI stream contained an unsupported SSE field".to_owned()
            ))
        );
        assert!(events.is_empty());
        server.join().expect("fixture server");
    }
}
