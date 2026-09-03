//! Provider-neutral web-search routing, bounded retries, failover, and cache.

use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::freshness::{
    FreshnessLedger, FreshnessPolicy, WebSearchFreshnessModeV1, WebSearchFreshnessV1,
};
use super::providers::{ProductionSearchExecutor, SearchFailure, SearchProviderV1};
use super::{CancellationToken, WebSearchResultV1, WebToolError, check_cancelled};

const MAXIMUM_CACHE_ENTRIES: usize = 256;
// Leave enough headroom inside the desktop's 512 KiB tool-result envelope for
// the query, routing evidence, JSON framing, and provider-attempt diagnostics.
const MAXIMUM_RESULTS_PAYLOAD_BYTES: usize = 384 * 1024;
const FLIGHT_WAIT_SLICE: Duration = Duration::from_millis(50);

/// Search backend selected in the frozen tool settings.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchBackendV1 {
    /// Prefer an explicitly identified credential provider, then SearXNG,
    /// then the rotating anonymous provider ring.
    Automatic,
    /// Rotate through the anonymous Exa, Parallel, Firecrawl, and Keenable
    /// services, with DuckDuckGo as the final no-account fallback.
    Keyless,
    /// Keyless DuckDuckGo HTML/Lite search.
    Duckduckgo,
    /// User-operated or public SearXNG JSON endpoint.
    Searxng,
    /// Exa neural search (anonymous MCP or API-key tier).
    Exa,
    /// Parallel Search API (anonymous MCP or API-key tier).
    Parallel,
    /// Firecrawl web search (anonymous or API-key tier).
    Firecrawl,
    /// Tavily web search (opt-in anonymous or API-key tier).
    Tavily,
    /// Brave Search API.
    Brave,
    /// Keenable search (anonymous or API-key tier).
    Keenable,
    /// xAI Responses API with server-side web search.
    Xai,
    /// DeepSeek Responses API with its server-side paid web-search tool.
    Deepseek,
}

impl WebSearchBackendV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Keyless => "keyless",
            Self::Duckduckgo => "duckduckgo",
            Self::Searxng => "searxng",
            Self::Exa => "exa",
            Self::Parallel => "parallel",
            Self::Firecrawl => "firecrawl",
            Self::Tavily => "tavily",
            Self::Brave => "brave",
            Self::Keenable => "keenable",
            Self::Xai => "xai",
            Self::Deepseek => "deepseek",
        }
    }

    fn is_paid_provider(self) -> bool {
        matches!(
            self,
            Self::Exa
                | Self::Parallel
                | Self::Firecrawl
                | Self::Tavily
                | Self::Brave
                | Self::Keenable
                | Self::Xai
                | Self::Deepseek
        )
    }
}

/// Selects a provider's anonymous or API-key route where both are available.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProviderTierV1 {
    /// Use the API-key route when a credential is bound, otherwise anonymous.
    Automatic,
    /// Force the provider's public anonymous route.
    Free,
    /// Require and use an API-key credential.
    Paid,
}

impl WebSearchProviderTierV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Free => "free",
            Self::Paid => "paid",
        }
    }
}

/// Complete, secret-free settings frozen with one Chat/Run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchConfigurationV1 {
    pub backend: WebSearchBackendV1,
    pub credential_backend: WebSearchBackendV1,
    pub provider_tier: WebSearchProviderTierV1,
    pub maximum_results: usize,
    pub request_timeout_seconds: u64,
    pub maximum_retries: u32,
    pub keyless_fallback: bool,
    pub keyless_rescue: bool,
    pub cache_enabled: bool,
    pub cache_ttl_minutes: u64,
    #[serde(default = "default_true")]
    pub freshness_validation: bool,
    #[serde(default = "default_freshness_maximum_age_days")]
    pub freshness_maximum_age_days: u64,
    #[serde(default = "default_true")]
    pub freshness_bypass_cache: bool,
    pub searxng_base_url: String,
    pub provider_base_url: String,
    pub parallel_search_mode: String,
    pub xai_model: String,
    pub xai_allowed_domains: Vec<String>,
    pub xai_excluded_domains: Vec<String>,
    pub deepseek_base_url: String,
    pub deepseek_model: String,
    pub deepseek_maximum_output_tokens: u32,
}

impl Default for WebSearchConfigurationV1 {
    fn default() -> Self {
        Self {
            backend: WebSearchBackendV1::Automatic,
            credential_backend: WebSearchBackendV1::Deepseek,
            provider_tier: WebSearchProviderTierV1::Automatic,
            maximum_results: 10,
            request_timeout_seconds: 30,
            maximum_retries: 1,
            keyless_fallback: true,
            keyless_rescue: true,
            cache_enabled: true,
            cache_ttl_minutes: 20,
            freshness_validation: true,
            freshness_maximum_age_days: default_freshness_maximum_age_days(),
            freshness_bypass_cache: true,
            searxng_base_url: String::new(),
            provider_base_url: String::new(),
            parallel_search_mode: "agentic".into(),
            xai_model: "grok-build-0.1".into(),
            xai_allowed_domains: Vec::new(),
            xai_excluded_domains: Vec::new(),
            deepseek_base_url: "https://api.deepseek.com".into(),
            deepseek_model: "deepseek-v4-flash".into(),
            deepseek_maximum_output_tokens: 4_096,
        }
    }
}

/// One provider attempt exposed as non-secret execution evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchAttemptV1 {
    pub backend: String,
    pub attempt: u32,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Settled search result with routing and cache evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchOutcomeV1 {
    pub query: String,
    pub backend: String,
    pub results: Vec<WebSearchResultV1>,
    pub cached: bool,
    pub coalesced: bool,
    pub attempts: Vec<WebSearchAttemptV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rescued_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_error: Option<String>,
    pub freshness: WebSearchFreshnessV1,
}

pub(super) trait SearchExecutorPort: Send + Sync {
    fn search(
        &self,
        provider: SearchProviderV1,
        configuration: &WebSearchConfigurationV1,
        query: &str,
        maximum_results: usize,
        api_key: Option<&str>,
    ) -> Result<Vec<WebSearchResultV1>, SearchFailure>;
}

#[derive(Clone)]
pub(super) struct WebSearchRuntime {
    executor: Arc<dyn SearchExecutorPort>,
    state: Arc<Mutex<SearchState>>,
}

#[derive(Default)]
struct SearchState {
    cache: HashMap<SearchCacheKey, CacheEntry>,
    flights: HashMap<SearchCacheKey, Arc<SearchFlight>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SearchCacheKey {
    provider: SearchProviderV1,
    provider_configuration: String,
    query: String,
    maximum_results: usize,
    freshness_required: bool,
}

struct CacheEntry {
    inserted: Instant,
    outcome: WebSearchOutcomeV1,
}

#[derive(Default)]
struct SearchFlight {
    result: Mutex<Option<Result<WebSearchOutcomeV1, String>>>,
    ready: Condvar,
}

impl WebSearchRuntime {
    pub(super) fn production() -> Self {
        Self::new(Arc::new(ProductionSearchExecutor))
    }

    pub(super) fn new(executor: Arc<dyn SearchExecutorPort>) -> Self {
        Self {
            executor,
            state: Arc::new(Mutex::new(SearchState::default())),
        }
    }

    pub(super) fn search(
        &self,
        query: &str,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<WebSearchOutcomeV1, WebToolError> {
        self.search_with_freshness(
            query,
            configuration,
            api_key,
            WebSearchFreshnessModeV1::Auto,
            cancellation,
        )
    }

    pub(super) fn search_with_freshness(
        &self,
        query: &str,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        freshness_mode: WebSearchFreshnessModeV1,
        cancellation: &CancellationToken,
    ) -> Result<WebSearchOutcomeV1, WebToolError> {
        configuration.validate()?;
        check_cancelled(cancellation)?;
        let freshness_policy = FreshnessPolicy::resolve(
            query,
            freshness_mode,
            configuration.freshness_validation,
            configuration.freshness_maximum_age_days,
        );
        let cache_allowed = configuration.cache_enabled
            && !(freshness_policy.required() && configuration.freshness_bypass_cache);
        let provider = select_provider(configuration, api_key)?;
        let requested_results = configuration.maximum_results;
        let fetch_results = bucket_limit(requested_results);
        let key = SearchCacheKey {
            provider,
            provider_configuration: provider_configuration_key(provider, configuration),
            query: normalize_query(query),
            maximum_results: fetch_results,
            freshness_required: freshness_policy.required(),
        };
        let ttl = Duration::from_secs(configuration.cache_ttl_minutes.saturating_mul(60));

        let (flight, owner) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| WebToolError::Transport("web-search cache lock poisoned".into()))?;
            prune_expired(&mut state, ttl);
            if cache_allowed && let Some(entry) = state.cache.get(&key) {
                let mut cached = entry.outcome.clone();
                cached.cached = true;
                return Ok(slice_outcome(cached, requested_results));
            }
            if let Some(flight) = state.flights.get(&key) {
                (flight.clone(), false)
            } else {
                let flight = Arc::new(SearchFlight::default());
                state.flights.insert(key.clone(), flight.clone());
                (flight, true)
            }
        };

        if !owner {
            return wait_for_flight(&flight, requested_results, cancellation);
        }

        let result = self
            .execute_uncached(
                provider,
                query,
                fetch_results,
                configuration,
                api_key,
                cancellation,
                &freshness_policy,
            )
            .map_err(|error| error.to_string());
        {
            let mut slot = flight
                .result
                .lock()
                .map_err(|_| WebToolError::Transport("web-search flight lock poisoned".into()))?;
            *slot = Some(result.clone());
            flight.ready.notify_all();
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebToolError::Transport("web-search cache lock poisoned".into()))?;
        state.flights.remove(&key);
        if cache_allowed
            && let Ok(outcome) = &result
            && outcome.rescued_from.is_none()
        {
            make_cache_room(&mut state);
            state.cache.insert(
                key,
                CacheEntry {
                    inserted: Instant::now(),
                    outcome: outcome.clone(),
                },
            );
        }
        result
            .map(|outcome| slice_outcome(outcome, requested_results))
            .map_err(WebToolError::Transport)
    }

    fn execute_uncached(
        &self,
        provider: SearchProviderV1,
        query: &str,
        fetch_results: usize,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        cancellation: &CancellationToken,
        freshness_policy: &FreshnessPolicy,
    ) -> Result<WebSearchOutcomeV1, WebToolError> {
        let mut attempts = Vec::new();
        let mut freshness = FreshnessLedger::default();
        match self.try_route(
            provider,
            query,
            fetch_results,
            configuration,
            api_key,
            cancellation,
            &mut attempts,
            freshness_policy,
            &mut freshness,
        ) {
            Ok((served_by, results)) => Ok(WebSearchOutcomeV1 {
                query: query.to_owned(),
                backend: served_by.as_str().into(),
                results,
                cached: false,
                coalesced: false,
                attempts,
                rescued_from: None,
                backend_error: None,
                freshness: freshness.finish(freshness_policy),
            }),
            Err(primary_error)
                if !provider.is_keyless()
                    && configuration.keyless_fallback
                    && configuration.keyless_rescue =>
            {
                let primary_message = primary_error.message;
                let (served_by, results) = self
                    .try_route(
                        SearchProviderV1::Keyless,
                        query,
                        fetch_results,
                        configuration,
                        None,
                        cancellation,
                        &mut attempts,
                        freshness_policy,
                        &mut freshness,
                    )
                    .map_err(|error| WebToolError::Transport(error.message))?;
                Ok(WebSearchOutcomeV1 {
                    query: query.to_owned(),
                    backend: served_by.as_str().into(),
                    results,
                    cached: false,
                    coalesced: false,
                    attempts,
                    rescued_from: Some(provider.as_str().into()),
                    backend_error: Some(primary_message),
                    freshness: freshness.finish(freshness_policy),
                })
            }
            Err(error) => Err(WebToolError::Transport(error.message)),
        }
    }

    fn try_route(
        &self,
        provider: SearchProviderV1,
        query: &str,
        maximum_results: usize,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        cancellation: &CancellationToken,
        attempts: &mut Vec<WebSearchAttemptV1>,
        freshness_policy: &FreshnessPolicy,
        freshness: &mut FreshnessLedger,
    ) -> Result<(SearchProviderV1, Vec<WebSearchResultV1>), SearchFailure> {
        if provider != SearchProviderV1::Keyless {
            let results = self.try_provider(
                provider,
                query,
                maximum_results,
                configuration,
                api_key,
                cancellation,
                attempts,
            )?;
            let evaluation = freshness.evaluate(freshness_policy, results);
            if evaluation.rejected_all {
                mark_stale_attempt(attempts);
                return Err(SearchFailure::terminal(format!(
                    "{} returned only stale or contradictory dated results",
                    provider.as_str()
                )));
            }
            return Ok((provider, evaluation.results));
        }

        let mut last_error = SearchFailure::retryable("all keyless providers failed");
        let mut last_empty_provider = None;
        for candidate in super::providers::keyless_order(configuration) {
            match self.try_provider(
                candidate,
                query,
                maximum_results,
                configuration,
                None,
                cancellation,
                attempts,
            ) {
                Ok(results) if !results.is_empty() => {
                    let evaluation = freshness.evaluate(freshness_policy, results);
                    if evaluation.rejected_all {
                        mark_stale_attempt(attempts);
                        last_error = SearchFailure::retryable(format!(
                            "{} returned only stale or contradictory dated results",
                            candidate.as_str()
                        ));
                        continue;
                    }
                    return Ok((candidate, evaluation.results));
                }
                Ok(results) => {
                    // Anonymous services occasionally return a syntactically valid but
                    // empty response when their public quota or anti-bot edge is active.
                    // Treat that as a ring miss instead of recreating the old silent
                    // `results: []` failure. If every provider genuinely has no matches,
                    // the final empty response is still returned as a successful search.
                    if let Some(attempt) = attempts.last_mut() {
                        attempt.status = "empty".into();
                        attempt.error = Some(
                            "anonymous provider returned zero results; trying the next provider"
                                .into(),
                        );
                    }
                    debug_assert!(results.is_empty());
                    last_empty_provider = Some(candidate);
                }
                Err(error) => {
                    // The public ring is deliberately best-effort. A stale endpoint,
                    // temporary authentication gate, quota response, or parse change at
                    // one vendor must not prevent later anonymous providers from running.
                    last_error = SearchFailure::retryable(error.message);
                }
            }
        }
        if let Some(provider) = last_empty_provider {
            return Ok((provider, Vec::new()));
        }
        Err(SearchFailure::retryable(format!(
            "all anonymous web-search providers failed; last error: {}",
            last_error.message
        )))
    }

    fn try_provider(
        &self,
        provider: SearchProviderV1,
        query: &str,
        maximum_results: usize,
        configuration: &WebSearchConfigurationV1,
        api_key: Option<&str>,
        cancellation: &CancellationToken,
        attempts: &mut Vec<WebSearchAttemptV1>,
    ) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
        debug_assert_ne!(provider, SearchProviderV1::Keyless);
        let maximum_attempts = configuration.maximum_retries.saturating_add(1);
        for attempt in 1..=maximum_attempts {
            if let Err(error) = check_cancelled(cancellation) {
                return Err(SearchFailure::terminal(error.to_string()));
            }
            match self
                .executor
                .search(provider, configuration, query, maximum_results, api_key)
            {
                Ok(mut results) => {
                    results.truncate(maximum_results);
                    bound_results_payload(&mut results);
                    attempts.push(WebSearchAttemptV1 {
                        backend: provider.as_str().into(),
                        attempt,
                        status: "completed".into(),
                        error: None,
                    });
                    return Ok(results);
                }
                Err(error) => {
                    attempts.push(WebSearchAttemptV1 {
                        backend: provider.as_str().into(),
                        attempt,
                        status: "failed".into(),
                        error: Some(error.message.clone()),
                    });
                    if !error.retryable || attempt == maximum_attempts {
                        return Err(error);
                    }
                }
            }
        }
        Err(SearchFailure::terminal("web-search retry loop exhausted"))
    }
}

impl WebSearchConfigurationV1 {
    pub fn validate(&self) -> Result<(), WebToolError> {
        if self.maximum_results == 0
            || self.maximum_results > super::MAXIMUM_SEARCH_RESULTS
            || !(5..=120).contains(&self.request_timeout_seconds)
            || self.maximum_retries > 3
            || !(1..=1_440).contains(&self.cache_ttl_minutes)
            || !(1..=365).contains(&self.freshness_maximum_age_days)
            || !(256..=16_384).contains(&self.deepseek_maximum_output_tokens)
            || !matches!(
                self.parallel_search_mode.as_str(),
                "fast" | "one-shot" | "agentic"
            )
            || self.xai_model.trim().is_empty()
            || self.xai_model.len() > 256
            || self.deepseek_model.trim().is_empty()
            || self.deepseek_model.len() > 256
        {
            return Err(WebToolError::InvalidBound);
        }
        if !self.credential_backend.is_paid_provider() {
            return Err(WebToolError::InvalidBound);
        }
        if self.backend == WebSearchBackendV1::Automatic
            && self.provider_tier != WebSearchProviderTierV1::Automatic
        {
            return Err(WebToolError::InvalidBound);
        }
        if self.xai_allowed_domains.len() > 5
            || self.xai_excluded_domains.len() > 5
            || (!self.xai_allowed_domains.is_empty() && !self.xai_excluded_domains.is_empty())
            || self
                .xai_allowed_domains
                .iter()
                .chain(self.xai_excluded_domains.iter())
                .any(|domain| !valid_domain_filter(domain))
        {
            return Err(WebToolError::InvalidBound);
        }
        if matches!(
            self.backend,
            WebSearchBackendV1::Brave | WebSearchBackendV1::Xai | WebSearchBackendV1::Deepseek
        ) && self.provider_tier == WebSearchProviderTierV1::Free
        {
            return Err(WebToolError::InvalidBound);
        }
        if matches!(
            self.backend,
            WebSearchBackendV1::Keyless
                | WebSearchBackendV1::Duckduckgo
                | WebSearchBackendV1::Searxng
        ) && self.provider_tier != WebSearchProviderTierV1::Automatic
        {
            return Err(WebToolError::InvalidBound);
        }
        if self.keyless_rescue && !self.keyless_fallback {
            return Err(WebToolError::InvalidBound);
        }
        validate_optional_search_url(&self.searxng_base_url, true)?;
        validate_optional_search_url(&self.provider_base_url, true)?;
        validate_optional_search_url(&self.deepseek_base_url, false)?;
        Ok(())
    }
}

fn mark_stale_attempt(attempts: &mut [WebSearchAttemptV1]) {
    if let Some(attempt) = attempts.last_mut() {
        attempt.status = "stale".into();
        attempt.error = Some(
            "provider results contained only stale or contradictory date signals; trying another route"
                .into(),
        );
    }
}

const fn default_true() -> bool {
    true
}

const fn default_freshness_maximum_age_days() -> u64 {
    45
}

fn select_provider(
    configuration: &WebSearchConfigurationV1,
    api_key: Option<&str>,
) -> Result<SearchProviderV1, WebToolError> {
    let has_key = api_key.is_some_and(|value| !value.trim().is_empty());
    match configuration.backend {
        WebSearchBackendV1::Automatic if has_key => paid_provider(configuration.credential_backend),
        WebSearchBackendV1::Automatic if !configuration.searxng_base_url.trim().is_empty() => {
            Ok(SearchProviderV1::Searxng)
        }
        WebSearchBackendV1::Automatic if configuration.keyless_fallback => {
            Ok(SearchProviderV1::Keyless)
        }
        WebSearchBackendV1::Automatic => Err(WebToolError::Transport(
            "no configured web-search provider and keyless fallback is disabled".into(),
        )),
        WebSearchBackendV1::Keyless if configuration.keyless_fallback => {
            Ok(SearchProviderV1::Keyless)
        }
        WebSearchBackendV1::Keyless => Err(WebToolError::Transport(
            "keyless web-search routing is disabled".into(),
        )),
        WebSearchBackendV1::Duckduckgo => Ok(SearchProviderV1::Duckduckgo),
        WebSearchBackendV1::Searxng if configuration.searxng_base_url.trim().is_empty() => Err(
            WebToolError::Transport("SearXNG requires a configured base URL".into()),
        ),
        WebSearchBackendV1::Searxng => Ok(SearchProviderV1::Searxng),
        backend @ (WebSearchBackendV1::Exa
        | WebSearchBackendV1::Parallel
        | WebSearchBackendV1::Firecrawl
        | WebSearchBackendV1::Keenable) => match configuration.provider_tier {
            WebSearchProviderTierV1::Paid if !has_key => Err(WebToolError::Transport(format!(
                "{} paid search requires the tool's api_key credential binding",
                backend.as_str()
            ))),
            WebSearchProviderTierV1::Paid => paid_provider(backend),
            WebSearchProviderTierV1::Free if configuration.keyless_fallback => {
                Ok(SearchProviderV1::Keyless)
            }
            WebSearchProviderTierV1::Automatic if has_key => paid_provider(backend),
            WebSearchProviderTierV1::Automatic | WebSearchProviderTierV1::Free
                if configuration.keyless_fallback =>
            {
                Ok(SearchProviderV1::Keyless)
            }
            _ => Err(WebToolError::Transport(format!(
                "{} has no API key and keyless routing is disabled",
                backend.as_str()
            ))),
        },
        WebSearchBackendV1::Tavily => match configuration.provider_tier {
            WebSearchProviderTierV1::Paid if !has_key => Err(WebToolError::Transport(
                "Tavily paid search requires the tool's api_key credential binding".into(),
            )),
            WebSearchProviderTierV1::Paid => Ok(SearchProviderV1::Tavily),
            WebSearchProviderTierV1::Free if configuration.keyless_fallback => {
                Ok(SearchProviderV1::TavilyKeyless)
            }
            WebSearchProviderTierV1::Automatic if has_key => Ok(SearchProviderV1::Tavily),
            WebSearchProviderTierV1::Automatic | WebSearchProviderTierV1::Free
                if configuration.keyless_fallback =>
            {
                Ok(SearchProviderV1::TavilyKeyless)
            }
            _ => Err(WebToolError::Transport(
                "Tavily has no API key and keyless routing is disabled".into(),
            )),
        },
        backend @ (WebSearchBackendV1::Brave
        | WebSearchBackendV1::Xai
        | WebSearchBackendV1::Deepseek)
            if !has_key =>
        {
            Err(WebToolError::Transport(format!(
                "{} search requires the tool's api_key credential binding",
                backend.as_str()
            )))
        }
        backend @ (WebSearchBackendV1::Brave
        | WebSearchBackendV1::Xai
        | WebSearchBackendV1::Deepseek) => paid_provider(backend),
    }
}

fn paid_provider(backend: WebSearchBackendV1) -> Result<SearchProviderV1, WebToolError> {
    match backend {
        WebSearchBackendV1::Exa => Ok(SearchProviderV1::Exa),
        WebSearchBackendV1::Parallel => Ok(SearchProviderV1::Parallel),
        WebSearchBackendV1::Firecrawl => Ok(SearchProviderV1::Firecrawl),
        WebSearchBackendV1::Tavily => Ok(SearchProviderV1::Tavily),
        WebSearchBackendV1::Brave => Ok(SearchProviderV1::Brave),
        WebSearchBackendV1::Keenable => Ok(SearchProviderV1::Keenable),
        WebSearchBackendV1::Xai => Ok(SearchProviderV1::Xai),
        WebSearchBackendV1::Deepseek => Ok(SearchProviderV1::Deepseek),
        _ => Err(WebToolError::InvalidBound),
    }
}

fn wait_for_flight(
    flight: &SearchFlight,
    requested_results: usize,
    cancellation: &CancellationToken,
) -> Result<WebSearchOutcomeV1, WebToolError> {
    let mut result = flight
        .result
        .lock()
        .map_err(|_| WebToolError::Transport("web-search flight lock poisoned".into()))?;
    loop {
        check_cancelled(cancellation)?;
        if let Some(settled) = result.as_ref() {
            let mut outcome = settled.clone().map_err(WebToolError::Transport)?;
            outcome.coalesced = true;
            return Ok(slice_outcome(outcome, requested_results));
        }
        let (next, _) = flight
            .ready
            .wait_timeout(result, FLIGHT_WAIT_SLICE)
            .map_err(|_| WebToolError::Transport("web-search flight lock poisoned".into()))?;
        result = next;
    }
}

fn prune_expired(state: &mut SearchState, ttl: Duration) {
    state
        .cache
        .retain(|_, entry| entry.inserted.elapsed() <= ttl);
}

fn make_cache_room(state: &mut SearchState) {
    if state.cache.len() < MAXIMUM_CACHE_ENTRIES {
        return;
    }
    if let Some(oldest) = state
        .cache
        .iter()
        .min_by_key(|(_, entry)| entry.inserted)
        .map(|(key, _)| key.clone())
    {
        state.cache.remove(&oldest);
    }
}

fn normalize_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn bucket_limit(limit: usize) -> usize {
    [10, 20, 50, 100]
        .into_iter()
        .find(|bucket| limit <= *bucket)
        .unwrap_or(super::MAXIMUM_SEARCH_RESULTS)
}

fn slice_outcome(mut outcome: WebSearchOutcomeV1, limit: usize) -> WebSearchOutcomeV1 {
    outcome.results.truncate(limit);
    outcome
}

fn bound_results_payload(results: &mut Vec<WebSearchResultV1>) {
    let mut retained = Vec::with_capacity(results.len());
    let mut serialized_bytes = 2_usize; // JSON array brackets.
    for result in results.drain(..) {
        let Ok(encoded) = serde_json::to_vec(&result) else {
            continue;
        };
        let separator = usize::from(!retained.is_empty());
        if serialized_bytes
            .saturating_add(separator)
            .saturating_add(encoded.len())
            > MAXIMUM_RESULTS_PAYLOAD_BYTES
        {
            break;
        }
        serialized_bytes += separator + encoded.len();
        retained.push(result);
    }
    *results = retained;
}

fn provider_configuration_key(
    provider: SearchProviderV1,
    configuration: &WebSearchConfigurationV1,
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        provider.as_str(),
        configuration.backend.as_str(),
        configuration.provider_tier.as_str(),
        configuration.searxng_base_url.trim().to_lowercase(),
        configuration.provider_base_url.trim().to_lowercase(),
        configuration.parallel_search_mode,
        configuration.xai_model,
        configuration.xai_allowed_domains.join(","),
        configuration.xai_excluded_domains.join(","),
        configuration.freshness_validation,
        configuration.freshness_maximum_age_days,
        configuration.freshness_bypass_cache,
        format_args!(
            "{}|{}|{}",
            configuration.deepseek_base_url.trim().to_lowercase(),
            configuration.deepseek_model,
            configuration.deepseek_maximum_output_tokens
        )
    )
}

fn valid_domain_filter(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 253
        && !trimmed.contains(['/', '\\', '\t', '\n', '\r', ' '])
        && trimmed.split('.').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 63
                && !segment.starts_with('-')
                && !segment.ends_with('-')
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn validate_optional_search_url(
    value: &str,
    allow_loopback_http: bool,
) -> Result<(), WebToolError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    if value.len() > 4_096 || value.contains('\0') {
        return Err(WebToolError::InvalidUrl);
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| WebToolError::InvalidUrl)?;
    let host = parsed.host_str().ok_or(WebToolError::InvalidUrl)?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(WebToolError::InvalidUrl);
    }
    let secure = parsed.scheme() == "https";
    let loopback_http = allow_loopback_http
        && parsed.scheme() == "http"
        && matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1");
    if !secure && !loopback_http {
        return Err(WebToolError::InvalidUrl);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;

    struct FakeExecutor {
        calls: AtomicUsize,
        fail_deepseek: bool,
    }

    impl SearchExecutorPort for FakeExecutor {
        fn search(
            &self,
            provider: SearchProviderV1,
            _configuration: &WebSearchConfigurationV1,
            _query: &str,
            _maximum_results: usize,
            _api_key: Option<&str>,
        ) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if provider == SearchProviderV1::Deepseek && self.fail_deepseek {
                return Err(SearchFailure::terminal("DeepSeek authentication rejected"));
            }
            Ok(vec![WebSearchResultV1 {
                title: provider.as_str().into(),
                url: "https://example.com/result".into(),
                snippet: "result".into(),
            }])
        }
    }

    #[test]
    fn automatic_selection_uses_keyless_search_without_credentials() {
        let executor = Arc::new(FakeExecutor {
            calls: AtomicUsize::new(0),
            fail_deepseek: false,
        });
        let runtime = WebSearchRuntime::new(executor);
        let outcome = runtime
            .search(
                "test query",
                &WebSearchConfigurationV1::default(),
                None,
                &CancellationToken::default(),
            )
            .expect("keyless search");
        assert!(outcome.backend.ends_with("-keyless") || outcome.backend == "duckduckgo");
        assert_eq!(outcome.results.len(), 1);
    }

    #[test]
    fn configured_terminal_failure_is_rescued_once_and_rescue_is_not_cached() {
        let executor = Arc::new(FakeExecutor {
            calls: AtomicUsize::new(0),
            fail_deepseek: true,
        });
        let runtime = WebSearchRuntime::new(executor.clone());
        let configuration = WebSearchConfigurationV1 {
            backend: WebSearchBackendV1::Deepseek,
            maximum_retries: 0,
            ..WebSearchConfigurationV1::default()
        };
        for _ in 0..2 {
            let outcome = runtime
                .search(
                    "test query",
                    &configuration,
                    Some("secret"),
                    &CancellationToken::default(),
                )
                .expect("rescued search");
            assert_ne!(outcome.backend, "deepseek");
            assert_eq!(outcome.rescued_from.as_deref(), Some("deepseek"));
            assert!(!outcome.cached);
        }
        assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn successful_searches_use_the_bounded_memory_cache() {
        let executor = Arc::new(FakeExecutor {
            calls: AtomicUsize::new(0),
            fail_deepseek: false,
        });
        let runtime = WebSearchRuntime::new(executor.clone());
        let configuration = WebSearchConfigurationV1::default();
        let first = runtime
            .search(
                "  Test   Query ",
                &configuration,
                None,
                &CancellationToken::default(),
            )
            .expect("first search");
        let second = runtime
            .search(
                "test query",
                &configuration,
                None,
                &CancellationToken::default(),
            )
            .expect("cached search");
        assert!(!first.cached);
        assert!(second.cached);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn provider_tiers_are_resolved_without_silent_paid_downgrades() {
        let paid = WebSearchConfigurationV1 {
            backend: WebSearchBackendV1::Exa,
            provider_tier: WebSearchProviderTierV1::Paid,
            ..WebSearchConfigurationV1::default()
        };
        assert!(select_provider(&paid, None).is_err());
        assert_eq!(
            select_provider(&paid, Some("key")).unwrap(),
            SearchProviderV1::Exa
        );

        let free = WebSearchConfigurationV1 {
            provider_tier: WebSearchProviderTierV1::Free,
            ..paid
        };
        assert_eq!(
            select_provider(&free, None).unwrap(),
            SearchProviderV1::Keyless
        );

        let invalid = WebSearchConfigurationV1 {
            backend: WebSearchBackendV1::Deepseek,
            provider_tier: WebSearchProviderTierV1::Free,
            ..WebSearchConfigurationV1::default()
        };
        assert!(invalid.validate().is_err());

        let invalid_automatic = WebSearchConfigurationV1 {
            provider_tier: WebSearchProviderTierV1::Paid,
            ..WebSearchConfigurationV1::default()
        };
        assert!(invalid_automatic.validate().is_err());

        let invalid_keyless = WebSearchConfigurationV1 {
            backend: WebSearchBackendV1::Keyless,
            provider_tier: WebSearchProviderTierV1::Paid,
            ..WebSearchConfigurationV1::default()
        };
        assert!(invalid_keyless.validate().is_err());

        let paid_exa = WebSearchConfigurationV1 {
            backend: WebSearchBackendV1::Exa,
            provider_tier: WebSearchProviderTierV1::Paid,
            ..WebSearchConfigurationV1::default()
        };
        assert!(
            !super::super::providers::keyless_order(&paid_exa)
                .contains(&SearchProviderV1::ExaKeyless)
        );
    }

    struct EmptyThenDuckduckgoExecutor {
        calls: AtomicUsize,
    }

    impl SearchExecutorPort for EmptyThenDuckduckgoExecutor {
        fn search(
            &self,
            provider: SearchProviderV1,
            _configuration: &WebSearchConfigurationV1,
            _query: &str,
            _maximum_results: usize,
            _api_key: Option<&str>,
        ) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if provider == SearchProviderV1::Duckduckgo {
                return Ok(vec![WebSearchResultV1 {
                    title: "DuckDuckGo fallback".into(),
                    url: "https://example.com/fallback".into(),
                    snippet: "fallback result".into(),
                }]);
            }
            Ok(Vec::new())
        }
    }

    #[test]
    fn keyless_ring_continues_past_empty_provider_responses() {
        let executor = Arc::new(EmptyThenDuckduckgoExecutor {
            calls: AtomicUsize::new(0),
        });
        let runtime = WebSearchRuntime::new(executor.clone());
        let outcome = runtime
            .search(
                "test query",
                &WebSearchConfigurationV1::default(),
                None,
                &CancellationToken::default(),
            )
            .expect("DuckDuckGo fallback search");

        assert_eq!(outcome.backend, "duckduckgo");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 5);
        assert_eq!(
            outcome
                .attempts
                .iter()
                .filter(|attempt| attempt.status == "empty")
                .count(),
            4
        );
    }

    struct StaleThenCurrentExecutor;

    impl SearchExecutorPort for StaleThenCurrentExecutor {
        fn search(
            &self,
            provider: SearchProviderV1,
            _configuration: &WebSearchConfigurationV1,
            _query: &str,
            _maximum_results: usize,
            _api_key: Option<&str>,
        ) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
            let (title, snippet) = if provider == SearchProviderV1::ExaKeyless {
                (
                    "September 2026 prices",
                    "Mai 2026 Preise: stale index snapshot",
                )
            } else {
                (
                    "Live shop result",
                    "Price and availability on the live page",
                )
            };
            Ok(vec![WebSearchResultV1 {
                title: title.into(),
                url: format!("https://example.com/{}", provider.as_str()),
                snippet: snippet.into(),
            }])
        }
    }

    #[test]
    fn current_search_rejects_stale_ring_results_and_uses_the_next_provider() {
        let runtime = WebSearchRuntime::new(Arc::new(StaleThenCurrentExecutor));
        let configuration = WebSearchConfigurationV1 {
            backend: WebSearchBackendV1::Exa,
            provider_tier: WebSearchProviderTierV1::Free,
            cache_enabled: true,
            freshness_bypass_cache: true,
            ..WebSearchConfigurationV1::default()
        };

        let outcome = runtime
            .search_with_freshness(
                "cheapest current price right now",
                &configuration,
                None,
                WebSearchFreshnessModeV1::Current,
                &CancellationToken::default(),
            )
            .expect("freshness fallback search");

        assert_ne!(outcome.backend, SearchProviderV1::ExaKeyless.as_str());
        assert_eq!(outcome.freshness.rejected_results, 1);
        assert!(outcome.freshness.extraction_required);
        assert_eq!(outcome.attempts[0].status, "stale");
        assert!(!outcome.cached);
    }

    #[test]
    fn xai_domains_require_valid_dns_labels() {
        for invalid_domain in ["-example.com", "example-.com", "exam_ple.com"] {
            let invalid = WebSearchConfigurationV1 {
                backend: WebSearchBackendV1::Xai,
                xai_allowed_domains: vec![invalid_domain.into()],
                ..WebSearchConfigurationV1::default()
            };
            assert!(invalid.validate().is_err(), "accepted {invalid_domain}");
        }
    }

    struct BucketExecutor {
        calls: AtomicUsize,
        requested: Mutex<Vec<usize>>,
    }

    impl SearchExecutorPort for BucketExecutor {
        fn search(
            &self,
            _provider: SearchProviderV1,
            _configuration: &WebSearchConfigurationV1,
            _query: &str,
            maximum_results: usize,
            _api_key: Option<&str>,
        ) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requested.lock().unwrap().push(maximum_results);
            Ok((0..maximum_results)
                .map(|index| WebSearchResultV1 {
                    title: format!("Result {index}"),
                    url: format!("https://example.com/{index}"),
                    snippet: String::new(),
                })
                .collect())
        }
    }

    #[test]
    fn nearby_result_limits_share_a_bucket_but_preserve_each_callers_limit() {
        let executor = Arc::new(BucketExecutor {
            calls: AtomicUsize::new(0),
            requested: Mutex::new(Vec::new()),
        });
        let runtime = WebSearchRuntime::new(executor.clone());
        let five = WebSearchConfigurationV1 {
            maximum_results: 5,
            ..WebSearchConfigurationV1::default()
        };
        let eight = WebSearchConfigurationV1 {
            maximum_results: 8,
            ..five.clone()
        };
        let first = runtime
            .search("bucket me", &five, None, &CancellationToken::default())
            .unwrap();
        let second = runtime
            .search("bucket me", &eight, None, &CancellationToken::default())
            .unwrap();
        assert_eq!(first.results.len(), 5);
        assert_eq!(second.results.len(), 8);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*executor.requested.lock().unwrap(), vec![10]);
    }

    #[test]
    fn verbose_results_are_trimmed_to_the_aggregate_continuation_budget() {
        let mut results = (0..100)
            .map(|index| WebSearchResultV1 {
                title: format!("Result {index} {}", "t".repeat(500)),
                url: format!("https://example.com/{index}/{}", "u".repeat(3_900)),
                snippet: "s".repeat(4_096),
            })
            .collect::<Vec<_>>();

        bound_results_payload(&mut results);

        assert!(!results.is_empty());
        assert!(results.len() < 100);
        assert!(serde_json::to_vec(&results).unwrap().len() <= MAXIMUM_RESULTS_PAYLOAD_BYTES);
        assert_eq!(results[0].title, format!("Result 0 {}", "t".repeat(500)));
    }

    struct SlowExecutor {
        calls: AtomicUsize,
        gate: Barrier,
    }

    impl SearchExecutorPort for SlowExecutor {
        fn search(
            &self,
            provider: SearchProviderV1,
            _configuration: &WebSearchConfigurationV1,
            _query: &str,
            _maximum_results: usize,
            _api_key: Option<&str>,
        ) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.gate.wait();
            thread::sleep(Duration::from_millis(50));
            Ok(vec![WebSearchResultV1 {
                title: provider.as_str().into(),
                url: "https://example.com/flight".into(),
                snippet: String::new(),
            }])
        }
    }

    #[test]
    fn concurrent_identical_searches_are_single_flight() {
        let executor = Arc::new(SlowExecutor {
            calls: AtomicUsize::new(0),
            gate: Barrier::new(2),
        });
        let runtime = WebSearchRuntime::new(executor.clone());
        let configuration = WebSearchConfigurationV1::default();
        let first_runtime = runtime.clone();
        let first_configuration = configuration.clone();
        let first = thread::spawn(move || {
            first_runtime
                .search(
                    "same query",
                    &first_configuration,
                    None,
                    &CancellationToken::default(),
                )
                .unwrap()
        });
        executor.gate.wait();
        let second = runtime
            .search(
                "same query",
                &configuration,
                None,
                &CancellationToken::default(),
            )
            .unwrap();
        let first = first.join().unwrap();
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert!(first.coalesced || second.coalesced || second.cached);
    }
}
