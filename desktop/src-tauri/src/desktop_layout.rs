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
pub fn desktop_layout(
    session: tauri::State<'_, LayoutSession>,
) -> Result<LayoutConfigurationV2, String> {
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
        Some((
            window.outer_position().map_err(|e| e.to_string())?,
            window.outer_size().map_err(|e| e.to_string())?,
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
        layout.x = Some(position.x);
        layout.y = Some(position.y);
        layout.width = Some(size.width);
        layout.height = Some(size.height);
        layout.scale_factor = Some(scale);
    }
    Ok(())
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

/// Restores physical outer bounds; Tauri set_size accepts a client size.
/// Windows uses the actual outer rectangle directly, including custom menu
/// height and DPI-dependent decorations. Other platforms subtract native insets.
pub fn restore_window_layout<R: Runtime>(
    app: &AppHandle<R>,
    layout: &LayoutConfigurationV2,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    let reachable = stored_position_is_reachable(&window, layout);
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
            let outer = window.outer_size().map_err(|e| e.to_string())?;
            let inner = window.inner_size().map_err(|e| e.to_string())?;
            window
                .set_size(tauri::PhysicalSize::new(
                    width
                        .saturating_sub(outer.width.saturating_sub(inner.width))
                        .max(1),
                    height
                        .saturating_sub(outer.height.saturating_sub(inner.height))
                        .max(1),
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
