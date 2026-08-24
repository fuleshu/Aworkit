//! Streamable HTTP client wrapper that keeps credential values out of SDK config.

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::streamable_http_client::{
        AuthRequiredError, InsufficientScopeError, SseError, StreamableHttpClient,
        StreamableHttpError, StreamableHttpPostResponse,
    },
};
use sse_stream::Sse;
use zeroize::Zeroizing;

use super::secrets::MaterializedTransportSecrets;

/// The SDK's public HTTP config derives `Debug`. This wrapper deliberately
/// keeps secrets in a non-formattable Aworkit-owned type and creates sensitive
/// `HeaderValue` instances only for the duration of each request.
pub(super) struct SecretHttpClient {
    inner: reqwest_mcp::Client,
    secrets: Arc<MaterializedTransportSecrets>,
    bearer_token_secret_slot: Option<String>,
    maximum_json_response_bytes: usize,
}

impl SecretHttpClient {
    pub(super) fn new(
        secrets: Arc<MaterializedTransportSecrets>,
        bearer_token_secret_slot: Option<String>,
        maximum_json_response_bytes: usize,
    ) -> Result<Self, reqwest_mcp::Error> {
        Ok(Self {
            // Credential-bearing headers must never be replayed to a redirect
            // target. Match the SDK's hardened default client and avoid stale
            // pooled connections between independently attested requests.
            inner: reqwest_mcp::Client::builder()
                .redirect(reqwest_mcp::redirect::Policy::none())
                .pool_max_idle_per_host(0)
                .build()?,
            secrets,
            bearer_token_secret_slot,
            maximum_json_response_bytes,
        })
    }

    fn bound_response(
        &self,
        response: StreamableHttpPostResponse,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<reqwest_mcp::Error>> {
        if let StreamableHttpPostResponse::Json(message, _) = &response {
            let bytes = serde_json::to_vec(message).map_err(StreamableHttpError::Deserialize)?;
            if bytes.len() > self.maximum_json_response_bytes {
                return Err(StreamableHttpError::UnexpectedServerResponse(
                    Cow::Borrowed("MCP HTTP JSON response exceeded the configured bound"),
                ));
            }
        }
        Ok(response)
    }

    fn sanitize_error(
        error: StreamableHttpError<reqwest_mcp::Error>,
    ) -> StreamableHttpError<reqwest_mcp::Error> {
        match error {
            StreamableHttpError::UnexpectedServerResponse(_) => {
                StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed(
                    "MCP HTTP server returned an unexpected response",
                ))
            }
            StreamableHttpError::UnexpectedContentType(_) => {
                StreamableHttpError::UnexpectedContentType(None)
            }
            StreamableHttpError::AuthRequired(_) => {
                StreamableHttpError::AuthRequired(AuthRequiredError::new("[REDACTED]".to_owned()))
            }
            StreamableHttpError::InsufficientScope(_) => StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new("[REDACTED]".to_owned(), None),
            ),
            other => other,
        }
    }

    fn merge_headers(
        &self,
        mut headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<HashMap<HeaderName, HeaderValue>, StreamableHttpError<reqwest_mcp::Error>> {
        for (slot, name, secret) in self.secrets.headers() {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed(
                    "invalid materialized HTTP header name",
                ))
            })?;
            let mut encoded = Zeroizing::new(Vec::new());
            let bytes = if self.bearer_token_secret_slot.as_deref() == Some(slot) {
                encoded.extend_from_slice(b"Bearer ");
                encoded.extend_from_slice(secret);
                encoded.as_slice()
            } else {
                secret
            };
            let mut value = HeaderValue::from_bytes(bytes).map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed(
                    "invalid materialized HTTP header value",
                ))
            })?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        Ok(headers)
    }
}

impl Clone for SecretHttpClient {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            secrets: self.secrets.clone(),
            bearer_token_secret_slot: self.bearer_token_secret_slot.clone(),
            maximum_json_response_bytes: self.maximum_json_response_bytes,
        }
    }
}

impl StreamableHttpClient for SecretHttpClient {
    type Error = reqwest_mcp::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let headers = self.merge_headers(custom_headers)?;
        let response = <reqwest_mcp::Client as StreamableHttpClient>::post_message(
            &self.inner,
            uri,
            message,
            session_id,
            auth_header,
            headers,
        )
        .await
        .map_err(Self::sanitize_error)?;
        self.bound_response(response)
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        maximum_sse_event_bytes: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let headers = self.merge_headers(custom_headers)?;
        let response =
            <reqwest_mcp::Client as StreamableHttpClient>::post_message_with_max_sse_event_size(
                &self.inner,
                uri,
                message,
                session_id,
                auth_header,
                headers,
                maximum_sse_event_bytes,
            )
            .await
            .map_err(Self::sanitize_error)?;
        self.bound_response(response)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let headers = self.merge_headers(custom_headers)?;
        <reqwest_mcp::Client as StreamableHttpClient>::delete_session(
            &self.inner,
            uri,
            session_id,
            auth_header,
            headers,
        )
        .await
        .map_err(Self::sanitize_error)
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let headers = self.merge_headers(custom_headers)?;
        <reqwest_mcp::Client as StreamableHttpClient>::get_stream(
            &self.inner,
            uri,
            session_id,
            last_event_id,
            auth_header,
            headers,
        )
        .await
        .map_err(Self::sanitize_error)
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        maximum_sse_event_bytes: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let headers = self.merge_headers(custom_headers)?;
        <reqwest_mcp::Client as StreamableHttpClient>::get_stream_with_max_sse_event_size(
            &self.inner,
            uri,
            session_id,
            last_event_id,
            auth_header,
            headers,
            maximum_sse_event_bytes,
        )
        .await
        .map_err(Self::sanitize_error)
    }
}
