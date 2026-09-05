//! Deterministic local extraction and conservative rendering assessment.
//! Article scoring is used only for article-shaped pages; indexes keep their links.

use super::document::{WebExtractionQualityV1 as Quality, WebSourceV1};
use dom_query::Document;
use dom_smoothie::{Config, Readability};

pub(super) struct Extraction {
    pub title: String,
    pub text: String,
    pub quality: Quality,
    pub method: &'static str,
}

pub(super) fn extract(source: &WebSourceV1) -> Result<Extraction, String> {
    let mime = source.content_type.split(';').next().unwrap_or("").trim();
    if mime != "text/html" && mime != "application/xhtml+xml" {
        if mime.starts_with("text/")
            || matches!(
                mime,
                "application/json" | "application/xml" | "application/javascript"
            )
        {
            let text = source.body.trim().to_owned();
            let quality = if text.is_empty() {
                Quality::Empty
            } else {
                Quality::Usable
            };
            return Ok(Extraction {
                title: source.title.clone().unwrap_or_default(),
                text,
                quality,
                method: "text",
            });
        }
        return Err(format!(
            "unsupported web content type: {mime}; use a document-specific tool"
        ));
    }
    let document = Document::from(source.body.as_str());
    // Bound expensive article scoring on hostile/wasteful DOMs.
    let article_shape = document.select("article").length() == 1
        || document
            .select("meta[property='og:type'][content='article']")
            .length()
            > 0;
    let title = document.select("title").text().trim().to_owned();
    let scripts = document.select("script").length();
    document
        .select("script,style,template,noscript,svg,canvas,iframe,[hidden],[aria-hidden='true']")
        .remove();
    // Resolve links before either extraction path, including <base href>.
    let base = reqwest::Url::parse(&source.final_url).map_err(|_| "invalid final URL")?;
    let base = document
        .select("base[href]")
        .attr("href")
        .and_then(|href| base.join(&href).ok())
        .unwrap_or(base);
    for link in document.select("a[href],img[src]").iter() {
        for attribute in ["href", "src"] {
            if let Some(value) = link.attr(attribute) {
                match base.join(&value) {
                    Ok(url) if matches!(url.scheme(), "http" | "https" | "mailto") => {
                        link.set_attr(attribute, url.as_str())
                    }
                    _ => link.remove_attr(attribute),
                }
            }
        }
    }
    let main = document.select("main,[role='main']");
    let html = if main.length() == 1 {
        main.html().to_string()
    } else {
        document.select("body").html().to_string()
    };
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "head", "script", "style", "img", "input", "button", "select", "textarea",
        ])
        .build();
    let broad = converter
        .convert(&html)
        .map_err(|e| format!("HTML extraction failed: {e}"))?;
    let mut result = Extraction {
        title,
        text: broad.trim().to_owned(),
        quality: Quality::Usable,
        method: "structuredHtml",
    };
    if article_shape && document.select("*").length() <= 50_000 {
        let config = Config {
            max_elements_to_parse: 50_000,
            ..Config::default()
        };
        if let Ok(mut reader) =
            Readability::new(html.as_str(), Some(&source.final_url), Some(config))
        {
            if let Ok(article) = reader.parse() {
                if article.length >= 200 {
                    if let Ok(markdown) = converter.convert(&article.content) {
                        if !markdown.trim().is_empty() {
                            result.text = markdown.trim().to_owned();
                            result.method = "readability";
                            if !article.title.is_empty() {
                                result.title = article.title;
                            }
                        }
                    }
                }
            }
        }
    }
    let visible = document.select("body").text().to_lowercase();
    let words = visible.split_whitespace().count();
    let blocked = words < 150
        && [
            "verify you are human",
            "checking your browser",
            "access denied",
            "enable cookies",
            "please complete the captcha",
            "subscribe to continue",
            "sign in to continue",
        ]
        .iter()
        .any(|s| visible.contains(s));
    let meaningful = document.select("h1,h2,p,a[href],pre,table,li").length();
    let shell = scripts > 0
        && ((words < 12 && meaningful == 0)
            || (words < 80
                && [
                    "enable javascript",
                    "javascript is required",
                    "loading...",
                    "loading…",
                ]
                .iter()
                .any(|s| visible.contains(s))));
    result.quality = if blocked {
        Quality::Blocked
    } else if shell {
        Quality::NeedsRendering
    } else if result.text.is_empty() {
        Quality::Empty
    } else {
        Quality::Usable
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(html: &str) -> WebSourceV1 {
        WebSourceV1 {
            final_url: "https://example.com/news/".into(),
            body: html.into(),
            content_type: "text/html".into(),
            bytes_downloaded: html.len() as u64,
            truncated: false,
            warning: None,
            title: None,
        }
    }
    #[test]
    fn index_preserves_structure_entities_and_absolute_links() {
        let result = extract(&source("<title>News</title><main><h1>Today &#x2600;</h1><ul><li><a href='../one'>One</a></li><li><a href='/two'>Two</a></li></ul><pre><code>a &lt; b\n  c</code></pre><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table><script>bad()</script></main>")).unwrap();
        assert!(result.text.contains("# Today ☀"), "{}", result.text);
        assert!(result.text.contains("[One](https://example.com/one)"));
        assert!(result.text.contains("a < b"));
        assert!(result.text.contains("|"));
        assert!(!result.text.contains("bad()"));
        assert_eq!(result.quality, Quality::Usable);
    }
    #[test]
    fn assessment_does_not_equate_short_pages_or_challenges_with_js_shells() {
        assert_eq!(
            extract(&source("<p>42</p>")).unwrap().quality,
            Quality::Usable
        );
        assert_eq!(
            extract(&source(
                "<div>Loading...</div><script src='/app.js'></script>"
            ))
            .unwrap()
            .quality,
            Quality::NeedsRendering
        );
        assert_eq!(
            extract(&source(
                "<p>Verify you are human</p><script>challenge()</script>"
            ))
            .unwrap()
            .quality,
            Quality::Blocked
        );
    }

    #[test]
    fn article_scoring_keeps_prose_and_code() {
        let paragraph = "The engineering team measured the device carefully, compared the results, and documented the practical consequences. ".repeat(6);
        let html = format!(
            "<title>Engineering report</title><nav>Account Settings Buy</nav><main><article><h1>Engineering report</h1><p>{paragraph}</p><p>{paragraph}</p><pre><code>result = 42;</code></pre></article></main>"
        );
        let result = extract(&source(&html)).unwrap();
        assert_eq!(result.method, "readability");
        assert!(result.text.contains("practical consequences"));
        assert!(result.text.contains("result = 42;"));
        assert!(!result.text.contains("Account Settings Buy"));
    }
}
