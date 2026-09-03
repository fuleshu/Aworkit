//! Keenable keyed and public search adapter.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::{SearchFailure, client, endpoint, http_failure, provider_base_url, validated_result};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.keenable.ai";

#[derive(Deserialize)]
struct KeenableResponse {
    #[serde(default)]
    results: Vec<KeenableResult>,
}

#[derive(Deserialize)]
struct KeenableResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    description: String,
}

pub(super) fn search(
    base_url: &str,
    query: &str,
    maximum_results: usize,
    api_key: Option<&str>,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let path = if api_key.is_some() {
        "v1/search"
    } else {
        "v1/search/public"
    };
    let mut request = client(timeout)?
        .post(endpoint(
            provider_base_url(base_url, DEFAULT_BASE_URL),
            path,
        )?)
        .header("X-Keenable-Title", "aworkit")
        .json(&json!({
            "query": query,
            "max_results": maximum_results.min(20)
        }));
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .map_err(|error| SearchFailure::retryable(format!("Keenable request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("Keenable", response.status()));
    }
    let payload: KeenableResponse = response.json().map_err(|error| {
        SearchFailure::retryable(format!("Keenable returned invalid JSON: {error}"))
    })?;
    Ok(normalize_results(payload, maximum_results))
}

fn normalize_results(payload: KeenableResponse, maximum_results: usize) -> Vec<WebSearchResultV1> {
    payload
        .results
        .into_iter()
        .filter_map(|result| {
            let snippet = if result.snippet.is_empty() {
                result.description
            } else {
                result.snippet
            };
            validated_result(&result.title, &result.url, &snippet)
        })
        .take(maximum_results)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_keenable_snippet_and_description_fallback() {
        let payload = serde_json::from_value(json!({"results":[
            {
                "title":"Keenable snippet",
                "url":"https://example.com/keenable/one",
                "snippet":"preferred"
            },
            {
                "title":"Keenable description",
                "url":"https://example.com/keenable/two",
                "description":"fallback"
            }
        ]}))
        .expect("Keenable payload");

        let results = normalize_results(payload, 5);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet, "preferred");
        assert_eq!(results[1].snippet, "fallback");
    }
}
