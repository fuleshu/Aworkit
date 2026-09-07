//! Feed behavior is checked at extraction boundaries without external requests.
use super::*;

fn source(body: &str, mime: &str) -> WebSourceV1 {
    WebSourceV1 {
        final_url: "https://example.com/news/feed/".into(),
        body: body.into(),
        content_type: mime.into(),
        bytes_downloaded: body.len() as u64,
        truncated: false,
        warning: None,
        title: None,
    }
}
fn run(body: &str, mime: &str, mode: WebFeedContentV1) -> Extraction {
    extract(&source(body, mime), mime, mode).unwrap().unwrap()
}
const RSS: &str = r#"<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel><title>News &amp; Views</title><link>https://example.com/</link><lastBuildDate>Mon, 07 Sep 2026 12:00:00 GMT</lastBuildDate><item><title>First &amp; best</title><link>../one</link><dc:creator>Ada</dc:creator><pubDate>Mon, 07 Sep 2026 10:00:00 GMT</pubDate><content:encoded><![CDATA[<p>Article body <strong>detail</strong>.</p><script>evil()</script><img src="https://tracker.example/pixel"><a href="/details">Read more</a><a href="javascript:alert(1)">bad link</a>]]></content:encoded></item><item><title>Second</title><link>https://example.com/two</link></item></channel></rss>"#;

#[test]
fn rss_metadata_is_compact_ordered_and_full_content_is_explicit() {
    let listing = run(RSS, "application/rss+xml", WebFeedContentV1::Metadata);
    assert_eq!(listing.title, "News & Views");
    assert_eq!(listing.feed.as_ref().unwrap().entries, 2);
    for expected in [
        "First & best",
        "Ada",
        "2026-09-07T10:00:00+00:00",
        "https://example.com/news/one",
        "Second",
        "2026-09-07T12:00:00+00:00",
    ] {
        assert!(listing.text.contains(expected), "{}", listing.text);
    }
    assert!(listing.text.find("First").unwrap() < listing.text.find("Second").unwrap());
    assert!(!listing.text.contains("Article body"));
    assert!(listing.text.len() < 1000);
    let full = run(RSS, "application/rss+xml", WebFeedContentV1::Full);
    assert!(
        full.text.contains("Article body **detail**"),
        "{}",
        full.text
    );
    assert!(full.text.contains("https://example.com/details"));
    for omitted in ["evil()", "tracker.example", "javascript:", "<img", "<rss"] {
        assert!(!full.text.contains(omitted), "{}", full.text);
    }
}

#[test]
fn atom_resolves_xml_base_xhtml_and_inherited_authors() {
    let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.com/blog/"><title type="html">A &amp;amp; B</title><author><name>Feed author</name></author><entry><id>urn:one</id><title>Atom story</title><updated>2026-09-07T12:30:00Z</updated><link rel="self" href="entry.atom"/><link href="one"/><content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>Atom body</p></div></content></entry></feed>"#;
    let listing = run(xml, "application/xml", WebFeedContentV1::Metadata);
    for expected in [
        "A & B",
        "Feed author",
        "https://example.com/blog/one",
        "2026-09-07T12:30:00+00:00",
    ] {
        assert!(listing.text.contains(expected), "{}", listing.text);
    }
    assert!(!listing.text.contains("entry.atom"));
    assert!(!listing.text.contains("Published:"));
    assert!(!listing.text.contains("Atom body"));
    assert!(
        run(xml, "text/xml", WebFeedContentV1::Full)
            .text
            .contains("Atom body")
    );
}

#[test]
fn rss_one_and_generic_xml_detection_preserve_non_feeds() {
    let xml = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel rdf:about="https://example.com/feed"><title>RSS One</title><link>https://example.com/</link><description>News</description></channel><item rdf:about="https://example.com/one"><title>Old format</title><link>https://example.com/one</link><dc:creator>Grace</dc:creator><dc:date>2026-09-07T10:00:00Z</dc:date></item></rdf:RDF>"#;
    let result = run(xml, "application/rdf+xml", WebFeedContentV1::Metadata);
    assert!(result.text.contains("Old format"));
    assert!(result.text.contains("Grace"));
    assert!(
        extract(
            &source(
                "<settings><title>Keep me</title></settings>",
                "application/xml"
            ),
            "application/xml",
            WebFeedContentV1::Metadata
        )
        .unwrap()
        .is_none()
    );
    assert!(
        run(RSS, "text/plain", WebFeedContentV1::Metadata)
            .text
            .contains("First")
    );
}

#[test]
fn truncated_or_malformed_feed_retains_only_complete_entries() {
    for suffix in ["<item><title>Broken", "<item><title>Broken</item>"] {
        let prefix = RSS.split("<item><title>Second").next().unwrap();
        let result = run(
            &format!("{prefix}{suffix}"),
            "application/rss+xml",
            WebFeedContentV1::Metadata,
        );
        assert_eq!(result.feed.as_ref().unwrap().entries, 1);
        assert!(result.feed.as_ref().unwrap().incomplete);
        assert!(result.text.contains("First"));
        assert!(!result.text.contains("Broken"));
        assert!(!result.warnings.is_empty());
    }
}

#[test]
fn bounded_metadata_and_summary_fallback_preserve_readable_text() {
    let xml = format!(
        "<rss version='2.0'><channel><title>A [B]</title><item><title>{}</title><description><![CDATA[<p>Summary <b>details</b></p>]]></description></item></channel></rss>",
        "α".repeat(600)
    );
    let listing = run(&xml, "application/rss+xml", WebFeedContentV1::Metadata);
    assert_eq!(listing.title, "A [B]");
    assert!(listing.text.contains("# A \\[B\\]"));
    assert!(listing.text.len() < 1000);
    assert!(!listing.warnings.is_empty());
    assert!(!listing.text.contains("Summary"));
    let full = run(&xml, "application/rss+xml", WebFeedContentV1::Full);
    assert!(full.text.contains("Summary **details**"));
}

#[test]
fn hostile_xml_and_unsafe_links_do_not_escape_the_extractor() {
    let dtd = r#"<!DOCTYPE rss [<!ENTITY secret SYSTEM "file:///C:/private">]><rss><channel><title>&secret;</title></channel></rss>"#;
    assert!(
        extract(
            &source(dtd, "application/rss+xml"),
            "application/rss+xml",
            WebFeedContentV1::Metadata
        )
        .err()
        .unwrap()
        .contains("DTD")
    );
    let deep = format!("<rss><channel>{}x{}", "<a>".repeat(70), "</a>".repeat(70));
    assert!(
        extract(
            &source(&deep, "application/rss+xml"),
            "application/rss+xml",
            WebFeedContentV1::Metadata
        )
        .is_err()
    );
    let bad = RSS.replace("../one", "javascript:alert(1)");
    assert!(
        !run(&bad, "application/rss+xml", WebFeedContentV1::Metadata)
            .text
            .contains("javascript:")
    );
    assert!(
        run(
            "<rss version='2.0'><channel><title>Empty</title></channel></rss>",
            "application/rss+xml",
            WebFeedContentV1::Metadata
        )
        .text
        .contains("Entries: 0")
    );
}
