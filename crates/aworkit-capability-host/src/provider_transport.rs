//! Shared bounded blocking JSON transport for built-in model providers.

use std::{io::Read, time::Duration};

use reqwest::{
    Url,
    blocking::{Client, RequestBuilder},
    redirect::Policy,
};
use serde::Deserialize;
use thiserror::Error;

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
    ) -> Result<T, BoundedJsonError> {
        let response = request.send().map_err(|error| {
            if error.is_timeout() {
                BoundedJsonError::RequestTimedOut
            } else {
                BoundedJsonError::Transport
            }
        })?;
        if !response.status().is_success() {
            let (status, overflow, detail) = context_overflow_response(response);
            return Err(if overflow {
                BoundedJsonError::ContextWindowExceeded
            } else {
                BoundedJsonError::HttpStatus {
                    status,
                    detail: http_status_detail(detail.as_deref()),
                }
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.maximum_response_bytes as u64)
        {
            return Err(BoundedJsonError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        response
            .take((self.maximum_response_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| BoundedJsonError::Transport)?;
        if bytes.len() > self.maximum_response_bytes {
            return Err(BoundedJsonError::ResponseTooLarge);
        }
        serde_json::from_slice(&bytes).map_err(|_| BoundedJsonError::InvalidJson)
    }
}

/// Sanitized transport diagnostics. Response and request bodies are never
/// retained in errors.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum BoundedJsonError {
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
    /// The provider's own bounded diagnostic, when it sent one.
    #[error("provider returned HTTP status {status}{detail}")]
    HttpStatus { status: u16, detail: String },
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

/// Read only a bounded error body, report whether it is a canonical overflow, and
/// return the provider's own bounded diagnostic when it sent one.
/// An arbitrary 400/413, network error, quota error or output-limit failure is
/// never proof that shortening conversation can repair the request.
pub(crate) fn context_overflow_response(
    response: reqwest::blocking::Response,
) -> (u16, bool, Option<String>) {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    let read = response
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .is_ok()
        && bytes.len() <= 64 * 1024;
    if !read {
        return (status, false, None);
    }
    let overflow = matches!(status, 400 | 413 | 422)
        && serde_json::from_slice(&bytes)
            .ok()
            .is_some_and(|value| is_context_overflow(&value));
    (status, overflow, provider_error_excerpt(&bytes))
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
