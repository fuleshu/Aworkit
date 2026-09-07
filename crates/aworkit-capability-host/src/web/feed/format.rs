//! Compact metadata listings and optional readable article bodies, never raw embeds.
use super::super::{WebFeedContentV1, document::prefix};
use feed_rs::model::{Feed, Link, Text};

pub(super) fn render(feed: &Feed, mode: WebFeedContentV1, base: &str) -> (String, String, bool) {
    let mut clipped = false;
    let title = feed
        .title
        .as_ref()
        .map(text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Untitled feed".into());
    let mut output = format!("# {}\n", field(&title, &mut clipped));
    if let Some(url) = article_link(&feed.links, base) {
        output.push_str(&format!("Website: <{url}>\n"));
    }
    if let Some(updated) = feed.updated {
        output.push_str(&format!("Feed updated: {}\n", updated.to_rfc3339()));
    }
    output.push_str(&format!("Entries: {} (feed order)\n", feed.entries.len()));
    if mode == WebFeedContentV1::Metadata {
        output.push_str("Article bodies omitted. Use article URLs, or request feedContent=full with the feed URL without documentId.\n");
    }
    for (index, entry) in feed.entries.iter().enumerate() {
        let title = entry
            .title
            .as_ref()
            .map(text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Untitled entry".into());
        output.push_str(&format!(
            "\n{}. {}\n",
            index + 1,
            field(&title, &mut clipped)
        ));
        let url = article_link(&entry.links, base);
        if let Some(url) = &url {
            output.push_str(&format!("   URL: <{url}>\n"));
        }
        if let Some(published) = entry.published {
            output.push_str(&format!("   Published: {}\n", published.to_rfc3339()));
        }
        if let Some(updated) = entry
            .updated
            .filter(|updated| Some(*updated) != entry.published)
        {
            output.push_str(&format!("   Updated: {}\n", updated.to_rfc3339()));
        }
        let authors = if entry.authors.is_empty() {
            &feed.authors
        } else {
            &entry.authors
        };
        if !authors.is_empty() {
            let names = authors
                .iter()
                .map(|author| author.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if !names.trim().is_empty() {
                output.push_str(&format!("   Author: {}\n", field(&names, &mut clipped)));
            }
        }
        if mode == WebFeedContentV1::Full {
            let body = entry
                .content
                .as_ref()
                .and_then(|content| {
                    content
                        .body
                        .as_deref()
                        .filter(|body| !body.trim().is_empty())
                        .map(|body| (body, content.content_type.as_str()))
                })
                .or_else(|| {
                    entry
                        .summary
                        .as_ref()
                        .map(|summary| (summary.content.as_str(), summary.content_type.as_str()))
                });
            if let Some((body, kind)) = body {
                let body = match kind {
                    "text/html" | "application/xhtml+xml" => {
                        markdown(body, url.as_deref().unwrap_or(base))
                    }
                    "text/plain" => body.to_owned(),
                    _ => "[Non-text embedded content omitted]".into(),
                };
                output.push('\n');
                output.push_str(body.trim());
                output.push('\n');
            }
        }
    }
    (title, output, clipped)
}

/// Human-readable fields must not introduce Markdown links, headings, or markup.
fn field(value: &str, clipped: &mut bool) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    *clipped |= value.len() > 512;
    let mut escaped = String::new();
    for ch in prefix(&value, 512).chars() {
        if "\\`*_{}[]<>#|".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    if value.len() > 512 {
        escaped.push('…');
    }
    escaped
}

fn text(value: &Text) -> String {
    if value.content_type.as_str() == "text/plain" {
        return value.content.clone();
    }
    let doc = dom_query::Document::from(value.content.as_str());
    doc.select("script,style,iframe,svg").remove();
    doc.select("body").text().to_string()
}

/// Only public web link schemes are emitted; self/enclosure links are not articles.
fn article_link(links: &[Link], base: &str) -> Option<String> {
    links
        .iter()
        .filter(|link| link.rel.as_deref().is_none_or(|rel| rel == "alternate"))
        .find_map(|link| safe_link(&link.href, base))
}

fn safe_link(href: &str, base: &str) -> Option<String> {
    let url = reqwest::Url::parse(base).ok()?.join(href).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.as_str().len() <= 4096)
        .then(|| url.to_string().replace('<', "%3C").replace('>', "%3E"))
}

fn markdown(html: &str, base: &str) -> String {
    let doc = dom_query::Document::from(html);
    doc.select("script,style,iframe,svg,canvas,object,embed,img,form,template")
        .remove();
    for node in doc.select("a[href]").iter() {
        if let Some(url) = node.attr("href").and_then(|href| safe_link(&href, base)) {
            node.set_attr("href", &url);
        } else {
            node.remove_attr("href");
        }
    }
    htmd::HtmlToMarkdown::builder()
        .build()
        .convert(&doc.select("body").html())
        .unwrap_or_else(|_| doc.select("body").text().to_string())
}
