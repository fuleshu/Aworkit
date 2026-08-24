//! Bounded HTTPS web search and fetch tools behind a replaceable transport
//! port. Search uses the keyless DuckDuckGo HTML endpoint; fetch downloads at
//! most a bounded prefix and extracts plain text. All network activity is
//! explicit desktop-user authority — these are read-only but not sandboxed.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::CancellationToken;

const MAXIMUM_SEARCH_RESULTS: usize = 8;
const MAXIMUM_QUERY_BYTES: usize = 16 * 1024;
const MAXIMUM_DOWNLOAD_BYTES: usize = 1024 * 1024;
const MAXIMUM_EXTRACT_BYTES: usize = 32 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchResultV1 {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFetchResultV1 {
    pub url: String,
    pub title: String,
    pub text: String,
    pub bytes_downloaded: u64,
}

/// Replaceable network seam; production uses the keyless DuckDuckGo HTML
/// search endpoint and plain HTTPS GET with redirects.
pub trait WebTransportPort: Send + Sync {
    fn search(&self, query: &str, maximum_results: usize)
    -> Result<Vec<WebSearchResultV1>, String>;

    fn fetch(
        &self,
        url: &str,
        maximum_download_bytes: usize,
    ) -> Result<(String, String, u64), String>;
}

#[derive(Clone)]
pub struct WebTools {
    transport: Arc<dyn WebTransportPort>,
}

impl WebTools {
    #[must_use]
    pub fn production() -> Self {
        Self::new(Arc::new(ProductionWebTransport))
    }

    #[must_use]
    pub fn new(transport: Arc<dyn WebTransportPort>) -> Self {
        Self { transport }
    }

    pub fn search_v1(
        &self,
        query: &str,
        maximum_results: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WebSearchResultV1>, WebToolError> {
        if query.trim().is_empty() || query.len() > MAXIMUM_QUERY_BYTES || query.contains('\0') {
            return Err(WebToolError::InvalidQuery);
        }
        if maximum_results == 0 || maximum_results > MAXIMUM_SEARCH_RESULTS {
            return Err(WebToolError::InvalidBound);
        }
        check_cancelled(cancellation)?;
        let mut results = self
            .transport
            .search(query, maximum_results)
            .map_err(WebToolError::Transport)?;
        results.truncate(maximum_results);
        Ok(results)
    }

    pub fn fetch_v1(
        &self,
        url: &str,
        maximum_download_bytes: usize,
        maximum_extract_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<WebFetchResultV1, WebToolError> {
        let parsed = parse_https_url(url)?;
        if maximum_download_bytes == 0
            || maximum_download_bytes > MAXIMUM_DOWNLOAD_BYTES
            || maximum_extract_bytes == 0
            || maximum_extract_bytes > MAXIMUM_EXTRACT_BYTES
        {
            return Err(WebToolError::InvalidBound);
        }
        check_cancelled(cancellation)?;
        let (title, text, bytes_downloaded) = self
            .transport
            .fetch(parsed.as_str(), maximum_download_bytes)
            .map_err(WebToolError::Transport)?;
        let text = truncate_utf8(text, maximum_extract_bytes);
        Ok(WebFetchResultV1 {
            url: parsed,
            title: truncate_utf8(title, 512),
            text,
            bytes_downloaded,
        })
    }
}

fn parse_https_url(url: &str) -> Result<String, WebToolError> {
    if url.len() > 4096 || url.contains('\0') {
        return Err(WebToolError::InvalidUrl);
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| WebToolError::InvalidUrl)?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err(WebToolError::InvalidUrl);
    }
    Ok(parsed.to_string())
}

struct ProductionWebTransport;

impl WebTransportPort for ProductionWebTransport {
    fn search(
        &self,
        query: &str,
        maximum_results: usize,
    ) -> Result<Vec<WebSearchResultV1>, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("Aworkit/1.0 web-search")
            .build()
            .map_err(|error| format!("web client unavailable: {error}"))?;
        let response = client
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", query)])
            .send()
            .map_err(|error| format!("web search failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("web search failed: HTTP {status}"));
        }
        let body = response
            .text()
            .map_err(|error| format!("web search response unreadable: {error}"))?;
        Ok(parse_duckduckgo_html(&body, maximum_results))
    }

    fn fetch(
        &self,
        url: &str,
        maximum_download_bytes: usize,
    ) -> Result<(String, String, u64), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("Aworkit/1.0 web-fetch")
            .build()
            .map_err(|error| format!("web client unavailable: {error}"))?;
        let response = client
            .get(url)
            .send()
            .map_err(|error| format!("web fetch failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("web fetch failed: HTTP {status}"));
        }
        let mut body = Vec::new();
        let mut reader = response;
        loop {
            let mut chunk = [0_u8; 8192];
            let count = std::io::Read::read(&mut reader, &mut chunk)
                .map_err(|error| format!("web fetch stream failed: {error}"))?;
            if count == 0 {
                break;
            }
            if body.len().saturating_add(count) > maximum_download_bytes {
                return Err(format!(
                    "web fetch exceeded the {} KiB download bound",
                    maximum_download_bytes / 1024
                ));
            }
            body.extend_from_slice(&chunk[..count]);
        }
        let bytes_downloaded = body.len() as u64;
        let text = String::from_utf8_lossy(&body).into_owned();
        Ok((
            extract_title(&text),
            extract_plain_text(&text),
            bytes_downloaded,
        ))
    }
}

fn parse_duckduckgo_html(html: &str, maximum_results: usize) -> Vec<WebSearchResultV1> {
    let mut results = Vec::new();
    let mut cursor = 0_usize;
    while results.len() < maximum_results {
        let Some(link_start) = html[cursor..].find("result__a") else {
            break;
        };
        let link_start = cursor + link_start;
        let Some(href_start) = html[link_start..].find("href=\"") else {
            cursor = link_start + 1;
            continue;
        };
        let href_start = link_start + href_start + "href=\"".len();
        let Some(href_end) = html[href_start..].find('"') else {
            break;
        };
        let href = &html[href_start..href_start + href_end];
        let Some(title_start) = html[href_start + href_end..].find('>') else {
            break;
        };
        let title_start = href_start + href_end + title_start + 1;
        let Some(title_end_rel) = html[title_start..].find("</a>") else {
            break;
        };
        let title = html[title_start..title_start + title_end_rel].to_owned();
        let snippet = html[title_start + title_end_rel..]
            .find("result__snippet")
            .and_then(|offset| {
                let offset = title_start + title_end_rel + offset;
                let start = html[offset..].find('>')? + offset + 1;
                let end = html[start..].find("</a>")?;
                Some(html[start..start + end].to_owned())
            })
            .unwrap_or_default();
        let url = reqwest::Url::parse(href)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "uddg")
                    .map(|(_, value)| value.to_string())
            })
            .or_else(|| Some(href.to_owned()))
            .filter(|url| url.starts_with("http"))
            .unwrap_or_else(|| href.to_owned());
        if !title.trim().is_empty() {
            results.push(WebSearchResultV1 {
                title: strip_tags(&title),
                url: url.trim().to_owned(),
                snippet: strip_tags(&snippet),
            });
        }
        cursor = title_start + title_end_rel + 1;
    }
    results
}

fn extract_title(html: &str) -> String {
    html.find("<title")
        .and_then(|start| {
            let content = html[start..].find('>')? + start + 1;
            let end = html[content..].find("</title>")?;
            Some(strip_tags(&html[content..content + end]))
        })
        .unwrap_or_default()
}

fn extract_plain_text(html: &str) -> String {
    let without_blocks = strip_blocks(html);
    let mut text = String::with_capacity(without_blocks.len());
    let mut in_tag = false;
    for character in without_blocks.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            other if !in_tag => text.push(other),
            _ => {}
        }
    }
    decode_entities(&collapse_whitespace(&text))
}

fn strip_blocks(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut remaining = html;
    while let Some(start) = remaining.find('<') {
        output.push_str(&remaining[..start]);
        let tag_end = remaining[start..]
            .find('>')
            .map(|end| start + end + 1)
            .unwrap_or(remaining.len());
        let tag = &remaining[start..tag_end];
        let lower = tag.to_ascii_lowercase();
        let block_name = if lower.starts_with("<script") {
            Some("script")
        } else if lower.starts_with("<style") {
            Some("style")
        } else {
            None
        };
        if let Some(name) = block_name {
            let after = &remaining[tag_end..];
            let needle = format!("</{name}");
            let lower_after = after.to_ascii_lowercase();
            match lower_after.find(&needle) {
                Some(offset) => {
                    let close_end = after[offset..]
                        .find('>')
                        .map(|end| offset + end + 1)
                        .unwrap_or(after.len());
                    remaining = &after[close_end..];
                    continue;
                }
                None => break,
            }
        }
        output.push(' ');
        remaining = &remaining[tag_end..];
    }
    output.push_str(remaining);
    output
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(text: &str) -> String {
    let mut decoded = text.replace("&amp;", "&");
    for (entity, replacement) in [
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&nbsp;", " "),
        ("&ndash;", "–"),
        ("&mdash;", "—"),
    ] {
        decoded = decoded.replace(entity, replacement);
    }
    decoded
}

fn strip_tags(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            other if !in_tag => output.push(other),
            _ => {}
        }
    }
    decode_entities(&output.trim().to_owned())
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> String {
    while value.len() > maximum_bytes {
        let mut boundary = maximum_bytes;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
        value.push('…');
        if value.len() <= maximum_bytes {
            break;
        }
        value.pop();
    }
    value
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), WebToolError> {
    if cancellation.is_cancelled() {
        Err(WebToolError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebToolError {
    #[error("web query is invalid")]
    InvalidQuery,
    #[error("web URL must be an HTTPS address")]
    InvalidUrl,
    #[error("web resource bound is invalid")]
    InvalidBound,
    #[error("web request failed: {0}")]
    Transport(String),
    #[error("web request was cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FakeTransport {
        search_results: Vec<WebSearchResultV1>,
        fetched: (String, String, u64),
    }

    impl WebTransportPort for FakeTransport {
        fn search(
            &self,
            _query: &str,
            maximum_results: usize,
        ) -> Result<Vec<WebSearchResultV1>, String> {
            Ok(self
                .search_results
                .iter()
                .take(maximum_results)
                .cloned()
                .collect())
        }

        fn fetch(
            &self,
            _url: &str,
            _maximum_download_bytes: usize,
        ) -> Result<(String, String, u64), String> {
            Ok(self.fetched.clone())
        }
    }

    #[test]
    fn web_tools_enforce_bounds_cancellation_and_transport_errors() {
        let tools = WebTools::new(Arc::new(FakeTransport {
            search_results: vec![
                WebSearchResultV1 {
                    title: "One".into(),
                    url: "https://one.example".into(),
                    snippet: "first".into(),
                },
                WebSearchResultV1 {
                    title: "Two".into(),
                    url: "https://two.example".into(),
                    snippet: "second".into(),
                },
            ],
            fetched: ("Page title".into(), "plain body text".into(), 12),
        }));

        let results = tools
            .search_v1("test query", 1, &CancellationToken::default())
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "One");
        assert!(matches!(
            tools.search_v1("", 1, &CancellationToken::default()),
            Err(WebToolError::InvalidQuery)
        ));
        assert!(matches!(
            tools.search_v1("query", 0, &CancellationToken::default()),
            Err(WebToolError::InvalidBound)
        ));
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert!(matches!(
            tools.search_v1("query", 1, &cancelled),
            Err(WebToolError::Cancelled)
        ));

        let fetched = tools
            .fetch_v1(
                "https://example.com/page",
                4096,
                256,
                &CancellationToken::default(),
            )
            .expect("fetch");
        assert_eq!(fetched.title, "Page title");
        assert_eq!(fetched.text, "plain body text");
        assert_eq!(fetched.bytes_downloaded, 12);
        assert!(matches!(
            tools.fetch_v1(
                "http://insecure.example",
                4096,
                256,
                &CancellationToken::default()
            ),
            Err(WebToolError::InvalidUrl)
        ));
        assert!(matches!(
            tools.fetch_v1("https://example.com", 0, 256, &CancellationToken::default()),
            Err(WebToolError::InvalidBound)
        ));
    }

    #[test]
    fn web_fetch_truncates_extracted_text_to_the_configured_bound() {
        let tools = WebTools::new(Arc::new(FakeTransport {
            search_results: Vec::new(),
            fetched: ("Title".into(), "x".repeat(300), 300),
        }));
        let fetched = tools
            .fetch_v1(
                "https://example.com",
                4096,
                10,
                &CancellationToken::default(),
            )
            .expect("fetch");
        assert_eq!(fetched.bytes_downloaded, 300);
        assert!(fetched.text.len() <= 10, "{}", fetched.text.len());
    }

    #[test]
    fn duckduckgo_parser_extracts_bounded_result_tuples() {
        let html = r#"<a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fone.example">One Title</a>
            <a class="result__snippet">First snippet</a>
            <a class="result__a" href="https://two.example">Two Title</a>
            <a class="result__snippet">Second snippet</a>"#;
        let results = parse_duckduckgo_html(html, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "One Title");
        assert_eq!(results[0].url, "https://one.example");
        assert_eq!(results[0].snippet, "First snippet");
        assert_eq!(results[1].url, "https://two.example");
    }

    #[test]
    fn plain_text_extraction_drops_blocks_and_decodes_entities() {
        assert_eq!(
            extract_plain_text(
                "<html><head><title>T</title><style>.x{color:red}</style></head>\
                 <body><p>Hello &amp; goodbye</p><script>alert(1)</script></body></html>"
            ),
            "T Hello & goodbye"
        );
    }
}
