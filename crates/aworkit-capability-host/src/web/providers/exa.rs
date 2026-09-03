//! Exa API-key search adapter.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::{SearchFailure, client, endpoint, http_failure, provider_base_url, validated_result};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.exa.ai";

#[derive(Deserialize)]
struct ExaResponse {
    #[serde(default)]
    results: Vec<ExaResult>,
}

#[derive(Deserialize)]
struct ExaResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    highlights: Vec<String>,
    #[serde(default)]
    text: String,
}

pub(super) fn search(
    base_url: &str,
    query: &str,
    maximum_results: usize,
    api_key: &str,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let response = client(timeout)?
        .post(endpoint(
            provider_base_url(base_url, DEFAULT_BASE_URL),
            "search",
        )?)
        .header("x-api-key", api_key)
        .json(&json!({
            "query": query,
            "numResults": maximum_results,
            "contents": {"highlights": true}
        }))
        .send()
        .map_err(|error| SearchFailure::retryable(format!("Exa request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("Exa", response.status()));
    }
    let payload: ExaResponse = response
        .json()
        .map_err(|error| SearchFailure::retryable(format!("Exa returned invalid JSON: {error}")))?;
    Ok(payload
        .results
        .into_iter()
        .filter_map(|result| {
            let snippet = if result.highlights.is_empty() {
                result.text
            } else {
                result.highlights.join(" ")
            };
            validated_result(&result.title, &result.url, &snippet)
        })
        .take(maximum_results)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exa_highlights() {
        let payload: ExaResponse = serde_json::from_value(json!({
            "results": [{
                "title": "Exa result",
                "url": "https://example.com/exa",
                "highlights": ["first", "second"]
            }]
        }))
        .expect("payload");
        assert_eq!(payload.results[0].highlights.join(" "), "first second");
    }
}
