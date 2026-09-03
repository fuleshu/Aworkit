//! Brave Search API adapter.

use std::time::Duration;

use serde::Deserialize;

use super::{SearchFailure, client, endpoint, http_failure, provider_base_url, validated_result};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.search.brave.com";

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

pub(super) fn search(
    base_url: &str,
    query: &str,
    maximum_results: usize,
    api_key: &str,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let response = client(timeout)?
        .get(endpoint(
            provider_base_url(base_url, DEFAULT_BASE_URL),
            "res/v1/web/search",
        )?)
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .query(&[
            ("q", query),
            ("count", &maximum_results.min(20).to_string()),
        ])
        .send()
        .map_err(|error| SearchFailure::retryable(format!("Brave request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("Brave", response.status()));
    }
    let payload: BraveResponse = response.json().map_err(|error| {
        SearchFailure::retryable(format!("Brave returned invalid JSON: {error}"))
    })?;
    Ok(normalize_results(payload, maximum_results))
}

fn normalize_results(payload: BraveResponse, maximum_results: usize) -> Vec<WebSearchResultV1> {
    payload
        .web
        .map(|web| web.results)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|result| validated_result(&result.title, &result.url, &result.description))
        .take(maximum_results)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_brave_web_results() {
        let payload = serde_json::from_value(json!({"web":{"results":[{
            "title":"Brave result",
            "url":"https://example.com/brave",
            "description":"search snippet"
        }]}}))
        .expect("Brave payload");

        let results = normalize_results(payload, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "search snippet");
    }
}
