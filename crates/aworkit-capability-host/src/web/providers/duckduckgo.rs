//! Keyless DuckDuckGo adapter with HTML and Lite endpoint failover.

use std::time::Duration;

use scraper::{ElementRef, Html, Selector};

use super::{SearchFailure, validated_result};
use crate::WebSearchResultV1;

const HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";

pub(super) fn search(
    query: &str,
    maximum_results: usize,
    timeout: Duration,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (compatible; Aworkit/1.0; +https://github.com)")
        .build()
        .map_err(|error| SearchFailure::terminal(format!("web client unavailable: {error}")))?;

    let mut failures = Vec::new();
    for endpoint in [HTML_ENDPOINT, LITE_ENDPOINT] {
        match request_endpoint(&client, endpoint, query, maximum_results) {
            Ok(results) => return Ok(results),
            Err(error) => failures.push(error.message),
        }
    }
    Err(SearchFailure::retryable(format!(
        "DuckDuckGo keyless search failed through both endpoints: {}",
        failures.join("; ")
    )))
}

fn request_endpoint(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    query: &str,
    maximum_results: usize,
) -> Result<Vec<WebSearchResultV1>, SearchFailure> {
    let response = client
        .post(endpoint)
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "en-US,en;q=0.8")
        .form(&[("q", query)])
        .send()
        .map_err(|error| SearchFailure::retryable(format!("DuckDuckGo request failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(super::http_failure("DuckDuckGo", status));
    }
    let body = response.text().map_err(|error| {
        SearchFailure::retryable(format!("DuckDuckGo response unreadable: {error}"))
    })?;
    let results = parse_results(&body, maximum_results);
    if !results.is_empty() || is_explicit_no_results(&body) {
        return Ok(results);
    }
    if body.to_ascii_lowercase().contains("anomaly-modal")
        || body.to_ascii_lowercase().contains("challenge")
    {
        return Err(SearchFailure::retryable(
            "DuckDuckGo returned an automated-traffic challenge",
        ));
    }
    Err(SearchFailure::retryable(
        "DuckDuckGo returned an unrecognized result document",
    ))
}

fn parse_results(html: &str, maximum_results: usize) -> Vec<WebSearchResultV1> {
    let document = Html::parse_document(html);
    let result_selector = Selector::parse(".result, .web-result").expect("static selector");
    let html_link = Selector::parse("a.result__a, a.result-link").expect("static selector");
    let snippet_selector =
        Selector::parse(".result__snippet, .result-snippet").expect("static selector");
    let mut results = Vec::new();

    for result in document.select(&result_selector) {
        if let Some(item) = parse_container(result, &html_link, &snippet_selector) {
            results.push(item);
            if results.len() == maximum_results {
                return results;
            }
        }
    }

    // Lite documents do not consistently wrap links and snippets in one row.
    if results.is_empty() {
        for link in document.select(&html_link) {
            let Some(href) = link.value().attr("href") else {
                continue;
            };
            let title = link.text().collect::<Vec<_>>().join(" ");
            if let Some(item) = validated_result(&title, &unwrap_redirect(href), "") {
                results.push(item);
                if results.len() == maximum_results {
                    break;
                }
            }
        }
    }
    deduplicate(results)
}

fn parse_container(
    result: ElementRef<'_>,
    link_selector: &Selector,
    snippet_selector: &Selector,
) -> Option<WebSearchResultV1> {
    let link = result.select(link_selector).next()?;
    let title = link.text().collect::<Vec<_>>().join(" ");
    let href = link.value().attr("href")?;
    let snippet = result
        .select(snippet_selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    validated_result(&title, &unwrap_redirect(href), &snippet)
}

fn unwrap_redirect(href: &str) -> String {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    reqwest::Url::parse(&absolute)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "uddg")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or(absolute)
}

fn deduplicate(results: Vec<WebSearchResultV1>) -> Vec<WebSearchResultV1> {
    let mut seen = std::collections::BTreeSet::new();
    results
        .into_iter()
        .filter(|result| seen.insert(result.url.clone()))
        .collect()
}

fn is_explicit_no_results(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("no results.") || lower.contains("no results found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_html_results_and_unwraps_redirects() {
        let html = r#"
          <div class="result results_links">
            <h2><a data-x="1" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fone.example%2Fdoc">One <b>Title</b></a></h2>
            <a class="result__snippet">First <em>snippet</em></a>
          </div>
          <div class="result"><a class="result__a" href="https://two.example/">Two</a></div>
        "#;
        let results = parse_results(html, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "One Title");
        assert_eq!(results[0].url, "https://one.example/doc");
        assert_eq!(results[0].snippet, "First snippet");
    }
}
