//! Parallel Search API adapter.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::{SearchFailure, client, endpoint, http_failure, provider_base_url, validated_result};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.parallel.ai";

#[derive(Deserialize)]
struct ParallelResponse {
    #[serde(default)]
    results: Vec<ParallelResult>,
}

#[derive(Deserialize)]
struct ParallelResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    excerpts: Vec<String>,
}

pub(super) fn search(
    base_url: &str,
    mode: &str,
    query: &str,
    maximum_results: usize,
    api_key: &str,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let response = client(timeout)?
        .post(endpoint(
            provider_base_url(base_url, DEFAULT_BASE_URL),
            "v1beta/search",
        )?)
        .header("x-api-key", api_key)
        .json(&json!({
            "objective": query,
            "search_queries": [query],
            "mode": mode,
            "max_results": maximum_results.min(20),
            "excerpts": {"max_chars_per_result": 4_000}
        }))
        .send()
        .map_err(|error| SearchFailure::retryable(format!("Parallel request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("Parallel", response.status()));
    }
    let payload: ParallelResponse = response.json().map_err(|error| {
        SearchFailure::retryable(format!("Parallel returned invalid JSON: {error}"))
    })?;
    Ok(normalize_results(payload, maximum_results))
}

fn normalize_results(payload: ParallelResponse, maximum_results: usize) -> Vec<WebSearchResultV1> {
    payload
        .results
        .into_iter()
        .filter_map(|result| {
            validated_result(&result.title, &result.url, &result.excerpts.join(" "))
        })
        .take(maximum_results)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_parallel_excerpts() {
        let payload = serde_json::from_value(json!({
            "results": [{
                "title": "Parallel result",
                "url": "https://example.com/parallel",
                "excerpts": ["first", "second"]
            }]
        }))
        .expect("parallel payload");

        let results = normalize_results(payload, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "first second");
    }
}
