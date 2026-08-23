//! Native presentation bindings shared by the Tauri command boundary.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime, Theme, WebviewWindow, menu::*};
use tauri_plugin_dialog::{DialogExt, FilePath, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;

const MAX_TITLE_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NativeAppearanceV1 {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NativeWindowActionV1 {
    Show,
    Hide,
    Focus,
    Minimize,
    ToggleMaximize,
    ToggleFullscreen,
    Close,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePresentationCapabilitiesV1 {
    pub platform: &'static str,
    pub appearance: bool,
    pub application_menu: bool,
    pub file_dialogs: bool,
    pub message_dialogs: bool,
    pub notifications: bool,
    pub window_lifecycle: bool,
    pub accessible_workbench_fallback: bool,
}

#[must_use]
pub const fn capabilities() -> NativePresentationCapabilitiesV1 {
    NativePresentationCapabilitiesV1 {
        platform: std::env::consts::OS,
        appearance: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
        application_menu: true,
        file_dialogs: true,
        message_dialogs: true,
        notifications: true,
        window_lifecycle: true,
        accessible_workbench_fallback: true,
    }
}

pub fn install_application_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let file = SubmenuBuilder::new(app, "File")
        .text("aworkit.new-chat", "New Chat")
        .text("aworkit.open-workflow", "Open Workflow…")
        .separator()
        .text("aworkit.settings", "Settings…")
        .separator()
        .close_window()
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View")
        .text("aworkit.chat", "Chat")
        .text("aworkit.workflows", "Workflows")
        .text("aworkit.management", "Management")
        .separator()
        .fullscreen()
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .build()?;
    let help = SubmenuBuilder::new(app, "Help")
        .text("aworkit.shortcuts", "Keyboard Shortcuts")
        .about(None)
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&file, &edit, &view, &window, &help])
        .build()?;
    app.set_menu(menu)?;
    Ok(())
}

pub fn forward_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let id = event.id().as_ref();
    if id.starts_with("aworkit.") {
        let _ = app.emit("aworkit:native-menu", id);
    }
}

pub fn apply_appearance<R: Runtime>(
    window: &WebviewWindow<R>,
    appearance: NativeAppearanceV1,
) -> Result<(), String> {
    let theme = match appearance {
        NativeAppearanceV1::System => None,
        NativeAppearanceV1::Light => Some(Theme::Light),
        NativeAppearanceV1::Dark => Some(Theme::Dark),
    };
    window.set_theme(theme).map_err(|error| error.to_string())
}

pub fn apply_window_action<R: Runtime>(
    window: &WebviewWindow<R>,
    action: NativeWindowActionV1,
) -> Result<(), String> {
    match action {
        NativeWindowActionV1::Show => window.show(),
        NativeWindowActionV1::Hide => window.hide(),
        NativeWindowActionV1::Focus => window.show().and_then(|()| window.set_focus()),
        NativeWindowActionV1::Minimize => window.minimize(),
        NativeWindowActionV1::ToggleMaximize => window.is_maximized().and_then(|maximized| {
            if maximized {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }),
        NativeWindowActionV1::ToggleFullscreen => window
            .is_fullscreen()
            .and_then(|current| window.set_fullscreen(!current)),
        NativeWindowActionV1::Close => window.close(),
    }
    .map_err(|error| error.to_string())
}

pub fn show_notification<R: Runtime>(
    app: &AppHandle<R>,
    title: String,
    body: String,
) -> Result<(), String> {
    validate_text(&title, &body)?;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|error| error.to_string())
}

pub fn confirm_message<R: Runtime>(
    app: &AppHandle<R>,
    title: String,
    body: String,
) -> Result<bool, String> {
    validate_text(&title, &body)?;
    Ok(app
        .dialog()
        .message(body)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Confirm".to_owned(),
            "Cancel".to_owned(),
        ))
        .blocking_show())
}

pub fn show_message<R: Runtime>(
    app: &AppHandle<R>,
    title: String,
    body: String,
) -> Result<(), String> {
    validate_text(&title, &body)?;
    app.dialog()
        .message(body)
        .title(title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::Ok)
        .blocking_show();
    Ok(())
}

pub fn pick_file<R: Runtime>(app: &AppHandle<R>) -> Option<FilePath> {
    app.dialog()
        .file()
        .set_title("Open Aworkit file")
        .blocking_pick_file()
}

pub fn pick_folder<R: Runtime>(app: &AppHandle<R>) -> Option<FilePath> {
    app.dialog()
        .file()
        .set_title("Choose workspace folder")
        .blocking_pick_folder()
}

fn validate_text(title: &str, body: &str) -> Result<(), String> {
    if title.trim().is_empty()
        || title.len() > MAX_TITLE_BYTES
        || body.trim().is_empty()
        || body.len() > MAX_MESSAGE_BYTES
    {
        return Err("native presentation text is empty or exceeds its bound".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_report_includes_accessible_fallback_and_current_platform() {
        let report = capabilities();
        assert!(!report.platform.is_empty());
        assert!(report.accessible_workbench_fallback);
        assert!(report.window_lifecycle && report.file_dialogs && report.message_dialogs);
    }

    #[test]
    fn native_message_bounds_are_fail_closed() {
        assert!(validate_text("", "body").is_err());
        assert!(validate_text("title", "").is_err());
        assert!(validate_text("title", &"x".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
    }
}
