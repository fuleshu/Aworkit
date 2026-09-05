//! Bounded web search and fetch tools behind replaceable transport ports.
//!
//! Search supports provider selection, retries, keyless rescue, request
//! coalescing, and a bounded memory cache. Fetch downloads at most a bounded
//! prefix and extracts plain text. Network activity uses desktop-user
//! authority: these tools are read-only, but they are not sandboxes.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::CancellationToken;

mod document;
#[cfg(test)]
mod document_tests;
mod extraction;
mod freshness;
mod retrieval;
pub use document::*;
mod providers;
mod search;

pub use freshness::{WebSearchFreshnessModeV1, WebSearchFreshnessV1};
pub use search::{
    WebSearchAttemptV1, WebSearchBackendV1, WebSearchConfigurationV1, WebSearchOutcomeV1,
    WebSearchProviderTierV1, WebSearchProviderUsageV1,
};

pub(super) const MAXIMUM_SEARCH_RESULTS: usize = 100;
const MAXIMUM_QUERY_BYTES: usize = 16 * 1024;
const MAXIMUM_DOWNLOAD_BYTES: usize = MAXIMUM_WEB_DOCUMENT_BYTES;
const MAXIMUM_EXTRACT_BYTES: usize = 32 * 1024;
const MAXIMUM_EXTRACT_URLS: usize = 10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchResultV1 {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFetchResultV1 {
    pub url: String,
    pub title: String,
    pub text: String,
    pub bytes_downloaded: u64,
    pub metadata: WebDocumentMetadataV1,
    pub preview_truncated: bool,
}

/// One independently settled page in a multi-URL `web_extract` call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebExtractPageV1 {
    pub url: String,
    pub title: String,
    pub content: String,
    pub raw_content: String,
    pub bytes_downloaded: u64,
    pub fetched_at_epoch_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<WebDocumentMetadataV1>,
    #[serde(default)]
    pub preview_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Replaceable network seam used by deterministic tests. Production search
/// uses the provider runtime while fetch uses the plain HTTPS transport.
pub trait WebTransportPort: Send + Sync {
    fn search(&self, query: &str, maximum_results: usize)
    -> Result<Vec<WebSearchResultV1>, String>;

    fn fetch(
        &self,
        url: &str,
        maximum_download_bytes: usize,
    ) -> Result<(String, String, u64), String>;

    fn fetch_document(
        &self,
        url: &str,
        maximum: usize,
        _cancellation: &CancellationToken,
    ) -> Result<WebSourceV1, String> {
        let (title, body, bytes_downloaded) = self.fetch(url, maximum)?;
        Ok(WebSourceV1 {
            final_url: url.into(),
            body,
            content_type: "text/plain".into(),
            bytes_downloaded,
            truncated: false,
            warning: None,
            title: Some(title),
        })
    }
}

#[derive(Clone)]
pub struct WebTools {
    transport: Arc<dyn WebTransportPort>,
    search_runtime: Option<search::WebSearchRuntime>,
    renderer: Option<Arc<dyn WebRendererPort>>,
}

impl WebTools {
    #[must_use]
    pub fn production() -> Self {
        Self {
            transport: Arc::new(ProductionWebTransport),
            renderer: None,
            search_runtime: Some(search::WebSearchRuntime::production()),
        }
    }

    #[must_use]
    pub fn new(transport: Arc<dyn WebTransportPort>) -> Self {
        Self {
            transport,
            search_runtime: None,
            renderer: None,
        }
    }

    pub fn search_v1(
        &self,
        query: &str,
        maximum_results: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WebSearchResultV1>, WebToolError> {
        if query.trim().is_empty() || query.len() > MAXIMUM_QUERY_BYTES || query.contains('\0') {
            return Err(WebToolError::InvalidQuery);
        }
        if maximum_results == 0 || maximum_results > MAXIMUM_SEARCH_RESULTS {
            return Err(WebToolError::InvalidBound);
        }
        check_cancelled(cancellation)?;
        let mut results = if let Some(runtime) = &self.search_runtime {
            let configuration = WebSearchConfigurationV1 {
                maximum_results,
                ..WebSearchConfigurationV1::default()
            };
            runtime
                .search(query, &configuration, None, cancellation)?
                .results
        } else {
            self.transport
                .search(query, maximum_results)
                .map_err(WebToolError::Transport)?
        };
        results.truncate(maximum_results);
        Ok(results)
    }

    /// Executes the exact provider configuration frozen with the Chat. API
    /// key material is invocation-local and is never stored by this runtime.
    pub fn search_configured_v1(
        &self,
        query: &str,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<WebSearchOutcomeV1, WebToolError> {
        self.search_configured_with_freshness_v1(
            query,
            configuration,
            api_key,
            WebSearchFreshnessModeV1::Auto,
            cancellation,
        )
    }

    /// Executes configured search with an explicit call-level freshness mode.
    pub fn search_configured_with_freshness_v1(
        &self,
        query: &str,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        freshness_mode: WebSearchFreshnessModeV1,
        cancellation: &CancellationToken,
    ) -> Result<WebSearchOutcomeV1, WebToolError> {
        validate_query(query)?;
        if let Some(runtime) = &self.search_runtime {
            return runtime.search_with_freshness(
                query,
                configuration,
                api_key,
                freshness_mode,
                cancellation,
            );
        }
        if configuration != &WebSearchConfigurationV1::default() || api_key.is_some() {
            return Err(WebToolError::Transport(
                "the injected test transport supports only default keyless search".into(),
            ));
        }
        let results = self.search_v1(query, configuration.maximum_results, cancellation)?;
        Ok(WebSearchOutcomeV1 {
            query: query.to_owned(),
            backend: "test".into(),
            results,
            cached: false,
            coalesced: false,
            attempts: vec![WebSearchAttemptV1 {
                backend: "test".into(),
                attempt: 1,
                status: "completed".into(),
                error: None,
                usage: None,
            }],
            rescued_from: None,
            backend_error: None,
            freshness: freshness::FreshnessLedger::default().finish(
                &freshness::FreshnessPolicy::resolve(
                    query,
                    freshness_mode,
                    configuration.freshness_validation,
                    configuration.freshness_maximum_age_days,
                ),
            ),
        })
    }

    /// Native composition installs an optional renderer; the caller freezes permission per invocation.
    pub fn with_renderer(mut self, renderer: Arc<dyn WebRendererPort>) -> Self {
        self.renderer = Some(renderer);
        self
    }

    /// Retrieves a full bounded document before any model-preview truncation.
    pub fn document_v1(
        &self,
        url: &str,
        maximum_download_bytes: usize,
        allow_render: bool,
        cancellation: &CancellationToken,
    ) -> Result<WebDocumentV1, WebToolError> {
        let url = parse_https_url(url)?;
        if maximum_download_bytes == 0 || maximum_download_bytes > MAXIMUM_DOWNLOAD_BYTES {
            return Err(WebToolError::InvalidBound);
        }
        check_cancelled(cancellation)?;
        let source = self
            .transport
            .fetch_document(&url, maximum_download_bytes, cancellation);
        check_cancelled(cancellation)?;
        let source = source.map_err(WebToolError::Transport)?;
        let mut extracted = extraction::extract(&source).map_err(WebToolError::Transport)?;
        let mut metadata = WebDocumentMetadataV1 {
            final_url: source.final_url.clone(),
            method: extracted.method.into(),
            quality: extracted.quality.clone(),
            download_truncated: source.truncated,
            snapshot_truncated: false,
            snapshot_bytes: None,
            render_settled: None,
            document_truncated: false,
            fetched_at_epoch_ms: now_epoch_ms(),
            warnings: source.warning.into_iter().collect(),
        };
        if source.truncated {
            metadata.warnings.push("Download was incomplete; content later in the source may be missing. Rendering was not attempted.".into());
        }
        if !source.truncated && matches!(extracted.quality, WebExtractionQualityV1::NeedsRendering)
        {
            if let Some(renderer) = self.renderer.as_ref().filter(|_| allow_render) {
                let rendered =
                    renderer.render(&source.final_url, MAXIMUM_WEB_DOCUMENT_BYTES, cancellation);
                check_cancelled(cancellation)?;
                match rendered {
                    Ok(snapshot) => {
                        parse_https_url(&snapshot.final_url)?;
                        let rendered_source = WebSourceV1 {
                            final_url: snapshot.final_url.clone(),
                            body: document::prefix(&snapshot.html, MAXIMUM_WEB_DOCUMENT_BYTES)
                                .into(),
                            content_type: "text/html".into(),
                            bytes_downloaded: 0,
                            truncated: snapshot.truncated
                                || snapshot.html.len() > MAXIMUM_WEB_DOCUMENT_BYTES,
                            warning: None,
                            title: None,
                        };
                        match extraction::extract(&rendered_source) {
                            Ok(candidate) if candidate.quality == WebExtractionQualityV1::Usable && !candidate.text.is_empty() => {
                                extracted = candidate;
                                metadata.final_url = snapshot.final_url;
                                metadata.method = format!("webview/{}", extracted.method);
                                metadata.snapshot_truncated = rendered_source.truncated;
                                metadata.snapshot_bytes = Some(rendered_source.body.len() as u64);
                                metadata.render_settled = Some(snapshot.settled);
                                if !snapshot.settled { metadata.warnings.push("Rendered DOM did not settle before the deadline.".into()); }
                                if rendered_source.truncated { metadata.warnings.push("Rendered HTML snapshot was truncated.".into()); }
                            },
                            _ => metadata.warnings.push("Rendering did not produce usable content; kept the HTTP extraction.".into()),
                        }
                    }
                    Err(error) => metadata.warnings.push(format!(
                        "Rendering failed; kept HTTP extraction: {}",
                        document::prefix(&error, 512)
                    )),
                }
            } else {
                metadata.warnings.push("This page appears to need JavaScript; native rendering is disabled or unavailable.".into());
            }
        }
        check_cancelled(cancellation)?;
        metadata.quality = extracted.quality;
        metadata.document_truncated = extracted.text.len() > MAXIMUM_WEB_DOCUMENT_BYTES;
        if metadata.document_truncated {
            metadata
                .warnings
                .push("Extracted document exceeded its retention limit.".into());
        }
        Ok(WebDocumentV1 {
            url,
            title: document::prefix(&extracted.title, 512).into(),
            text: document::prefix(&extracted.text, MAXIMUM_WEB_DOCUMENT_BYTES).into(),
            bytes_downloaded: source.bytes_downloaded,
            metadata,
        })
    }

    pub fn fetch_v1(
        &self,
        url: &str,
        maximum_download_bytes: usize,
        maximum_extract_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<WebFetchResultV1, WebToolError> {
        if maximum_extract_bytes == 0 || maximum_extract_bytes > MAXIMUM_EXTRACT_BYTES {
            return Err(WebToolError::InvalidBound);
        }
        let document = self.document_v1(url, maximum_download_bytes, false, cancellation)?;
        Ok(WebFetchResultV1 {
            url: document.url,
            title: document.title,
            preview_truncated: document.text.len() > maximum_extract_bytes,
            text: document::prefix(&document.text, maximum_extract_bytes).into(),
            bytes_downloaded: document.bytes_downloaded,
            metadata: document.metadata,
        })
    }

    /// Fetches and extracts multiple candidate pages independently. A broken
    /// page becomes an item-level error so one failed URL cannot erase useful
    /// evidence from the rest of the search result set.
    pub fn extract_v1(
        &self,
        urls: &[String],
        maximum_download_bytes: usize,
        maximum_extract_bytes: usize,
        requested_extract_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WebExtractPageV1>, WebToolError> {
        if urls.is_empty()
            || urls.len() > MAXIMUM_EXTRACT_URLS
            || requested_extract_bytes == 0
            || requested_extract_bytes > maximum_extract_bytes
        {
            return Err(WebToolError::InvalidBound);
        }
        let mut pages = Vec::with_capacity(urls.len());
        for url in urls {
            check_cancelled(cancellation)?;
            match self.fetch_v1(
                url,
                maximum_download_bytes,
                requested_extract_bytes,
                cancellation,
            ) {
                Ok(fetched) => pages.push(WebExtractPageV1 {
                    url: fetched.url,
                    title: fetched.title,
                    raw_content: fetched.text.clone(),
                    content: fetched.text,
                    bytes_downloaded: fetched.bytes_downloaded,
                    metadata: Some(fetched.metadata),
                    preview_truncated: fetched.preview_truncated,
                    fetched_at_epoch_ms: now_epoch_ms(),
                    error: None,
                }),
                Err(WebToolError::Cancelled) => return Err(WebToolError::Cancelled),
                Err(error) => pages.push(WebExtractPageV1 {
                    url: url.clone(),
                    title: String::new(),
                    content: String::new(),
                    raw_content: String::new(),
                    bytes_downloaded: 0,
                    metadata: None,
                    preview_truncated: false,
                    fetched_at_epoch_ms: now_epoch_ms(),
                    error: Some(error.to_string()),
                }),
            }
        }
        Ok(pages)
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn parse_https_url(url: &str) -> Result<String, WebToolError> {
    if url.len() > 4096 || url.contains('\0') {
        return Err(WebToolError::InvalidUrl);
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| WebToolError::InvalidUrl)?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(WebToolError::InvalidUrl);
    }
    Ok(parsed.to_string())
}

fn validate_query(query: &str) -> Result<(), WebToolError> {
    if query.trim().is_empty() || query.len() > MAXIMUM_QUERY_BYTES || query.contains('\0') {
        Err(WebToolError::InvalidQuery)
    } else {
        Ok(())
    }
}

struct ProductionWebTransport;

impl WebTransportPort for ProductionWebTransport {
    fn search(
        &self,
        query: &str,
        maximum_results: usize,
    ) -> Result<Vec<WebSearchResultV1>, String> {
        providers::search_keyless(query, maximum_results, REQUEST_TIMEOUT)
            .map_err(|error| error.message)
    }

    fn fetch(
        &self,
        url: &str,
        maximum_download_bytes: usize,
    ) -> Result<(String, String, u64), String> {
        let source =
            retrieval::retrieve(url, maximum_download_bytes, &CancellationToken::default())?;
        let extracted = extraction::extract(&source)?;
        Ok((extracted.title, extracted.text, source.bytes_downloaded))
    }

    fn fetch_document(
        &self,
        url: &str,
        maximum: usize,
        cancellation: &CancellationToken,
    ) -> Result<WebSourceV1, String> {
        retrieval::retrieve(url, maximum, cancellation)
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), WebToolError> {
    if cancellation.is_cancelled() {
        Err(WebToolError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebToolError {
    #[error("web query is invalid")]
    InvalidQuery,
    #[error("web URL must be an HTTPS address")]
    InvalidUrl,
    #[error("web resource bound is invalid")]
    InvalidBound,
    #[error("web request failed: {0}")]
    Transport(String),
    #[error("web request was cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FakeTransport {
        search_results: Vec<WebSearchResultV1>,
        fetched: (String, String, u64),
    }

    impl WebTransportPort for FakeTransport {
        fn search(
            &self,
            _query: &str,
            maximum_results: usize,
        ) -> Result<Vec<WebSearchResultV1>, String> {
            Ok(self
                .search_results
                .iter()
                .take(maximum_results)
                .cloned()
                .collect())
        }

        fn fetch(
            &self,
            _url: &str,
            _maximum_download_bytes: usize,
        ) -> Result<(String, String, u64), String> {
            Ok(self.fetched.clone())
        }
    }

    #[test]
    fn web_tools_enforce_bounds_cancellation_and_transport_errors() {
        let tools = WebTools::new(Arc::new(FakeTransport {
            search_results: vec![
                WebSearchResultV1 {
                    title: "One".into(),
                    url: "https://one.example".into(),
                    snippet: "first".into(),
                },
                WebSearchResultV1 {
                    title: "Two".into(),
                    url: "https://two.example".into(),
                    snippet: "second".into(),
                },
            ],
            fetched: ("Page title".into(), "plain body text".into(), 12),
        }));

        let results = tools
            .search_v1("test query", 1, &CancellationToken::default())
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "One");
        assert!(matches!(
            tools.search_v1("", 1, &CancellationToken::default()),
            Err(WebToolError::InvalidQuery)
        ));
        assert!(matches!(
            tools.search_v1("query", 0, &CancellationToken::default()),
            Err(WebToolError::InvalidBound)
        ));
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert!(matches!(
            tools.search_v1("query", 1, &cancelled),
            Err(WebToolError::Cancelled)
        ));

        let fetched = tools
            .fetch_v1(
                "https://example.com/page",
                4096,
                256,
                &CancellationToken::default(),
            )
            .expect("fetch");
        assert_eq!(fetched.title, "Page title");
        assert_eq!(fetched.text, "plain body text");
        assert_eq!(fetched.bytes_downloaded, 12);
        assert!(matches!(
            tools.fetch_v1(
                "http://insecure.example",
                4096,
                256,
                &CancellationToken::default()
            ),
            Err(WebToolError::InvalidUrl)
        ));
        assert!(matches!(
            tools.fetch_v1("https://example.com", 0, 256, &CancellationToken::default()),
            Err(WebToolError::InvalidBound)
        ));
    }

    #[test]
    fn web_fetch_truncates_extracted_text_to_the_configured_bound() {
        let tools = WebTools::new(Arc::new(FakeTransport {
            search_results: Vec::new(),
            fetched: ("Title".into(), "x".repeat(300), 300),
        }));
        let fetched = tools
            .fetch_v1(
                "https://example.com",
                4096,
                10,
                &CancellationToken::default(),
            )
            .expect("fetch");
        assert_eq!(fetched.bytes_downloaded, 300);
        assert!(fetched.text.len() <= 10, "{}", fetched.text.len());
    }

    struct PartialExtractTransport;

    impl WebTransportPort for PartialExtractTransport {
        fn search(
            &self,
            _query: &str,
            _maximum_results: usize,
        ) -> Result<Vec<WebSearchResultV1>, String> {
            Ok(Vec::new())
        }

        fn fetch(
            &self,
            url: &str,
            _maximum_download_bytes: usize,
        ) -> Result<(String, String, u64), String> {
            if url.contains("broken") {
                Err("fixture page failed".into())
            } else {
                Ok(("Live page".into(), "current page content".into(), 20))
            }
        }
    }

    #[test]
    fn web_extract_keeps_successful_pages_when_another_url_fails() {
        let tools = WebTools::new(Arc::new(PartialExtractTransport));
        let pages = tools
            .extract_v1(
                &[
                    "https://example.com/live".into(),
                    "https://broken.example/page".into(),
                ],
                4096,
                1024,
                1024,
                &CancellationToken::default(),
            )
            .expect("multi-page extraction");

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].content, "current page content");
        assert!(pages[0].error.is_none());
        assert_eq!(
            pages[1].error.as_deref(),
            Some("web request failed: fixture page failed")
        );
        assert!(pages[0].fetched_at_epoch_ms > 0);
    }

    #[test]
    #[ignore = "uses the configured public internet"]
    fn production_keyless_search_returns_ranked_results() {
        let tools = WebTools::production();
        let configuration = WebSearchConfigurationV1 {
            maximum_results: 3,
            maximum_retries: 0,
            cache_enabled: false,
            ..WebSearchConfigurationV1::default()
        };
        let outcome = tools
            .search_configured_v1(
                "deepseek-v4-flash pricing per million tokens",
                &configuration,
                None,
                &CancellationToken::default(),
            )
            .expect("public keyless search");
        assert!(!outcome.results.is_empty(), "{outcome:?}");
        assert!(
            outcome
                .results
                .iter()
                .all(|result| result.url.starts_with("http")),
            "{outcome:?}"
        );
    }
}
