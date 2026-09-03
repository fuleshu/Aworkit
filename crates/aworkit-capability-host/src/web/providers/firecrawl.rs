//! Firecrawl keyed and anonymous search adapter.

use std::time::Duration;

use serde_json::{Value, json};

use super::{SearchFailure, client, endpoint, http_failure, provider_base_url, validated_result};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.firecrawl.dev";

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
            "v2/search",
        )?)
        .json(&json!({"query": query, "limit": maximum_results}));
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .map_err(|error| SearchFailure::retryable(format!("Firecrawl request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("Firecrawl", response.status()));
    }
    let payload: Value = response.json().map_err(|error| {
        SearchFailure::retryable(format!("Firecrawl returned invalid JSON: {error}"))
    })?;
    parse_response(&payload, maximum_results)
}

fn parse_response(
    payload: &Value,
    maximum_results: usize,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(SearchFailure::retryable(
            payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Firecrawl reported an unsuccessful search"),
        ));
    }
    let rows = payload
        .pointer("/data/web")
        .or_else(|| payload.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| SearchFailure::retryable("Firecrawl response has no web results"))?;
    Ok(rows
        .iter()
        .filter_map(|result| {
            let metadata = result.get("metadata").unwrap_or(&Value::Null);
            validated_result(
                result
                    .get("title")
                    .or_else(|| metadata.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                result
                    .get("url")
                    .or_else(|| metadata.get("sourceURL"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                result
                    .get("description")
                    .or_else(|| metadata.get("description"))
                    .or_else(|| result.get("markdown"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
        })
        .take(maximum_results)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_v2_response_shape() {
        let results = parse_response(
            &json!({"success":true,"data":{"web":[{
                "title":"Firecrawl", "url":"https://example.com/", "description":"result"
            }]}}),
            10,
        )
        .expect("results");
        assert_eq!(results[0].title, "Firecrawl");
    }
}
