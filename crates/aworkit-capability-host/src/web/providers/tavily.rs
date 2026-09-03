//! Tavily keyed and opt-in anonymous search adapter.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::{SearchFailure, client, endpoint, http_failure, provider_base_url, validated_result};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.tavily.com";

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
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
    api_key: Option<&str>,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let mut request = client(timeout)?
        .post(endpoint(
            provider_base_url(base_url, DEFAULT_BASE_URL),
            "search",
        )?)
        .header("X-Client-Name", "aworkit")
        .json(&json!({
            "query": query,
            "max_results": maximum_results.min(20),
            "include_raw_content": false,
            "include_images": false
        }));
    request = if let Some(api_key) = api_key {
        request.bearer_auth(api_key)
    } else {
        request.header("X-Tavily-Access-Mode", "keyless")
    };
    let response = request
        .send()
        .map_err(|error| SearchFailure::retryable(format!("Tavily request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("Tavily", response.status()));
    }
    let payload: TavilyResponse = response.json().map_err(|error| {
        SearchFailure::retryable(format!("Tavily returned invalid JSON: {error}"))
    })?;
    Ok(normalize_results(payload, maximum_results))
}

fn normalize_results(payload: TavilyResponse, maximum_results: usize) -> Vec<WebSearchResultV1> {
    payload
        .results
        .into_iter()
        .filter_map(|result| validated_result(&result.title, &result.url, &result.content))
        .take(maximum_results)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_tavily_content() {
        let payload = serde_json::from_value(json!({"results":[{
            "title":"Tavily result",
            "url":"https://example.com/tavily",
            "content":"AI search snippet"
        }]}))
        .expect("Tavily payload");

        let results = normalize_results(payload, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "AI search snippet");
    }
}
