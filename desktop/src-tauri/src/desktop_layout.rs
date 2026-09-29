//! Native layout lifecycle. The renderer reports separators while it is alive;
//! the host retains normal bounds and awaits the final write before destruction.
use crate::{SharedRuntime, runtime_worker};
use aworkit_desktop::runtime::LayoutConfigurationV2;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Manager, Runtime, WindowEvent};

/// Latest measurements, independent of the busy Chat coordinator.
pub struct LayoutSession {
    layout: Mutex<LayoutConfigurationV2>,
    closing: AtomicBool,
}

impl LayoutSession {
    pub fn restore(&self, layout: LayoutConfigurationV2) -> Result<(), String> {
        *self.layout.lock().map_err(|_| "desktop layout lock is unavailable")? = layout;
        Ok(())
    }
    pub fn new(layout: LayoutConfigurationV2) -> Self {
        Self {
            layout: Mutex::new(layout),
            closing: AtomicBool::new(false),
        }
    }
    fn snapshot(&self) -> Result<LayoutConfigurationV2, String> {
        self.layout
            .lock()
            .map(|layout| layout.clone())
            .map_err(|_| "desktop layout lock is unavailable".to_owned())
    }
}

/// Returns the last native layout including separators acknowledged this session.
#[tauri::command]
pub async fn desktop_layout(
    session: tauri::State<'_, LayoutSession>,
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<LayoutConfigurationV2, String> {
    runtime_worker(Arc::clone(runtime.inner()), "layout startup", |_| Ok(())).await?;
    session.snapshot()
}

/// Receives separator changes before unload. Round fractional CSS coordinates in
/// the renderer, then validate and stage synchronously before the disk worker.
#[tauri::command]
pub async fn desktop_layout_commit(
    app: AppHandle,
    runtime: tauri::State<'_, SharedRuntime>,
    session: tauri::State<'_, LayoutSession>,
    history_pane_width: Option<u32>,
    inspector_pane_width: Option<u32>,
) -> Result<(), String> {
    {
        let mut current = session
            .layout
            .lock()
            .map_err(|_| "desktop layout lock is unavailable")?;
        let mut next = current.clone();
        if let Some(width) = history_pane_width {
            next.history_pane_width = Some(width);
        }
        if let Some(width) = inspector_pane_width {
            next.inspector_pane_width = Some(width);
        }
        next.validate()?;
        *current = next;
    }
    runtime_worker(
        Arc::clone(runtime.inner()),
        "desktop layout commit",
        move |runtime| {
            // Read after acquiring the coordinator: older queued writes must never
            // overwrite a more recent drag or the final native frame.
            runtime.settings_commit_layout(app.state::<LayoutSession>().snapshot()?)
        },
    )
    .await
}

/// Keep the last normal frame when minimizing/maximizing. Never persist iconic
/// coordinates or substitute the monitor rectangle for normal restore bounds.
fn measure(window: &tauri::Window, session: &LayoutSession) -> Result<(), String> {
    if window.is_minimized().map_err(|e| e.to_string())? {
        return Ok(());
    }
    // Tao updates its cached maximized flag after Windows emits move/resize.
    // Query the HWND during those callbacks or a maximize overwrites normal
    // bounds with the monitor rectangle before the cached flag catches up.
    #[cfg(target_os = "windows")]
    let maximized = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::IsZoomed(
            window.hwnd().map_err(|e| e.to_string())?.0,
        ) != 0
    };
    #[cfg(not(target_os = "windows"))]
    let maximized = window.is_maximized().map_err(|e| e.to_string())?;
    let fullscreen = window.is_fullscreen().map_err(|e| e.to_string())?;
    let frame = if maximized || fullscreen {
        None
    } else {
        // Windows restores the Win32 outer rectangle, so its measurement is the
        // outer size. Everywhere else `set_size` applies a client size, so the
        // client size is what is captured: restoring never re-derives the
        // decoration from the live window, whose platform insets are unreliable
        // (on Linux `tao` only refreshes them from `_NET_FRAME_EXTENTS`, which
        // is empty for client-side decorations). Reading them used to grow the
        // window by its decoration on every restart, or collapse it when the
        // reported inset degenerated.
        #[cfg(target_os = "windows")]
        let size = window.outer_size().map_err(|e| e.to_string())?;
        #[cfg(not(target_os = "windows"))]
        let size = window.inner_size().map_err(|e| e.to_string())?;
        Some((
            window.outer_position().map_err(|e| e.to_string())?,
            size,
            window.scale_factor().map_err(|e| e.to_string())?,
        ))
    };
    let mut layout = session
        .layout
        .lock()
        .map_err(|_| "desktop layout lock is unavailable")?;
    if !fullscreen {
        layout.maximized = maximized;
    }
    if let Some((position, size, scale)) = frame {
        // A session that cannot place windows cannot report where this one is
        // either, so the stored position is cleared rather than filled with the
        // meaningless origin the platform hands back.
        let stored_position = session_places_windows().then_some((position.x, position.y));
        store_frame(
            &mut layout,
            stored_position,
            (size.width, size.height),
            scale,
        );
    }
    Ok(())
}

/// Whether this desktop session lets an application place its own windows.
///
/// An X11 window manager honours a move request for a mapped window, so a
/// stored position is meaningful there. A Wayland compositor owns placement
/// outright: `xdg-shell` has no position request, `gtk_window_move` does
/// nothing, and `gtk_window_get_position` always reports the origin. Reading
/// the position on Wayland therefore yields `(0, 0)` — writing that as if the
/// user had chosen it would also misplace the window in a later X11 session.
#[cfg(target_os = "linux")]
fn session_places_windows() -> bool {
    static PLACES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PLACES.get_or_init(|| {
        session_places_windows_for(
            std::env::var("GDK_BACKEND").ok().as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
        )
    })
}

/// The decision itself, with the environment passed in so it stays testable.
#[cfg(target_os = "linux")]
fn session_places_windows_for(gdk_backend: Option<&str>, wayland_display: bool) -> bool {
    // An explicit GDK_BACKEND decides which backend GDK actually uses, so it
    // wins over the session's own type (GTK4 accepts a comma-separated list).
    let requested = gdk_backend
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    match requested.as_deref() {
        Some("x11") => true,
        Some("wayland") => false,
        _ => !wayland_display,
    }
}

#[cfg(not(target_os = "linux"))]
fn session_places_windows() -> bool {
    true
}

/// Stores one measured frame, unless it would not survive validation.
///
/// A frame read before the window is realized, or from a platform cache that
/// reports a zero size or an impossible scale factor, must never replace a
/// usable one. The persisted record is validated on write, and a rejected
/// record silently keeps an older frame — which is exactly how a single bad
/// sample turns into "the window opens at the wrong size after a restart".
///
/// `position` is `None` on a session that cannot express a window position;
/// the stored one is then cleared instead of keeping a stale value.
/// Returns whether the frame was stored.
fn store_frame(
    layout: &mut LayoutConfigurationV2,
    position: Option<(i32, i32)>,
    size: (u32, u32),
    scale: f64,
) -> bool {
    let mut candidate = layout.clone();
    match position {
        Some((x, y)) => {
            candidate.x = Some(x);
            candidate.y = Some(y);
        }
        None => {
            candidate.x = None;
            candidate.y = None;
        }
    }
    candidate.width = Some(size.0);
    candidate.height = Some(size.1);
    candidate.scale_factor = Some(scale);
    if candidate.validate().is_err() {
        return false;
    }
    *layout = candidate;
    true
}

/// Native close is held only until the final document write completes. Waiting
/// for the coordinator happens off the UI thread, with no try_lock data loss.
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }
    let Some(session) = window.app_handle().try_state::<LayoutSession>() else {
        return;
    };
    match event {
        WindowEvent::Moved(_)
        | WindowEvent::Resized(_)
        | WindowEvent::ScaleFactorChanged { .. } => {
            if !session.closing.load(Ordering::SeqCst) {
                if let Err(error) = measure(window, &session) {
                    eprintln!("aworkit: could not measure window placement: {error}");
                }
            }
        }
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if session.closing.swap(true, Ordering::SeqCst) {
                return;
            }
            let window = window.clone();
            tauri::async_runtime::spawn(async move {
                let worker_window = window.clone();
                let result = tauri::async_runtime::spawn_blocking(move || {
                    let app = worker_window.app_handle();
                    let session = app.state::<LayoutSession>();
                    measure(&worker_window, &session)?;
                    let runtime = app.state::<SharedRuntime>();
                    let mut runtime = runtime
                        .lock()
                        .map_err(|_| "desktop runtime lock is unavailable")?;
                    runtime.settings_commit_layout(session.snapshot()?)
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("aworkit: could not save closing layout: {error}"),
                    Err(error) => eprintln!("aworkit: closing layout worker failed: {error}"),
                }
                // destroy bypasses CloseRequested, so the saved close runs once.
                if let Err(error) = window.destroy() {
                    window
                        .state::<LayoutSession>()
                        .closing
                        .store(false, Ordering::SeqCst);
                    eprintln!("aworkit: could not close window: {error}");
                }
            });
        }
        _ => {}
    }
}

/// Restores the persisted placement; on Windows that is the physical outer
/// rectangle through `SetWindowPos`, which is the shape the Win32 frame is
/// captured in. Windows uses the actual outer rectangle directly, including
/// custom menu height and DPI-dependent decorations. Everywhere else the
/// position is the outer frame origin and the size is the client size, which is
/// exactly what `set_size` applies, so no decoration inset is re-derived from
/// the live window.
pub fn restore_window_layout<R: Runtime>(
    app: &AppHandle<R>,
    layout: &LayoutConfigurationV2,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    // A session that cannot place windows is never even asked to move one: on
    // Wayland the compositor owns placement, so the stored position would be
    // ignored. The stored size still applies everywhere.
    let reachable = session_places_windows() && stored_position_is_reachable(&window, layout);
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
        if !reachable {
            flags |= SWP_NOMOVE;
        }
        if layout.width.is_none() || layout.height.is_none() {
            flags |= SWP_NOSIZE;
        }
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0;
        // Both coordinates and dimensions refer to the Win32 outer frame. Do
        // not feed them through AdjustWindowRect or scale logical pixels again.
        if unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                layout.x.unwrap_or(0),
                layout.y.unwrap_or(0),
                layout.width.unwrap_or(1) as i32,
                layout.height.unwrap_or(1) as i32,
                flags,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if reachable {
            window
                .set_position(tauri::PhysicalPosition::new(
                    layout.x.unwrap(),
                    layout.y.unwrap(),
                ))
                .map_err(|e| e.to_string())?;
        }
        if let (Some(width), Some(height)) = (layout.width, layout.height) {
            // `measure` captured the client size on these platforms, so it is
            // applied as-is. A Wayland compositor owns window placement and may
            // ignore the position above; the size always applies.
            window
                .set_size(tauri::PhysicalSize::new(
                    width.max(1),
                    height.max(1),
                ))
                .map_err(|e| e.to_string())?;
        }
    }
    if layout.maximized {
        window.maximize().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Whether the stored frame's title bar lands on a currently attached display.
///
/// A placement saved while another display was attached would otherwise open the
/// window where the user can neither see nor grab it, so only the position is
/// skipped and the stored size still applies. A display query that fails is not
/// read as "no display attached": a platform that cannot enumerate monitors keeps
/// restoring what it stored.
fn stored_position_is_reachable<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    layout: &LayoutConfigurationV2,
) -> bool {
    let (Some(x), Some(y)) = (layout.x, layout.y) else {
        return false;
    };
    let Ok(monitors) = window.available_monitors() else {
        return true;
    };
    if monitors.is_empty() {
        return true;
    }
    // The middle of the title-bar strip is the smallest part of the frame the
    // user needs in order to bring the window back into view.
    let probe_x = i64::from(x) + i64::from(layout.width.unwrap_or(0) / 2);
    // The invisible resize border can exceed eight physical pixels at high
    // DPI. Probe inside the title bar so a window aligned to the screen's top
    // (with a slightly negative outer y) remains a reachable placement.
    let probe_y = i64::from(y) + (24.0 * layout.scale_factor.unwrap_or(1.0)).round() as i64;
    monitors.iter().any(|monitor| {
        let left = i64::from(monitor.position().x);
        let top = i64::from(monitor.position().y);
        let right = left + i64::from(monitor.size().width);
        let bottom = top + i64::from(monitor.size().height);
        probe_x >= left && probe_x < right && probe_y >= top && probe_y < bottom
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A measurement that cannot be persisted must never replace a usable frame.
    /// The persisted record is validated on write, and a rejected record keeps
    /// an older frame silently — the "wrong size after a restart" this guards.
    #[test]
    fn keeps_the_last_usable_frame_when_a_measurement_is_unusable() {
        let mut layout = LayoutConfigurationV2::default();
        assert!(store_frame(&mut layout, Some((120, 64)), (1600, 1000), 1.0));
        assert_eq!(
            (layout.x, layout.y, layout.width, layout.height),
            (Some(120), Some(64), Some(1600), Some(1000))
        );

        // A size read before the window was realized.
        assert!(!store_frame(&mut layout, Some((0, 0)), (0, 0), 1.0));
        // A scale factor outside its persisted range.
        assert!(!store_frame(&mut layout, Some((0, 0)), (1600, 1000), 0.0));
        assert!(!store_frame(&mut layout, Some((0, 0)), (1600, 1000), f64::NAN));
        // A coordinate outside its persisted range.
        assert!(!store_frame(&mut layout, Some((i32::MAX, 0)), (1600, 1000), 1.0));

        assert_eq!(
            (layout.x, layout.y, layout.width, layout.height, layout.scale_factor),
            (Some(120), Some(64), Some(1600), Some(1000), Some(1.0))
        );

        // A later usable frame still replaces the stored one.
        assert!(store_frame(&mut layout, Some((-1280, 40)), (1440, 940), 1.5));
        assert_eq!(
            (layout.x, layout.y, layout.width, layout.height, layout.scale_factor),
            (Some(-1280), Some(40), Some(1440), Some(940), Some(1.5))
        );
    }

    /// A frame is stored exactly as measured; nothing scales or re-seats it.
    #[test]
    fn stores_a_usable_frame_verbatim() {
        let mut layout = LayoutConfigurationV2::default();
        assert!(store_frame(&mut layout, Some((0, 0)), (1, 1), 0.5));
        assert_eq!(layout.width, Some(1));
        assert_eq!(layout.scale_factor, Some(0.5));
    }

    /// A session that cannot report a window position stores none, so a stale
    /// or made-up origin is never re-applied later (on Wayland, or after the
    /// same profile is opened from an X11 session).
    #[test]
    fn clears_the_stored_position_when_the_session_cannot_report_one() {
        let mut layout = LayoutConfigurationV2::default();
        assert!(store_frame(&mut layout, Some((320, 180)), (1280, 800), 1.0));
        assert!(store_frame(&mut layout, None, (1280, 900), 1.0));
        assert_eq!((layout.x, layout.y), (None, None));
        // The size and scale still move with the window.
        assert_eq!((layout.width, layout.height), (Some(1280), Some(900)));
    }

    /// Only a session that can actually place windows is asked to move one.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_x11_sessions_place_their_own_windows() {
        // An X11 session, and a Wayland one, decided by the session itself.
        assert!(session_places_windows_for(None, false));
        assert!(!session_places_windows_for(None, true));
        // An explicit backend wins over the session's own type, which is also
        // how GDK picks: x11 runs through XWayland on a Wayland desktop.
        assert!(session_places_windows_for(Some("x11"), true));
        assert!(!session_places_windows_for(Some("wayland"), false));
        assert!(session_places_windows_for(Some("X11,wayland"), true));
        assert!(!session_places_windows_for(Some(" wayland "), false));
    }
}
