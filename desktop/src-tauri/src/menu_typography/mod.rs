//! Per-window typography for the existing Windows HMENU. Command routing,
//! accelerators, focus, popup placement and accessibility remain native.
#[cfg(target_os = "windows")]
mod paint;
#[cfg(target_os = "windows")]
mod windows;

pub fn install(window: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    return windows::install(window);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        Ok(())
    }
}

/// CSS supplies a logical font size including app/OS text scale, but not DPI.
pub fn project(window: &tauri::WebviewWindow, font_size: f64, dark: bool) -> Result<(), String> {
    if !font_size.is_finite() || !(6.0..=128.0).contains(&font_size) {
        return Err("menu font size is outside the supported range".into());
    }
    #[cfg(target_os = "windows")]
    return windows::project(window, font_size, dark);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, dark);
        Ok(())
    }
}

#[cfg(all(debug_assertions, target_os = "windows"))]
pub fn metrics(window: &tauri::WebviewWindow) -> Result<serde_json::Value, String> {
    windows::metrics(window)
}
