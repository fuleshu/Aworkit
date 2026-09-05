//! Native implementation of the capability-owned web rendering port.
//! Each invocation owns an ephemeral hidden window, private callback channels,
//! and a deadline. It shares neither the main window's profile nor its IPC authority.

use aworkit_capability_host::{CancellationToken, WebRenderSnapshotV1, WebRendererPort};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
mod profile;

#[cfg(debug_assertions)]
pub mod qa;

const RENDER_TIMEOUT: Duration = Duration::from_secs(15);
static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);

/// One renderer service per desktop runtime, with at most one active background page.
pub struct NativeWebRenderer {
    app: tauri::AppHandle,
    slot: Mutex<()>,
    #[cfg(debug_assertions)]
    fixture_origin: Option<url::Origin>,
    #[cfg(debug_assertions)]
    fixture_profiles: Mutex<Vec<std::path::PathBuf>>,
}

impl NativeWebRenderer {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            slot: Mutex::new(()),
            #[cfg(debug_assertions)]
            fixture_origin: None,
            #[cfg(debug_assertions)]
            fixture_profiles: Mutex::new(Vec::new()),
        }
    }

    fn allowed(&self, url: &url::Url) -> bool {
        #[cfg(debug_assertions)]
        if self
            .fixture_origin
            .as_ref()
            .is_some_and(|origin| origin == &url.origin())
        {
            return true;
        }
        allowed_url(url)
    }
}

fn allowed_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && !matches!(
            url.host_str(),
            Some("tauri.localhost" | "ipc.localhost" | "asset.localhost")
        )
        && url.username().is_empty()
        && url.password().is_none()
}

/// Drop closes the page on success, failure, cancellation, and early return.
struct RenderWindow {
    app: tauri::AppHandle,
    label: String,
    abandoned: Arc<AtomicBool>,
}
impl Drop for RenderWindow {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::SeqCst);
        let app = self.app.clone();
        let label = self.label.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.destroy();
            }
        });
    }
}

impl WebRendererPort for NativeWebRenderer {
    fn render(
        &self,
        url: &str,
        maximum_snapshot_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<WebRenderSnapshotV1, String> {
        let url = url::Url::parse(url).map_err(|_| "invalid rendering URL")?;
        if !self.allowed(&url) {
            return Err("rendering requires HTTPS without credentials".into());
        }
        let started = Instant::now();
        let _slot = loop {
            check_active(cancellation, started)?;
            match self.slot.try_lock() {
                Ok(slot) => break slot,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(_) => return Err("web renderer lock unavailable".into()),
            }
        };
        // A separate temporary profile is held until window destruction has been dispatched.
        let profile = Arc::new(profile::WebProfile::create()?);
        #[cfg(debug_assertions)]
        if self.fixture_origin.is_some() {
            self.fixture_profiles
                .lock()
                .map_err(|_| "fixture profile lock unavailable")?
                .push(profile.path().into());
        }
        let label = format!(
            "web-extract-{}",
            NEXT_WINDOW.fetch_add(1, Ordering::Relaxed)
        );
        let abandoned = Arc::new(AtomicBool::new(false));
        let _cleanup = RenderWindow {
            app: self.app.clone(),
            label: label.clone(),
            abandoned: abandoned.clone(),
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let app = self.app.clone();
        #[cfg(debug_assertions)]
        let fixture_origin = self.fixture_origin.clone();
        self.app
            .run_on_main_thread(move || {
                if abandoned.load(Ordering::SeqCst) {
                    return;
                }
                let profile_hold = profile.clone();
                let result = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(url))
                    .title("Web extraction")
                    .visible(false)
                    .focused(false)
                    .skip_taskbar(true)
                    .inner_size(1200.0, 900.0)
                    .incognito(true)
                    .data_directory(profile.path().to_path_buf())
                    .disable_drag_drop_handler()
                    .on_navigation(move |url| {
                        #[cfg(debug_assertions)]
                        if fixture_origin
                            .as_ref()
                            .is_some_and(|origin| origin == &url.origin())
                        {
                            return true;
                        }
                        allowed_url(url)
                    })
                    .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
                    .on_download(|_, _| false)
                    .initialization_script(include_str!("web_renderer/observe.js"))
                    .build()
                    .map_err(|e| format!("native rendering unavailable: {e}"));
                if let Ok(window) = &result {
                    // The profile stays alive for the complete native window lifecycle.
                    window.on_window_event(move |_| {
                        let _ = &profile_hold;
                    });
                    if abandoned.load(Ordering::SeqCst) {
                        let _ = window.destroy();
                        return;
                    }
                }
                let _ = tx.try_send(result);
            })
            .map_err(|e| e.to_string())?;
        let window = receive(&rx, cancellation, started)??;
        let mut latest: Option<WebRenderSnapshotV1> = None;
        loop {
            check_active(cancellation, started)?;
            // Capture at most once per poll; no unbounded queue of pending evaluations.
            let script = include_str!("web_renderer/snapshot.js")
                .replace("__MAX_BYTES__", &maximum_snapshot_bytes.to_string());
            let (tx, rx) = mpsc::sync_channel(1);
            window
                .eval_with_callback(script, move |value| {
                    let _ = tx.try_send(value);
                })
                .map_err(|e| e.to_string())?;
            let response = match receive(&rx, cancellation, started) {
                Ok(value) => value,
                Err(_) if !cancellation.is_cancelled() && latest.is_some() => {
                    return Ok(latest.unwrap());
                }
                Err(error) => return Err(error),
            };
            if response.len()
                > maximum_snapshot_bytes
                    .saturating_mul(6)
                    .saturating_add(8192)
            {
                return Err("render callback exceeded its bound".into());
            }
            let value: serde_json::Value =
                serde_json::from_str(&response).map_err(|_| "invalid render callback")?;
            if let Some(error) = value["error"].as_str() {
                return Err(format!("DOM capture failed: {error}"));
            }
            if let (Some(html), Some(final_url)) = (value["html"].as_str(), value["url"].as_str()) {
                let final_parsed =
                    url::Url::parse(final_url).map_err(|_| "invalid rendered URL")?;
                if !self.allowed(&final_parsed) {
                    return Err("rendered URL is outside the allowed web schemes".into());
                }
                let mut snapshot = WebRenderSnapshotV1 {
                    final_url: final_url.into(),
                    html: html.into(),
                    truncated: value["truncated"].as_bool().unwrap_or(true),
                    settled: value["settled"].as_bool().unwrap_or(false),
                };
                if snapshot.html.len() > maximum_snapshot_bytes {
                    return Err("rendered snapshot exceeded its bound".into());
                }
                if snapshot.settled && started.elapsed() >= Duration::from_secs(1) {
                    return Ok(snapshot);
                }
                snapshot.settled = false;
                latest = Some(snapshot);
            }
            if started.elapsed() + Duration::from_millis(300) >= RENDER_TIMEOUT {
                return latest.ok_or_else(|| {
                    "rendering deadline expired before a DOM snapshot was available".into()
                });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn check_active(cancellation: &CancellationToken, started: Instant) -> Result<(), String> {
    if cancellation.is_cancelled() {
        return Err("web rendering was cancelled".into());
    }
    if started.elapsed() >= RENDER_TIMEOUT {
        return Err("web rendering deadline expired".into());
    }
    Ok(())
}

fn receive<T>(
    rx: &mpsc::Receiver<T>,
    cancellation: &CancellationToken,
    started: Instant,
) -> Result<T, String> {
    loop {
        check_active(cancellation, started)?;
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("native rendering callback was disconnected".into()),
        }
    }
}
