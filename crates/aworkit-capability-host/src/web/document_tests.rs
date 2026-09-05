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
