//! Debug-only deterministic native WebView acceptance runner.
use super::*;
use aworkit_capability_host::{WebSearchResultV1, WebSourceV1, WebTools, WebTransportPort};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
};

pub fn start(app: tauri::AppHandle, report: PathBuf) {
    std::thread::spawn(move || {
        let result = run(&app);
        let value = match &result {
            Ok(value) => value.clone(),
            Err(error) => serde_json::json!({"ok":false,"error":error}),
        };
        let written = std::fs::write(
            &report,
            serde_json::to_vec_pretty(&value).unwrap_or_default(),
        )
        .is_ok();
        app.exit(if result.is_ok() && written { 0 } else { 1 });
    });
}

fn run(app: &tauri::AppHandle) -> Result<serde_json::Value, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let origin = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let server = std::thread::spawn(move || {
        while !server_stop.load(Ordering::SeqCst) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let html = r#"<!doctype html><html><head><title>Native fixture</title></head><body><div id='root'>Loading...</div><script>
                setTimeout(async()=>{
                  let blocked = !window.__TAURI_INTERNALS__;
                  if (!blocked) {
                    try { await window.__TAURI_INTERNALS__.invoke('desktop_snapshot',{afterSequence:0}); }
                    catch (error) { blocked=/Application commands are unavailable|not allowed|not permitted|not authorized|does not have permission/i.test(String(error)); }
                  }
                  document.getElementById('root').innerHTML='<main><h1>Native rendered evidence</h1><p>Delayed JavaScript content arrived. IPC blocked: '+blocked+'</p><a href="/details">Details</a></main>';
                  window.open('/popup');
                },350);
                </script></body></html>"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = stream.write_all(response.as_bytes());
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    });
    let mut renderer = NativeWebRenderer::new(app.clone());
    renderer.fixture_origin = Some(
        url::Url::parse(&origin)
            .map_err(|e| e.to_string())?
            .origin(),
    );
    let result = (|| {
        let snapshot = renderer.render(
            &format!("{origin}/page"),
            8192,
            &CancellationToken::default(),
        )?;
        if !snapshot.html.contains("Native rendered evidence")
            || !snapshot.html.contains("IPC blocked: true")
        {
            return Err(format!("native fixture failed: {}", snapshot.html));
        }
        if !snapshot.settled || snapshot.truncated {
            return Err("native snapshot completeness was incorrect".into());
        }
        let tools = WebTools::new(Arc::new(SnapshotTransport(snapshot.clone())));
        let document = tools
            .document_v1(
                "https://fixture.example/page",
                8192,
                false,
                &CancellationToken::default(),
            )
            .map_err(|e| e.to_string())?;
        if !document.text.contains("# Native rendered evidence")
            || !document
                .text
                .contains(&format!("[Details]({origin}/details)"))
        {
            return Err(format!("rendered extraction failed: {}", document.text));
        }
        let truncated =
            renderer.render(&format!("{origin}/page"), 32, &CancellationToken::default())?;
        if !truncated.truncated || truncated.html.len() > 32 {
            return Err("native snapshot limit was not enforced".into());
        }
        let cancellation = CancellationToken::default();
        let cancel_worker = cancellation.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel_worker.cancel();
        });
        let began = Instant::now();
        if renderer
            .render(&format!("{origin}/cancel"), 8192, &cancellation)
            .is_ok()
            || began.elapsed() > Duration::from_secs(2)
        {
            return Err("native cancellation did not stop rendering promptly".into());
        }
        // Native destruction includes a platform event after the UI-thread request.
        // Wait for that event, rather than mistaking its queued window for a leak.
        let cleanup_started = Instant::now();
        let windows = loop {
            let (tx, rx) = mpsc::sync_channel(1);
            let inspect = app.clone();
            app.run_on_main_thread(move || {
                let _ = tx.send(
                    inspect
                        .webview_windows()
                        .keys()
                        .filter(|label| label.starts_with("web-extract-"))
                        .count(),
                );
            })
            .map_err(|e| e.to_string())?;
            let windows = rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            if windows == 0 {
                break windows;
            }
            if cleanup_started.elapsed() > Duration::from_secs(2) {
                return Err(format!("{windows} renderer windows leaked after cleanup"));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let profiles = renderer
            .fixture_profiles
            .lock()
            .map_err(|_| "fixture profile lock")?
            .clone();
        let cleanup_started = Instant::now();
        while profiles.iter().any(|profile| profile.exists())
            && cleanup_started.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        let remaining_profiles = profiles.iter().filter(|profile| profile.exists()).count();
        if remaining_profiles > 0 {
            return Err(format!(
                "{remaining_profiles} temporary browser profiles remained after cleanup: {profiles:?}"
            ));
        }
        Ok(
            serde_json::json!({"ok":true,"nativeJavaScript":true,"structuredExtraction":true,"ipcBlocked":true,"snapshotBound":true,"cancellation":true,"remainingRendererWindows":windows,"remainingProfiles":remaining_profiles,"markdown":document.text}),
        )
    })();
    stop.store(true, Ordering::SeqCst);
    let _ = server.join();
    result
}

struct SnapshotTransport(WebRenderSnapshotV1);
impl WebTransportPort for SnapshotTransport {
    fn search(&self, _: &str, _: usize) -> Result<Vec<WebSearchResultV1>, String> {
        Err("not used".into())
    }
    fn fetch(&self, _: &str, _: usize) -> Result<(String, String, u64), String> {
        Err("not used".into())
    }
    fn fetch_document(
        &self,
        _: &str,
        _: usize,
        _: &CancellationToken,
    ) -> Result<WebSourceV1, String> {
        Ok(WebSourceV1 {
            final_url: self.0.final_url.clone(),
            body: self.0.html.clone(),
            content_type: "text/html".into(),
            bytes_downloaded: 0,
            truncated: self.0.truncated,
            warning: None,
            title: None,
        })
    }
}
