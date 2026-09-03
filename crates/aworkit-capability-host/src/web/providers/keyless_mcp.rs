//! Minimal JSON-RPC client for the public Exa and Parallel MCP endpoints.

use std::{
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{SearchFailure, client, validated_result};
use crate::WebSearchResultV1;

const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";
const PARALLEL_MCP_URL: &str = "https://search.parallel.ai/mcp";

pub(super) fn search_exa(
    query: &str,
    maximum_results: usize,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let text = mcp_call(
        EXA_MCP_URL,
        "web_search_exa",
        json!({"query": query, "numResults": maximum_results}),
        timeout,
    )?;
    let results = parse_exa_text(&text, maximum_results);
    if results.is_empty() {
        return Err(SearchFailure::retryable(
            "Exa keyless search returned no recognizable results",
        ));
    }
    Ok(results)
}

pub(super) fn search_parallel(
    query: &str,
    maximum_results: usize,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let text = mcp_call(
        PARALLEL_MCP_URL,
        "web_search",
        json!({
            "objective": query,
            "search_queries": [query],
            "session_id": session_id()
        }),
        timeout,
    )?;
    let payload: Value = serde_json::from_str(&text).map_err(|error| {
        SearchFailure::retryable(format!("Parallel keyless payload is invalid: {error}"))
    })?;
    let rows = payload
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| SearchFailure::retryable("Parallel keyless response has no results"))?;
    Ok(rows
        .iter()
        .filter_map(|result| {
            let snippet = result
                .get("excerpts")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            validated_result(
                result
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                result
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                &snippet,
            )
        })
        .take(maximum_results)
        .collect())
}

fn mcp_call(
    url: &str,
    tool: &str,
    arguments: Value,
    timeout: Duration,
) -> Result<String, SearchFailure> {
    let response = client(timeout)?
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }))
        .send()
        .map_err(|error| {
            SearchFailure::retryable(format!("keyless MCP request failed: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(super::http_failure("keyless MCP", response.status()));
    }
    let body = response.text().map_err(|error| {
        SearchFailure::retryable(format!("keyless MCP response unreadable: {error}"))
    })?;
    parse_mcp_body(&body)
}

fn parse_mcp_body(body: &str) -> Result<String, SearchFailure> {
    if let Ok(value) = serde_json::from_str::<Value>(body.trim())
        && let Some(text) = mcp_text(&value)?
    {
        return Ok(text);
    }
    for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
        if let Ok(value) = serde_json::from_str::<Value>(line)
            && let Some(text) = mcp_text(&value)?
        {
            return Ok(text);
        }
    }
    Err(SearchFailure::retryable(
        "keyless MCP returned an unrecognized response",
    ))
}

fn mcp_text(payload: &Value) -> Result<Option<String>, SearchFailure> {
    if let Some(error) = payload.get("error") {
        return Err(search_failure_from_text(&error.to_string()));
    }
    let result = payload.get("result").unwrap_or(&Value::Null);
    let texts = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(search_failure_from_text(&texts.join(" ")));
    }
    Ok(texts.first().map(|text| (*text).to_owned()))
}

fn search_failure_from_text(message: &str) -> SearchFailure {
    let lowered = message.to_ascii_lowercase();
    if [
        "rate limit",
        "rate-limit",
        "ratelimit",
        "too many requests",
        "429",
        "quota",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        SearchFailure::retryable(message)
    } else {
        SearchFailure::terminal(message)
    }
}

fn parse_exa_text(text: &str, maximum_results: usize) -> Vec<WebSearchResultV1> {
    text.split("\n---\n")
        .filter_map(|block| {
            let mut title = "";
            let mut url = "";
            let mut in_highlights = false;
            let mut highlights = Vec::new();
            for line in block.lines().map(str::trim) {
                if let Some(value) = line.strip_prefix("Title:") {
                    title = value.trim();
                    in_highlights = false;
                } else if let Some(value) = line.strip_prefix("URL:") {
                    url = value.trim();
                    in_highlights = false;
                } else if line.starts_with("Highlights:") {
                    in_highlights = true;
                } else if line.starts_with("Published:") || line.starts_with("Author:") {
                    in_highlights = false;
                } else if in_highlights && !line.is_empty() {
                    highlights.push(line);
                }
            }
            validated_result(title, url, &highlights.join(" "))
        })
        .take(maximum_results)
        .collect()
}

fn session_id() -> &'static str {
    static SESSION_ID: OnceLock<String> = OnceLock::new();
    SESSION_ID.get_or_init(|| {
        let seed = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        format!("aworkit-{:x}", Sha256::digest(seed.as_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_sse_mcp_results() {
        let payload = json!({"result":{"content":[{"type":"text","text":"hello"}]}});
        assert_eq!(parse_mcp_body(&payload.to_string()).unwrap(), "hello");
        assert_eq!(
            parse_mcp_body(&format!("event: message\ndata: {}\n", payload)).unwrap(),
            "hello"
        );
    }

    #[test]
    fn parses_exa_formatted_results() {
        let rows = parse_exa_text(
            "Title: Example\nURL: https://example.com\nHighlights:\nUseful text",
            5,
        );
        assert_eq!(rows[0].snippet, "Useful text");
    }
}
