//! Tauri host for the unprivileged Aworkit presentation client.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use aworkit_desktop::management::{
    LocalRepairLedgerAdapter, ManagementRepairCommandInput, ManagementRepairGateway,
    ManagementRepairProjectionDto, ManagementRepairReceipt,
};
use aworkit_desktop::presentation::{
    NativeAppearanceV1, NativePresentationCapabilitiesV1, NativeWindowActionV1,
};
use aworkit_desktop::runtime::{
    DesktopRuntime, RuntimeSnapshot, SettingsCommitInput, SettingsSnapshot, UiCommandInput,
    UiCommandReceipt, WorkflowCommitInput, WorkflowSnapshot,
};
use aworkit_local_store::RedactionSet;
use tauri::Manager;

#[tauri::command]
fn native_presentation_capabilities() -> NativePresentationCapabilitiesV1 {
    aworkit_desktop::presentation::capabilities()
}

#[tauri::command]
fn native_set_appearance(
    window: tauri::WebviewWindow,
    appearance: NativeAppearanceV1,
) -> Result<(), String> {
    aworkit_desktop::presentation::apply_appearance(&window, appearance)
}

#[tauri::command]
fn native_window_action(
    window: tauri::WebviewWindow,
    action: NativeWindowActionV1,
) -> Result<(), String> {
    aworkit_desktop::presentation::apply_window_action(&window, action)
}

#[tauri::command]
fn native_notify(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    aworkit_desktop::presentation::show_notification(&app, title, body)
}

#[tauri::command]
async fn native_confirm(
    app: tauri::AppHandle,
    title: String,
    body: String,
) -> Result<bool, String> {
    aworkit_desktop::presentation::confirm_message(&app, title, body)
}

#[tauri::command]
async fn native_message(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    aworkit_desktop::presentation::show_message(&app, title, body)
}

#[tauri::command]
async fn native_pick_file(app: tauri::AppHandle) -> Option<tauri_plugin_dialog::FilePath> {
    aworkit_desktop::presentation::pick_file(&app)
}

#[tauri::command]
async fn native_pick_folder(app: tauri::AppHandle) -> Option<tauri_plugin_dialog::FilePath> {
    aworkit_desktop::presentation::pick_folder(&app)
}

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

#[tauri::command]
fn management_repair_snapshot(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
    after_sequence: u64,
) -> Result<ManagementRepairProjectionDto, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .management_repair_snapshot(after_sequence)
}

#[tauri::command]
fn management_repair_command(
    runtime: tauri::State<'_, Mutex<DesktopRuntime>>,
    command: ManagementRepairCommandInput,
    expected_version: u64,
) -> Result<ManagementRepairReceipt, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .management_repair_command(command, expected_version)
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            aworkit_desktop::presentation::install_application_menu(app.handle())?;
            let repair_root = app
                .path()
                .app_data_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .join("repair");
            let ledger = Arc::new(
                LocalRepairLedgerAdapter::for_store_root(repair_root, RedactionSet::default())
                    .map_err(|error| std::io::Error::other(error.to_string()))?,
            );
            let management = ManagementRepairGateway::with_durable_ledger(ledger);
            app.manage(Mutex::new(
                DesktopRuntime::default().with_management_repair(management),
            ));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_snapshot,
            desktop_command,
            settings_snapshot,
            settings_commit,
            workflow_snapshot,
            workflow_commit,
            management_repair_snapshot,
            management_repair_command,
            native_presentation_capabilities,
            native_set_appearance,
            native_window_action,
            native_notify,
            native_confirm,
            native_message,
            native_pick_file,
            native_pick_folder
        ])
        .on_menu_event(aworkit_desktop::presentation::forward_menu_event)
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running the Aworkit desktop shell");
}
