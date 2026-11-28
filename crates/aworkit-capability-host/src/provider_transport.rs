//! Cancellable HTTP transport for the synchronous model worker.

use std::{
    future::Future,
    io::{self, Read},
    sync::OnceLock,
    time::Duration,
};

use reqwest::{Client, RequestBuilder, Response, Url, redirect::Policy};
use serde::Deserialize;
use thiserror::Error;
use tokio::runtime::Runtime;

use crate::CancellationToken;

pub(crate) const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub(crate) fn validate_limits(
    connect_timeout: Duration,
    request_timeout: Duration,
    maximum_response_bytes: usize,
) -> bool {
    !connect_timeout.is_zero()
        && connect_timeout <= MAX_CONNECT_TIMEOUT
        && !request_timeout.is_zero()
        && request_timeout <= MAX_REQUEST_TIMEOUT
        && maximum_response_bytes > 0
        && (maximum_response_bytes == usize::MAX || maximum_response_bytes <= MAX_RESPONSE_BYTES)
}

pub(crate) fn validate_base_url(value: &str) -> Result<Url, BoundedJsonError> {
    let mut url = Url::parse(value).map_err(|_| BoundedJsonError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.cannot_be_a_base()
    {
        return Err(BoundedJsonError::InvalidBaseUrl);
    }
    let normalized_path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&normalized_path);
    Ok(url)
}

pub(crate) struct BoundedJsonClient {
    client: Client,
    maximum_response_bytes: usize,
}

impl BoundedJsonClient {
    pub(crate) fn new(
        connect_timeout: Duration,
        request_timeout: Duration,
        maximum_response_bytes: usize,
    ) -> Result<Self, BoundedJsonError> {
        if !validate_limits(connect_timeout, request_timeout, maximum_response_bytes) {
            return Err(BoundedJsonError::InvalidLimits);
        }
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|_| BoundedJsonError::ClientConstruction)?;
        Ok(Self {
            client,
            maximum_response_bytes,
        })
    }

    pub(crate) fn get(&self, url: Url) -> RequestBuilder {
        self.client.get(url)
    }

    pub(crate) fn post(&self, url: Url) -> RequestBuilder {
        self.client.post(url)
    }

    pub(crate) fn send<T: for<'de> Deserialize<'de>>(
        &self,
        request: RequestBuilder,
        cancellation: &CancellationToken,
    ) -> Result<T, BoundedJsonError> {
        ProviderResponse::send(request, cancellation)?.decode_json(self.maximum_response_bytes)
    }
}

/// Runs provider I/O on one shared reactor. Cancelling drops the actual HTTP
/// future, so the Chat worker can settle without a detached blocking request.
fn wait_for_io<T>(
    cancellation: &CancellationToken,
    operation: impl Future<Output = Result<T, reqwest::Error>>,
) -> Result<T, BoundedJsonError> {
    static RUNTIME: OnceLock<Result<Runtime, io::Error>> = OnceLock::new();
    let runtime = RUNTIME
        .get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build())
        .as_ref()
        .map_err(|_| BoundedJsonError::ClientConstruction)?;
    runtime.block_on(async {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(BoundedJsonError::Cancelled),
            result = operation => result.map_err(|error| {
                if error.is_timeout() {
                    BoundedJsonError::RequestTimedOut
                } else {
                    BoundedJsonError::Transport
                }
            }),
        }
    })
}

/// A cancellable reader so streaming parsing stays on the Chat worker and
/// publishes chunks in order. Each read can interrupt a silent response body.
pub(crate) struct ProviderResponse<'a> {
    response: Response,
    cancellation: &'a CancellationToken,
    remainder: io::Cursor<Vec<u8>>,
}

impl<'a> ProviderResponse<'a> {
    pub(crate) fn send(
        request: RequestBuilder,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, BoundedJsonError> {
        let response = wait_for_io(cancellation, async { request.send().await })?;
        Ok(Self {
            response,
            cancellation,
            remainder: io::Cursor::default(),
        })
    }

    pub(crate) fn success(self, limit: usize) -> Result<Self, BoundedJsonError> {
        let status = self.response.status();
        if !status.is_success() {
            let status = status.as_u16();
            // Read the wait the provider named before consuming the response.
            let retry_after_seconds = retry_after_seconds(&self.response);
            // Preserve the status even if the bounded diagnostic cannot be read.
            let detail = match self.read_bounded(64 * 1024) {
                Ok(bytes) => bytes,
                Err(BoundedJsonError::Cancelled) => return Err(BoundedJsonError::Cancelled),
                Err(_) => Vec::new(),
            };
            let (overflow, excerpt) = context_overflow_detail(status, &detail);
            return Err(if overflow {
                BoundedJsonError::ContextWindowExceeded
            } else {
                BoundedJsonError::HttpStatus {
                    status,
                    detail: http_status_detail(excerpt.as_deref()),
                    retry_after_seconds,
                }
            });
        }
        if self.response.content_length().is_some_and(|bytes| bytes > limit as u64) {
            return Err(BoundedJsonError::ResponseTooLarge);
        }
        Ok(self)
    }

    pub(crate) fn decode_json<T: for<'de> Deserialize<'de>>(self, limit: usize) -> Result<T, BoundedJsonError> {
        let bytes = self.success(limit)?.read_bounded(limit)?;
        serde_json::from_slice(&bytes).map_err(|_| BoundedJsonError::InvalidJson)
    }

    fn read_bounded(self, limit: usize) -> Result<Vec<u8>, BoundedJsonError> {
        let mut bytes = Vec::new();
        self.take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| match error.kind() {
                io::ErrorKind::ConnectionAborted => BoundedJsonError::Cancelled,
                io::ErrorKind::TimedOut => BoundedJsonError::RequestTimedOut,
                _ => BoundedJsonError::Transport,
            })?;
        if bytes.len() > limit {
            return Err(BoundedJsonError::ResponseTooLarge);
        }
        Ok(bytes)
    }
}

impl Read for ProviderResponse<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let copied = self.remainder.read(output)?;
        if copied != 0 || output.is_empty() {
            return Ok(copied);
        }
        let chunk = wait_for_io(self.cancellation, self.response.chunk()).map_err(|error| match error {
            BoundedJsonError::Cancelled => io::ErrorKind::ConnectionAborted,
            BoundedJsonError::RequestTimedOut => io::ErrorKind::TimedOut,
            _ => io::ErrorKind::Other,
        })?;
        let Some(chunk) = chunk else { return Ok(0); };
        let copied = output.len().min(chunk.len());
        output[..copied].copy_from_slice(&chunk[..copied]);
        self.remainder = io::Cursor::new(chunk[copied..].to_vec());
        Ok(copied)
    }
}

/// Sanitized transport diagnostics. Response and request bodies are never
/// retained in errors.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum BoundedJsonError {
    #[error("provider request cancelled")]
    Cancelled,
    #[error("provider context window exceeded")]
    ContextWindowExceeded,
    #[error("provider base URL is invalid")]
    InvalidBaseUrl,
    #[error("provider HTTP limits are invalid")]
    InvalidLimits,
    #[error("provider HTTP client could not be constructed")]
    ClientConstruction,
    #[error("provider request timed out")]
    RequestTimedOut,
    #[error("provider transport failed")]
    Transport,
    /// The provider's own bounded diagnostic, when it sent one, plus the
    /// `Retry-After` delay it asked for if it named one. A rate limit that names
    /// its own wait is honoured instead of being replaced by a guess.
    #[error("provider returned HTTP status {status}{detail}")]
    HttpStatus {
        status: u16,
        detail: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("provider response exceeds the configured size bound")]
    ResponseTooLarge,
    #[error("provider response is not valid JSON")]
    InvalidJson,
}

/// The provider's own refusal text, bounded and collapsed to a single line.
///
/// A refusal body is the most actionable diagnostic a run can carry, and it used
/// to be discarded: the recorded incident reported only "HTTP status 400" 335
/// times while the body named the malformed message that caused it. The excerpt is
/// bounded and whitespace-collapsed so it can ride in an error, a model notice and
/// a run record without becoming a payload of its own.
pub(crate) const PROVIDER_ERROR_EXCERPT_BYTES: usize = 2048;

/// One bounded single-line excerpt, or `None` when there is nothing to say.
fn bounded_single_line(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut end = collapsed.len().min(PROVIDER_ERROR_EXCERPT_BYTES);
    while !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    Some(collapsed[..end].to_owned())
}

/// The provider's message from a bounded error body, preferring the structured
/// `error.message` an OpenAI-compatible provider sends.
fn provider_error_excerpt(bytes: &[u8]) -> Option<String> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        let error = value.get("error").unwrap_or(&value);
        for key in ["message", "detail"] {
            if let Some(text) = error.get(key).and_then(|value| value.as_str()) {
                return bounded_single_line(text);
            }
        }
        if let Some(text) = error.as_str() {
            return bounded_single_line(text);
        }
    }
    bounded_single_line(&String::from_utf8_lossy(bytes))
}

/// The suffix one HTTP status error carries: empty, or the provider's own words.
pub(crate) fn http_status_detail(detail: Option<&str>) -> String {
    detail.map(|detail| format!(": {detail}")).unwrap_or_default()
}

/// The delay a provider asked for through `Retry-After`, in whole seconds.
///
/// Only the delta-seconds form is honoured; an HTTP-date or a malformed value is
/// ignored rather than guessed at, so the caller falls back to its own bounded
/// backoff. The value is capped so a hostile or mistaken header cannot park a run.
pub(crate) fn retry_after_seconds(response: &Response) -> Option<u64> {
    const MAXIMUM_RETRY_AFTER_SECONDS: u64 = 300;
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map(|seconds| seconds.min(MAXIMUM_RETRY_AFTER_SECONDS))
}

/// Read only a bounded error body, report whether it is a canonical overflow, and
/// return the provider's own bounded diagnostic when it sent one.
/// An arbitrary 400/413, network error, quota error or output-limit failure is
/// never proof that shortening conversation can repair the request.
fn context_overflow_detail(status: u16, bytes: &[u8]) -> (bool, Option<String>) {
    let overflow = matches!(status, 400 | 413 | 422)
        && serde_json::from_slice(bytes)
            .ok()
            .is_some_and(|value| is_context_overflow(&value));
    (overflow, provider_error_excerpt(bytes))
}

fn is_context_overflow(value: &serde_json::Value) -> bool {
    let error = value.get("error").unwrap_or(value);
    if ["code", "type"].iter().any(|key| {
        matches!(
            error[*key].as_str(),
            Some("context_length_exceeded" | "context_window_exceeded")
        )
    }) {
        return true;
    }
    let message = error["message"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = error["type"].as_str().or_else(|| error["status"].as_str());
    matches!(kind, Some("invalid_request_error" | "INVALID_ARGUMENT"))
        && (message.starts_with("prompt is too long")
            || (message.contains("input token count") && message.contains("exceeds the maximum"))
            || (message.contains("maximum context length") && message.contains("tokens")))
}

#[cfg(test)]
mod provider_error_excerpt_tests {
    use super::*;

    #[test]
    fn a_refusal_keeps_the_providers_own_message() {
        // The recorded incident reported only "HTTP status 400" 335 times while
        // the body named the malformed message that caused it.
        let body = br#"{"error":{"message":"Invalid assistant message: content or tool_calls is required","type":"invalid_request_error"}}"#;
        assert_eq!(
            provider_error_excerpt(body).as_deref(),
            Some("Invalid assistant message: content or tool_calls is required")
        );
    }

    #[test]
    fn a_plain_or_empty_body_stays_single_line_and_bounded() {
        assert_eq!(
            provider_error_excerpt(b"  quota\nexceeded  ").as_deref(),
            Some("quota exceeded")
        );
        assert_eq!(provider_error_excerpt(b"   \n  "), None);
        assert_eq!(provider_error_excerpt(b""), None);
        let long = vec![b'x'; PROVIDER_ERROR_EXCERPT_BYTES * 2];
        let excerpt = provider_error_excerpt(&long).expect("bounded excerpt");
        assert_eq!(excerpt.len(), PROVIDER_ERROR_EXCERPT_BYTES);
    }

    #[test]
    fn the_status_suffix_is_empty_without_a_message() {
        assert_eq!(http_status_detail(None), "");
        assert_eq!(http_status_detail(Some("nope")), ": nope");
    }
}

#[cfg(test)]
mod compaction_errors {
    use super::*;
    use serde_json::json;
    #[test]
    fn recognizes_only_provider_context_overflow() {
        for error in [
            json!({"code":"context_length_exceeded"}),
            json!({"type":"invalid_request_error","message":"prompt is too long: 123 tokens > 100 maximum"}),
            json!({"status":"INVALID_ARGUMENT","message":"The input token count exceeds the maximum number of tokens allowed"}),
        ] {
            assert!(is_context_overflow(&json!({"error":error})));
        }
        for error in [
            json!({"code":"rate_limit_exceeded"}),
            json!({"type":"invalid_request_error","message":"max_tokens is too large"}),
            json!({"message":"prompt is too long"}),
            json!({"type":"authentication_error","message":"prompt is too long"}),
        ] {
            assert!(!is_context_overflow(&json!({"error":error})));
        }
    }
}
