//! xAI Responses API adapter with server-side web search.

use std::time::Duration;

use serde_json::{Value, json};

use super::{
    SearchFailure, client, deepseek::parse_agentic_response, endpoint, http_failure,
    provider_base_url,
};
use crate::WebSearchResultV1;

const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

pub(super) fn search(
    base_url: &str,
    model: &str,
    allowed_domains: &[String],
    excluded_domains: &[String],
    query: &str,
    maximum_results: usize,
    api_key: &str,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let body = request_body(
        model,
        allowed_domains,
        excluded_domains,
        query,
        maximum_results,
    );
    let response = client(timeout)?
        .post(endpoint(
            provider_base_url(base_url, DEFAULT_BASE_URL),
            "responses",
        )?)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .map_err(|error| SearchFailure::retryable(format!("xAI search request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(http_failure("xAI", response.status()));
    }
    let payload: Value = response
        .json()
        .map_err(|error| SearchFailure::retryable(format!("xAI returned invalid JSON: {error}")))?;
    parse_agentic_response(&payload, maximum_results, "xAI")
}

fn request_body(
    model: &str,
    allowed_domains: &[String],
    excluded_domains: &[String],
    query: &str,
    maximum_results: usize,
) -> Value {
    let mut web_search_tool = json!({"type": "web_search"});
    if !allowed_domains.is_empty() {
        web_search_tool["filters"] = json!({"allowed_domains": allowed_domains});
    } else if !excluded_domains.is_empty() {
        web_search_tool["filters"] = json!({"excluded_domains": excluded_domains});
    }
    let prompt = format!(
        "Search the current public web for the following query and return only JSON as {{\"results\":[{{\"title\":\"...\",\"url\":\"https://...\",\"snippet\":\"...\"}}]}} with at most {maximum_results} results. Do not invent URLs. Query: {query}"
    );
    json!({
        "model": model,
        "input": [{"role": "user", "content": prompt}],
        "tools": [web_search_tool],
        "include": ["no_inline_citations"],
        "store": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_server_side_search_and_domain_filters() {
        let body = request_body(
            "grok-build-0.1",
            &["arxiv.org".into()],
            &[],
            "current research",
            7,
        );

        assert_eq!(body["model"], json!("grok-build-0.1"));
        assert_eq!(body["tools"][0]["type"], json!("web_search"));
        assert_eq!(
            body["tools"][0]["filters"]["allowed_domains"],
            json!(["arxiv.org"])
        );
        assert_eq!(body["store"], json!(false));
        assert!(
            body["input"][0]["content"]
                .as_str()
                .unwrap()
                .contains("at most 7 results")
        );
    }
}
