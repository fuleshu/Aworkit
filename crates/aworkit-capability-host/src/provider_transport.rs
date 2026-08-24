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
        && maximum_response_bytes <= MAX_RESPONSE_BYTES
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
            return Err(BoundedJsonError::HttpStatus(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.maximum_response_bytes as u64)
        {
            return Err(BoundedJsonError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        response
            .take(self.maximum_response_bytes as u64 + 1)
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
    #[error("provider returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("provider response exceeds the configured size bound")]
    ResponseTooLarge,
    #[error("provider response is not valid JSON")]
    InvalidJson,
}
