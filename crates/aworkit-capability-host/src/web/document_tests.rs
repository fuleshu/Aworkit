//! Behavioral tests of retrieval decisions, partial preservation, and cancellation.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Source(WebSourceV1);
impl WebTransportPort for Source {
    fn search(&self, _: &str, _: usize) -> Result<Vec<WebSearchResultV1>, String> {
        Ok(vec![])
    }
    fn fetch(&self, _: &str, _: usize) -> Result<(String, String, u64), String> {
        unreachable!()
    }
    fn fetch_document(
        &self,
        _: &str,
        _: usize,
        _: &CancellationToken,
    ) -> Result<WebSourceV1, String> {
        Ok(self.0.clone())
    }
}
struct Renderer {
    calls: AtomicUsize,
    body: Option<String>,
    cancel: bool,
}
impl WebRendererPort for Renderer {
    fn render(
        &self,
        url: &str,
        _: usize,
        cancellation: &CancellationToken,
    ) -> Result<WebRenderSnapshotV1, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.cancel {
            cancellation.cancel();
        }
        match &self.body {
            Some(body) => Ok(WebRenderSnapshotV1 {
                final_url: url.into(),
                html: body.clone(),
                truncated: false,
                settled: true,
            }),
            None => Err("fixture rendering unavailable".into()),
        }
    }
}
fn source(html: &str, truncated: bool) -> WebSourceV1 {
    WebSourceV1 {
        final_url: "https://example.com/".into(),
        body: html.into(),
        content_type: "text/html".into(),
        bytes_downloaded: html.len() as u64,
        truncated,
        warning: None,
        title: None,
    }
}

#[test]
fn one_fallback_for_complete_shell_and_never_for_truncated_download() {
    for (truncated, allowed, expected) in [(false, true, 1), (true, true, 0), (false, false, 0)] {
        let renderer = Arc::new(Renderer {
            calls: AtomicUsize::new(0),
            body: Some("<h1>Rendered</h1><p>Useful article body</p>".into()),
            cancel: false,
        });
        let tools = WebTools::new(Arc::new(Source(source(
            "<div>Loading...</div><script src='/app'></script>",
            truncated,
        ))))
        .with_renderer(renderer.clone());
        let document = tools
            .document_v1(
                "https://example.com",
                8192,
                allowed,
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(renderer.calls.load(Ordering::SeqCst), expected);
        assert_eq!(document.metadata.download_truncated, truncated);
        if expected == 1 {
            assert!(document.text.contains("Useful article body"));
            assert!(document.metadata.method.starts_with("webview/"));
        } else {
            assert!(document.text.contains("Loading"));
            assert!(!document.metadata.warnings.is_empty());
        }
    }
}

#[test]
fn rendering_failure_preserves_original_and_cancel_propagates() {
    for body in [None, Some("<p>Access denied</p>".into())] {
        let renderer = Arc::new(Renderer {
            calls: AtomicUsize::new(0),
            body,
            cancel: false,
        });
        let tools = WebTools::new(Arc::new(Source(source(
            "<div>Loading...</div><script></script>",
            false,
        ))))
        .with_renderer(renderer);
        let document = tools
            .document_v1(
                "https://example.com",
                8192,
                true,
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(document.text.contains("Loading"));
        assert!(!document.metadata.warnings.is_empty());
    }
    let renderer = Arc::new(Renderer {
        calls: AtomicUsize::new(0),
        body: None,
        cancel: true,
    });
    let tools = WebTools::new(Arc::new(Source(source(
        "<div>Loading...</div><script></script>",
        false,
    ))))
    .with_renderer(renderer);
    assert!(matches!(
        tools.document_v1(
            "https://example.com",
            8192,
            true,
            &CancellationToken::default()
        ),
        Err(WebToolError::Cancelled)
    ));
}

#[test]
fn http_prefix_exact_eof_chunked_overflow_and_unicode_are_explicit() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    for (chunked, body, cap, truncated) in [
        (false, "αβγ", 6, false),
        (false, "αβγ", 4, true),
        (true, "αβγ", 4, true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let response = if chunked {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
                    body.len(),
                    body
                )
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            stream.write_all(response.as_bytes()).unwrap();
        });
        // Production URL admission requires HTTPS; only this transport-level fixture uses loopback HTTP.
        let source = retrieval::retrieve(&url, cap, &CancellationToken::default()).unwrap();
        assert_eq!(source.bytes_downloaded, cap as u64);
        assert_eq!(source.truncated, truncated);
        assert!(source.body.starts_with("αβ"));
        worker.join().unwrap();
    }
}

#[test]
#[ignore = "live public website diagnostic"]
fn live_spiegel_extracts_with_explicit_completeness() {
    let tools = WebTools::production();
    for maximum in [1024 * 1024, 8 * 1024 * 1024] {
        let page = tools
            .document_v1(
                "https://www.spiegel.de/",
                maximum,
                false,
                &CancellationToken::default(),
            )
            .unwrap();
        println!(
            "cap={maximum} bytes={} extracted={} metadata={:?}",
            page.bytes_downloaded,
            page.text.len(),
            page.metadata
        );
        assert!(!page.text.is_empty());
    }
}

#[test]
fn feed_media_types_preserve_xml_and_partial_evidence_without_rendering() {
    for (mime, xml) in [
        (
            "application/rss+xml; charset=UTF-8",
            "<rss><channel><title>News αβγ</title><item><title>Story</title><link>https://example.com/story</link><description><![CDATA[<b>Useful</b> content]]></description></item></channel></rss>",
        ),
        (
            " Application/Atom+XML ; charset=utf-8",
            "<feed xmlns='http://www.w3.org/2005/Atom'><title>News αβγ</title><entry><title>Story</title><link href='https://example.com/story'/></entry></feed>",
        ),
    ] {
        for truncated in [false, true] {
            let xml = if truncated {
                &xml[..xml.len() - 10]
            } else {
                xml
            };
            let mut feed = source(xml, truncated);
            feed.content_type = mime.into();
            let renderer = Arc::new(Renderer {
                calls: AtomicUsize::new(0),
                body: None,
                cancel: false,
            });
            let tools = WebTools::new(Arc::new(Source(feed))).with_renderer(renderer.clone());
            let cancellation = CancellationToken::default();
            let document = tools
                .document_v1("https://example.com/feed", 8192, true, &cancellation)
                .unwrap();
            assert_eq!(document.text, xml);
            assert_eq!(document.metadata.quality, WebExtractionQualityV1::Usable);
            assert_eq!(document.metadata.method, "text");
            assert_eq!(document.metadata.download_truncated, truncated);
            assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);

            let fetched = tools
                .fetch_v1("https://example.com/feed", 8192, 64, &cancellation)
                .unwrap();
            assert!(xml.starts_with(&fetched.text));
            assert!(!fetched.text.is_empty());
            assert!(fetched.preview_truncated);
            assert_eq!(fetched.metadata.download_truncated, truncated);
            let pages = tools
                .extract_v1(
                    &["https://example.com/feed".into()],
                    8192,
                    64,
                    64,
                    &cancellation,
                )
                .unwrap();
            assert!(pages[0].error.is_none());
            assert_eq!(pages[0].content, fetched.text);
            assert_eq!(
                pages[0].metadata.as_ref().unwrap().download_truncated,
                truncated
            );
        }
    }
}

#[test]
fn feed_support_does_not_accept_binary_documents_as_text() {
    for mime in ["application/pdf", "image/png", "application/octet-stream"] {
        let mut binary = source("binary fixture", false);
        binary.content_type = mime.into();
        let tools = WebTools::new(Arc::new(Source(binary)));
        let error = tools
            .document_v1(
                "https://example.com/file",
                8192,
                true,
                &CancellationToken::default(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("unsupported web content type: {mime}"))
        );
    }
}
