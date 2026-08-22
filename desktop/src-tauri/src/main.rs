//! Tauri host for the unprivileged Aworkit presentation client.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

use aworkit_desktop::runtime::{
    DesktopRuntime, RuntimeSnapshot, SettingsCommitInput, SettingsSnapshot, UiCommandInput,
    UiCommandReceipt, WorkflowCommitInput, WorkflowSnapshot,
};

#[tauri::command]
fn desktop_snapshot(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
    after_sequence: u64,
) -> Result<RuntimeSnapshot, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .snapshot(after_sequence)
}

#[tauri::command]
fn desktop_command(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
    command: UiCommandInput,
) -> Result<UiCommandReceipt, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .command(command)
}

#[tauri::command]
fn settings_snapshot(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
) -> Result<SettingsSnapshot, String> {
    Ok(runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .settings_snapshot())
}

#[tauri::command]
fn settings_commit(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
    command: SettingsCommitInput,
) -> Result<UiCommandReceipt, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .settings_commit(command)
}

#[tauri::command]
fn workflow_snapshot(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
) -> Result<WorkflowSnapshot, String> {
    Ok(runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .workflow_snapshot())
}

#[tauri::command]
fn workflow_commit(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
    command: WorkflowCommitInput,
) -> Result<UiCommandReceipt, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .workflow_commit(command)
}

#[cfg(target_os = "linux")]
fn prepare_graphical_backend() {
    if std::env::var("GDK_BACKEND")
        .is_ok_and(|backend| backend.split(',').any(|name| name == "broadway"))
    {
        use gtk::prelude::*;

        gtk::init().expect("the Broadway QA display must initialize GTK");
        if let Some(settings) = gtk::Settings::default()
            && settings.gtk_xft_dpi() <= 0
        {
            // Broadway has no physical monitor and reports an invalid DPI. A
            // deterministic 96 DPI keeps WebKit's viewport finite for native
            // headless rendering without affecting X11 or Wayland launches.
            settings.set_gtk_xft_dpi(96 * 1024);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn prepare_graphical_backend() {}

fn main() {
    prepare_graphical_backend();
    tauri::Builder::default()
        .manage(Mutex::new(DesktopRuntime::default()))
        .invoke_handler(tauri::generate_handler![
            desktop_snapshot,
            desktop_command,
            settings_snapshot,
            settings_commit,
            workflow_snapshot,
            workflow_commit
        ])
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running the Aworkit desktop shell");
}
