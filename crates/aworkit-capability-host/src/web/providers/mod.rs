//! Built-in search-provider adapters.

mod brave;
mod deepseek;
mod duckduckgo;
mod exa;
mod firecrawl;
mod keenable;
mod keyless_mcp;
mod parallel;
mod searxng;
mod tavily;
mod xai;

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::super::WebSearchResultV1;
use super::{
    WebSearchBackendV1, WebSearchConfigurationV1, WebSearchProviderTierV1,
    WebSearchProviderUsageV1, search::SearchExecutorPort,
};

#[derive(Clone, Debug)]
pub(super) struct ProviderSearchOutcome {
    pub(super) results: Vec<WebSearchResultV1>,
    pub(super) usage: Option<WebSearchProviderUsageV1>,
}

impl ProviderSearchOutcome {
    pub(super) fn without_usage(results: Vec<WebSearchResultV1>) -> Self {
        Self {
            results,
            usage: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SearchProviderV1 {
    /// Virtual route which expands to the rotating anonymous-provider ring.
    Keyless,
    Duckduckgo,
    Searxng,
    Exa,
    ExaKeyless,
    Parallel,
    ParallelKeyless,
    Firecrawl,
    FirecrawlKeyless,
    Tavily,
    TavilyKeyless,
    Brave,
    Keenable,
    KeenableKeyless,
    Xai,
    Deepseek,
}

impl SearchProviderV1 {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Keyless => "keyless",
            Self::Duckduckgo => "duckduckgo",
            Self::Searxng => "searxng",
            Self::Exa => "exa",
            Self::ExaKeyless => "exa-keyless",
            Self::Parallel => "parallel",
            Self::ParallelKeyless => "parallel-keyless",
            Self::Firecrawl => "firecrawl",
            Self::FirecrawlKeyless => "firecrawl-keyless",
            Self::Tavily => "tavily",
            Self::TavilyKeyless => "tavily-keyless",
            Self::Brave => "brave",
            Self::Keenable => "keenable",
            Self::KeenableKeyless => "keenable-keyless",
            Self::Xai => "xai",
            Self::Deepseek => "deepseek",
        }
    }

    pub(super) fn is_keyless(self) -> bool {
        matches!(
            self,
            Self::Keyless
                | Self::Duckduckgo
                | Self::ExaKeyless
                | Self::ParallelKeyless
                | Self::FirecrawlKeyless
                | Self::TavilyKeyless
                | Self::KeenableKeyless
        )
    }
}

#[derive(Clone, Debug)]
pub(super) struct SearchFailure {
    pub(super) message: String,
    pub(super) retryable: bool,
    pub(super) usage: Option<WebSearchProviderUsageV1>,
}

impl SearchFailure {
    pub(super) fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: bounded_text(&message.into(), 1_024),
            retryable: true,
            usage: None,
        }
    }

    pub(super) fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: bounded_text(&message.into(), 1_024),
            retryable: false,
            usage: None,
        }
    }

    pub(super) fn with_usage(mut self, usage: Option<WebSearchProviderUsageV1>) -> Self {
        self.usage = usage;
        self
    }
}

pub(super) struct ProductionSearchExecutor;

pub(super) fn search_keyless(
    query: &str,
    maximum_results: usize,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    duckduckgo::search(query, maximum_results, timeout)
}

/// Hermes-compatible anonymous ring. Explicit free-provider selection pins
/// the first vendor; automatic/keyless mode advances a process-local cursor.
pub(super) fn keyless_order(configuration: &WebSearchConfigurationV1) -> Vec<SearchProviderV1> {
    const RING: [SearchProviderV1; 4] = [
        SearchProviderV1::ExaKeyless,
        SearchProviderV1::ParallelKeyless,
        SearchProviderV1::FirecrawlKeyless,
        SearchProviderV1::KeenableKeyless,
    ];
    let pinned = match configuration.backend {
        WebSearchBackendV1::Exa => Some(0),
        WebSearchBackendV1::Parallel => Some(1),
        WebSearchBackendV1::Firecrawl => Some(2),
        WebSearchBackendV1::Keenable => Some(3),
        _ => None,
    };
    let start =
        pinned.unwrap_or_else(|| keyless_cursor().fetch_add(1, Ordering::Relaxed) % RING.len());
    let mut order = (0..RING.len())
        .map(|offset| RING[(start + offset) % RING.len()])
        .filter(|candidate| {
            !(configuration.provider_tier == WebSearchProviderTierV1::Paid
                && candidate_matches_backend(*candidate, configuration.backend))
        })
        .collect::<Vec<_>>();
    // DDG has no anonymous vendor quota and provides a final local fallback
    // when all public ring services are unavailable.
    order.push(SearchProviderV1::Duckduckgo);
    order
}

fn candidate_matches_backend(candidate: SearchProviderV1, backend: WebSearchBackendV1) -> bool {
    matches!(
        (candidate, backend),
        (SearchProviderV1::ExaKeyless, WebSearchBackendV1::Exa)
            | (
                SearchProviderV1::ParallelKeyless,
                WebSearchBackendV1::Parallel
            )
            | (
                SearchProviderV1::FirecrawlKeyless,
                WebSearchBackendV1::Firecrawl
            )
            | (
                SearchProviderV1::KeenableKeyless,
                WebSearchBackendV1::Keenable
            )
    )
}

fn keyless_cursor() -> &'static AtomicUsize {
    static CURSOR: OnceLock<AtomicUsize> = OnceLock::new();
    CURSOR.get_or_init(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as usize;
        AtomicUsize::new(now ^ std::process::id() as usize)
    })
}

impl SearchExecutorPort for ProductionSearchExecutor {
    fn search(
        &self,
        provider: SearchProviderV1,
        configuration: &WebSearchConfigurationV1,
        query: &str,
        maximum_results: usize,
        api_key: Option<&str>,
    ) -> Result<ProviderSearchOutcome, SearchFailure> {
        let timeout = Duration::from_secs(configuration.request_timeout_seconds);
        let override_url = configuration.provider_base_url.trim();
        let require_key = || {
            api_key
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    SearchFailure::terminal(format!("{} API key is missing", provider.as_str()))
                })
        };
        let results = match provider {
            SearchProviderV1::Keyless => Err(SearchFailure::terminal(
                "internal web-search error: unresolved keyless route",
            )),
            SearchProviderV1::Duckduckgo => duckduckgo::search(query, maximum_results, timeout),
            SearchProviderV1::Searxng => searxng::search(
                &configuration.searxng_base_url,
                query,
                maximum_results,
                timeout,
            ),
            SearchProviderV1::Exa => exa::search(
                override_url,
                query,
                maximum_results,
                require_key()?,
                timeout,
            ),
            SearchProviderV1::ExaKeyless => {
                keyless_mcp::search_exa(query, maximum_results, timeout)
            }
            SearchProviderV1::Parallel => parallel::search(
                override_url,
                &configuration.parallel_search_mode,
                query,
                maximum_results,
                require_key()?,
                timeout,
            ),
            SearchProviderV1::ParallelKeyless => {
                keyless_mcp::search_parallel(query, maximum_results, timeout)
            }
            SearchProviderV1::Firecrawl => firecrawl::search(
                override_url,
                query,
                maximum_results,
                Some(require_key()?),
                timeout,
            ),
            SearchProviderV1::FirecrawlKeyless => firecrawl::search(
                if configuration.backend == WebSearchBackendV1::Firecrawl {
                    override_url
                } else {
                    ""
                },
                query,
                maximum_results,
                None,
                timeout,
            ),
            SearchProviderV1::Tavily => tavily::search(
                override_url,
                query,
                maximum_results,
                Some(require_key()?),
                timeout,
            ),
            SearchProviderV1::TavilyKeyless => {
                tavily::search(override_url, query, maximum_results, None, timeout)
            }
            SearchProviderV1::Brave => brave::search(
                override_url,
                query,
                maximum_results,
                require_key()?,
                timeout,
            ),
            SearchProviderV1::Keenable => keenable::search(
                override_url,
                query,
                maximum_results,
                Some(require_key()?),
                timeout,
            ),
            SearchProviderV1::KeenableKeyless => keenable::search(
                if configuration.backend == WebSearchBackendV1::Keenable {
                    override_url
                } else {
                    ""
                },
                query,
                maximum_results,
                None,
                timeout,
            ),
            SearchProviderV1::Xai => xai::search(
                override_url,
                &configuration.xai_model,
                &configuration.xai_allowed_domains,
                &configuration.xai_excluded_domains,
                query,
                maximum_results,
                require_key()?,
                timeout,
            ),
            SearchProviderV1::Deepseek => {
                return deepseek::search(
                    &configuration.deepseek_base_url,
                    &configuration.deepseek_model,
                    configuration.deepseek_maximum_output_tokens,
                    query,
                    maximum_results,
                    require_key()?,
                    timeout,
                );
            }
        }?;
        Ok(ProviderSearchOutcome::without_usage(results))
    }
}

pub(super) fn provider_base_url<'a>(override_url: &'a str, default: &'a str) -> &'a str {
    if override_url.trim().is_empty() {
        default
    } else {
        override_url.trim()
    }
}

pub(super) fn endpoint(base_url: &str, path: &str) -> Result<reqwest::Url, SearchFailure> {
    let normalized = format!("{}/", base_url.trim().trim_end_matches('/'));
    reqwest::Url::parse(&normalized)
        .and_then(|base| base.join(path.trim_start_matches('/')))
        .map_err(|_| SearchFailure::terminal("web-search provider base URL is invalid"))
}

pub(super) fn client(timeout: Duration) -> Result<reqwest::blocking::Client, SearchFailure> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        // Provider credentials must never follow a redirect to a different
        // origin. Keyless endpoints are also expected to be canonical.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Aworkit/1.0 web-search")
        .build()
        .map_err(|error| SearchFailure::terminal(format!("web client unavailable: {error}")))
}

pub(super) fn bounded_text(value: &str, maximum_bytes: usize) -> String {
    let mut value = value.trim().to_owned();
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push('…');
    value
}

pub(super) fn validated_result(title: &str, url: &str, snippet: &str) -> Option<WebSearchResultV1> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    let title = bounded_text(&collapse_whitespace(title), 512);
    if title.is_empty() {
        return None;
    }
    Some(WebSearchResultV1 {
        title,
        url: bounded_text(parsed.as_str(), 4_096),
        snippet: bounded_text(&collapse_whitespace(snippet), 4_096),
    })
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn http_failure(provider: &str, status: reqwest::StatusCode) -> SearchFailure {
    let message = format!("{provider} search failed with HTTP {status}");
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        SearchFailure::retryable(message)
    } else {
        SearchFailure::terminal(message)
    }
}
