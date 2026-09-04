//! Paid DeepSeek Responses API search adapter.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ProviderSearchOutcome, SearchFailure, bounded_text, endpoint, validated_result};
use crate::{WebSearchProviderUsageV1, WebSearchResultV1};

#[derive(Deserialize)]
struct StructuredSearchResults {
    #[serde(default)]
    results: Vec<StructuredSearchResult>,
}

#[derive(Deserialize)]
struct StructuredSearchResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default, alias = "description")]
    snippet: String,
}

pub(super) fn search(
    base_url: &str,
    model: &str,
    maximum_output_tokens: u32,
    query: &str,
    maximum_results: usize,
    api_key: &str,
    timeout: Duration,
) -> Result<ProviderSearchOutcome, SearchFailure> {
    if api_key.trim().is_empty() {
        return Err(SearchFailure::terminal("DeepSeek API key is empty"));
    }
    let endpoint = endpoint(base_url, "responses")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Aworkit/1.0 web-search")
        .build()
        .map_err(|error| SearchFailure::terminal(format!("web client unavailable: {error}")))?;
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .json(&request_body(
            model,
            maximum_output_tokens,
            query,
            maximum_results,
        ))
        .send()
        .map_err(|error| {
            SearchFailure::retryable(format!("DeepSeek search request failed: {error}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(deepseek_http_failure(status, response));
    }
    let payload: Value = response.json().map_err(|error| {
        if error.is_timeout() {
            SearchFailure::retryable(
                "DeepSeek search response timed out while the server-side search was still running; increase the web-search request timeout",
            )
        } else {
            SearchFailure::retryable(format!(
                "DeepSeek response is not valid JSON: {error}"
            ))
        }
    })?;
    let usage = parse_usage(&payload, model);
    parse_agentic_response(&payload, maximum_results, "DeepSeek")
        .map(|results| ProviderSearchOutcome {
            results,
            usage: usage.clone(),
        })
        .map_err(|error| error.with_usage(usage))
}

fn parse_usage(payload: &Value, model: &str) -> Option<WebSearchProviderUsageV1> {
    let usage = payload.get("usage")?.as_object()?;
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_input_tokens = usage
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_output_tokens = usage
        .get("output_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| input_tokens.saturating_add(output_tokens));
    if input_tokens == 0 && output_tokens == 0 && total_tokens == 0 {
        return None;
    }
    Some(WebSearchProviderUsageV1 {
        provider: "deepseek".into(),
        model: model.to_owned(),
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
        total_tokens,
        // The DeepSeek Responses API reports token counts, not a charged
        // currency amount. Keep this explicitly absent instead of estimating.
        reported_cost_micros: None,
        reported_cost_currency: None,
    })
}

fn request_body(
    model: &str,
    maximum_output_tokens: u32,
    query: &str,
    maximum_results: usize,
) -> Value {
    json!({
        "model": model,
        "instructions": format!(
            "Perform a current web search. Return only a JSON object with a results array of at most {maximum_results} items. Every item must contain title, url, and snippet. Never invent a URL; include only URLs surfaced by web search."
        ),
        "input": query,
        "tools": [{"type": "web_search"}],
        "tool_choice": {"type": "web_search"},
        "text": {
            "format": {
                "type": "json_schema",
                "name": "web_search_results",
                "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "results": {
                            "type": "array",
                            "maxItems": maximum_results,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "title": {"type": "string"},
                                    "url": {"type": "string"},
                                    "snippet": {"type": "string"}
                                },
                                "required": ["title", "url", "snippet"]
                            }
                        }
                    },
                    "required": ["results"]
                }
            }
        },
        "max_output_tokens": maximum_output_tokens,
        "store": false
    })
}

pub(super) fn parse_agentic_response(
    payload: &Value,
    maximum_results: usize,
    provider: &str,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    // Successful Responses objects carry `"error": null`. Treat only a
    // populated error value as a provider failure; the previous presence-only
    // check rejected every successful DeepSeek response before reading output.
    if let Some(error) = payload.get("error").filter(|value| !value.is_null()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("provider returned an error response");
        return Err(SearchFailure::retryable(format!(
            "{provider}: {}",
            bounded_text(message, 768)
        )));
    }
    let output_text = payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(structured) = parse_structured_results(&output_text) {
        let results = structured
            .results
            .into_iter()
            .filter_map(|result| validated_result(&result.title, &result.url, &result.snippet))
            .take(maximum_results)
            .collect::<Vec<_>>();
        if !results.is_empty() {
            return Ok(results);
        }
    }

    let citations = extract_citations(payload, &output_text, maximum_results);
    if !citations.is_empty() {
        return Ok(citations);
    }
    Err(SearchFailure::retryable(format!(
        "{provider} completed web search without a usable structured result or URL citation"
    )))
}

/// Preserves the provider's safe error code/message without retaining headers,
/// credentials, or an unbounded response body in durable Run evidence.
fn deepseek_http_failure(
    status: reqwest::StatusCode,
    response: reqwest::blocking::Response,
) -> SearchFailure {
    let detail = response
        .json::<Value>()
        .ok()
        .and_then(|payload| payload.get("error").cloned())
        .and_then(|error| {
            let code = error.get("code").and_then(Value::as_str);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str());
            match (code, message) {
                (Some(code), Some(message)) => Some(format!("{code}: {message}")),
                (None, Some(message)) => Some(message.to_owned()),
                (Some(code), None) => Some(code.to_owned()),
                (None, None) => None,
            }
        });
    let message = detail.map_or_else(
        || format!("DeepSeek search failed with HTTP {status}"),
        |detail| {
            format!(
                "DeepSeek search failed with HTTP {status}: {}",
                bounded_text(&detail, 768)
            )
        },
    );
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        SearchFailure::retryable(message)
    } else {
        SearchFailure::terminal(message)
    }
}

fn extract_citations(
    payload: &Value,
    output_text: &str,
    maximum_results: usize,
) -> Vec<WebSearchResultV1> {
    let mut results = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if let Some(citations) = payload.get("citations").and_then(Value::as_array) {
        for citation in citations {
            let url = citation
                .as_str()
                .or_else(|| citation.get("url").and_then(Value::as_str));
            let Some(url) = url else { continue };
            if !seen.insert(url.to_owned()) {
                continue;
            }
            let title = citation.get("title").and_then(Value::as_str).unwrap_or(url);
            if let Some(result) = validated_result(title, url, &bounded_text(output_text, 4_096)) {
                results.push(result);
            }
            if results.len() == maximum_results {
                return results;
            }
        }
    }
    let Some(output) = payload.get("output").and_then(Value::as_array) else {
        return results;
    };
    for content in output
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
    {
        let Some(annotations) = content.get("annotations").and_then(Value::as_array) else {
            continue;
        };
        for annotation in annotations {
            let citation = annotation.get("url_citation").unwrap_or(annotation);
            let Some(url) = citation.get("url").and_then(Value::as_str) else {
                continue;
            };
            if !seen.insert(url.to_owned()) {
                continue;
            }
            let title = citation.get("title").and_then(Value::as_str).unwrap_or(url);
            if let Some(result) = validated_result(title, url, &bounded_text(output_text, 4_096)) {
                results.push(result);
            }
            if results.len() == maximum_results {
                return results;
            }
        }
    }
    results
}

fn parse_structured_results(output_text: &str) -> Option<StructuredSearchResults> {
    serde_json::from_str(output_text.trim()).ok().or_else(|| {
        let start = output_text.find('{')?;
        let end = output_text.rfind('}')?;
        serde_json::from_str(&output_text[start..=end]).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_deepseek_output() {
        let payload = json!({
            "error": null,
            "output": [{"type":"message","content":[{
                "type":"output_text",
                "text":"{\"results\":[{\"title\":\"Docs\",\"url\":\"https://example.com/docs\",\"snippet\":\"Current docs\"}]}"
            }]}]
        });
        let results = parse_agentic_response(&payload, 3, "DeepSeek").expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Docs");
    }

    #[test]
    fn populated_response_error_remains_a_failure() {
        let error = parse_agentic_response(
            &json!({"error":{"code":"invalid_request","message":"Unsupported option"}}),
            3,
            "DeepSeek",
        )
        .unwrap_err();
        assert_eq!(error.message, "DeepSeek: Unsupported option");
    }

    #[test]
    fn parses_provider_reported_usage_without_inventing_a_currency_cost() {
        let usage = parse_usage(
            &json!({
                "usage": {
                    "input_tokens": 120,
                    "input_tokens_details": {"cached_tokens": 20},
                    "output_tokens": 30,
                    "output_tokens_details": {"reasoning_tokens": 7},
                    "total_tokens": 150
                }
            }),
            "deepseek-v4-flash",
        )
        .expect("usage");
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cached_input_tokens, 20);
        assert_eq!(usage.reasoning_output_tokens, 7);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.reported_cost_micros, None);
        assert_eq!(usage.reported_cost_currency, None);
    }

    #[test]
    fn request_forces_server_side_search_and_structured_results() {
        let body = request_body("deepseek-v4-flash", 4_096, "current pricing", 7);
        assert_eq!(body["model"], json!("deepseek-v4-flash"));
        assert_eq!(body["tools"], json!([{"type":"web_search"}]));
        assert_eq!(body["tool_choice"], json!({"type":"web_search"}));
        assert_eq!(
            body["text"]["format"]["schema"]["properties"]["results"]["maxItems"],
            json!(7)
        );
        assert_eq!(body["max_output_tokens"], json!(4_096));
        assert_eq!(body["store"], json!(false));
    }

    #[test]
    fn falls_back_to_response_url_citations() {
        let payload = json!({
            "output": [{"type":"message","content":[{
                "type":"output_text",
                "text":"Search summary",
                "annotations":[{"type":"url_citation","url":"https://example.com/","title":"Example"}]
            }]}]
        });
        let results = parse_agentic_response(&payload, 3, "DeepSeek").expect("citations");
        assert_eq!(results[0].url, "https://example.com/");
    }
}
