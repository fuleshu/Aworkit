//! SearXNG JSON search adapter.

use std::time::Duration;

use serde::Deserialize;

use super::{SearchFailure, endpoint, validated_result};
use crate::WebSearchResultV1;

#[derive(Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

pub(super) fn search(
    base_url: &str,
    query: &str,
    maximum_results: usize,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let endpoint = endpoint(base_url, "search")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("Aworkit/1.0 web-search")
        .build()
        .map_err(|error| SearchFailure::terminal(format!("web client unavailable: {error}")))?;
    let response = client
        .get(endpoint)
        .query(&[
            ("q", query),
            ("format", "json"),
            ("categories", "general"),
            ("language", "auto"),
        ])
        .send()
        .map_err(|error| SearchFailure::retryable(format!("SearXNG request failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(super::http_failure("SearXNG", status));
    }
    let payload: SearxngResponse = response.json().map_err(|error| {
        SearchFailure::retryable(format!("SearXNG response is not valid JSON: {error}"))
    })?;
    Ok(normalize_results(payload, maximum_results))
}

fn normalize_results(payload: SearxngResponse, maximum_results: usize) -> Vec<WebSearchResultV1> {
    payload
        .results
        .into_iter()
        .filter_map(|result| validated_result(&result.title, &result.url, &result.content))
        .take(maximum_results)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_searxng_content() {
        let payload = serde_json::from_value(json!({"results":[{
            "title":"SearXNG result",
            "url":"https://example.com/searxng",
            "content":"metasearch snippet"
        }]}))
        .expect("SearXNG payload");

        let results = normalize_results(payload, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "metasearch snippet");
    }
}
