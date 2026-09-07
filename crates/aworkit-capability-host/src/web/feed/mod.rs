//! Native syndication extraction shared by single- and multi-URL web tools.
//! feed-rs maps RSS 0/1/2 and Atom namespaces into one model; no secondary fetches.
mod format;
#[cfg(test)]
mod tests;
mod xml;

use super::{
    WebExtractionQualityV1, WebFeedContentV1, WebFeedMetadataV1, WebSourceV1,
    extraction::Extraction,
};

pub(super) fn extract(
    source: &WebSourceV1,
    mime: &str,
    content: WebFeedContentV1,
) -> Result<Option<Extraction>, String> {
    let required = matches!(
        mime,
        "application/rss+xml" | "application/atom+xml" | "application/rdf+xml"
    );
    if !required
        && !matches!(
            mime,
            "application/xml" | "text/xml" | "text/plain" | "application/octet-stream"
        )
    {
        return Ok(None);
    }
    let Some(prepared) = xml::prepare(&source.body, required)? else {
        return Ok(None);
    };
    // Transport has already decoded the source to UTF-8. Do not let an original
    // ISO-8859-1/etc declaration make the feed parser decode those bytes twice.
    let mut xml = prepared.xml.trim_start_matches('\u{feff}').trim_start();
    if xml
        .strip_prefix("<?xml")
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    {
        if let Some(end) = xml.find("?>") {
            xml = &xml[end + 2..];
        }
    }
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(&source.final_url))
        .build()
        .parse(xml.as_bytes())
        .map_err(|error| format!("RSS/Atom feed parsing failed: {error}"))?;
    let (title, text, clipped) = format::render(&feed, content, &source.final_url);
    let mut warnings = Vec::new();
    if prepared.incomplete {
        warnings.push("Feed XML was incomplete or malformed; only complete fields and entries before the break were retained.".into());
    }
    if clipped {
        warnings.push("Oversized feed titles or author fields were shortened to 512 bytes.".into());
    }
    Ok(Some(Extraction {
        title,
        text,
        quality: WebExtractionQualityV1::Usable,
        method: "feed",
        feed: Some(WebFeedMetadataV1 {
            format: format!("{:?}", feed.feed_type),
            entries: feed.entries.len(),
            content,
            incomplete: prepared.incomplete || source.truncated,
        }),
        warnings,
    }))
}
