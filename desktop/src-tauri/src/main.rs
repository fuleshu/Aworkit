//! Tauri host for the unprivileged Aworkit presentation client.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use aworkit_desktop::management::{
    LocalRepairLedgerAdapter, ManagementRepairCommandInput, ManagementRepairGateway,
    ManagementRepairProjectionDto, ManagementRepairReceipt,
};
use aworkit_desktop::presentation::{
    NativeAppearanceV1, NativePresentationCapabilitiesV1, NativeWindowActionV1,
};
use aworkit_desktop::runtime::{
    CredentialDeleteInputV2, CredentialStoreInputV2, DesktopRuntime, ExtensionConfigurationV2,
    ExtensionRegisterInputV2, ExternalAgentProbeRequestV2, ExternalAgentProbeResultV2,
    McpProbeRequestV2, McpProbeResultV2, ModelDiscoveryRequestV2, ModelDiscoveryResultV2,
    ProjectProbeRequestV2, ProjectProbeResultV2, ProviderProbeRequestV2, ProviderProbeResultV2,
    ProviderTestInput, ProviderTestResult, RuntimeSnapshot, SettingsCommitInput, SettingsSnapshot,
    SettingsV2CommitInput, SettingsV2Snapshot, ToolProbeRequestV2, ToolProbeResultV2,
    UiCommandInput, UiCommandReceipt, WorkflowCommitInput, WorkflowSnapshot,
};
use aworkit_local_store::RedactionSet;
use tauri::Manager;

type SharedRuntime = Arc<Mutex<DesktopRuntime>>;

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
    runtime: tauri::State<'_, SharedRuntime>,
    after_sequence: u64,
) -> Result<RuntimeSnapshot, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .snapshot(after_sequence)
}

#[tauri::command]
async fn desktop_command(
    runtime: tauri::State<'_, SharedRuntime>,
    command: UiCommandInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .command(command)
    })
    .await
    .map_err(|error| format!("desktop command worker failed: {error}"))?
}

#[tauri::command]
fn settings_snapshot(runtime: tauri::State<'_, SharedRuntime>) -> Result<SettingsSnapshot, String> {
    Ok(runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .settings_snapshot())
}

#[tauri::command]
async fn settings_commit(
    runtime: tauri::State<'_, SharedRuntime>,
    command: SettingsCommitInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_commit(command)
    })
    .await
    .map_err(|error| format!("settings command worker failed: {error}"))?
}

#[tauri::command]
async fn settings_test_provider(
    runtime: tauri::State<'_, SharedRuntime>,
    request: ProviderTestInput,
) -> Result<ProviderTestResult, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        Ok(runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_test_provider(request))
    })
    .await
    .map_err(|error| format!("provider test worker failed: {error}"))?
}

#[tauri::command]
fn settings_v2_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<SettingsV2Snapshot, String> {
    Ok(runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .settings_v2_snapshot())
}

#[tauri::command]
async fn settings_v2_commit(
    runtime: tauri::State<'_, SharedRuntime>,
    command: SettingsV2CommitInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_commit(command)
    })
    .await
    .map_err(|error| format!("settings v2 command worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_store_credential(
    runtime: tauri::State<'_, SharedRuntime>,
    command: CredentialStoreInputV2,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_store_credential(command)
    })
    .await
    .map_err(|error| format!("credential-store worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_delete_credential(
    runtime: tauri::State<'_, SharedRuntime>,
    command: CredentialDeleteInputV2,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_delete_credential(command)
    })
    .await
    .map_err(|error| format!("credential-delete worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_test_provider(
    runtime: tauri::State<'_, SharedRuntime>,
    request: ProviderProbeRequestV2,
) -> Result<ProviderProbeResultV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_test_provider(request)
    })
    .await
    .map_err(|error| format!("provider-probe worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_discover_models(
    runtime: tauri::State<'_, SharedRuntime>,
    request: ModelDiscoveryRequestV2,
) -> Result<ModelDiscoveryResultV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_discover_models(request)
    })
    .await
    .map_err(|error| format!("model-discovery worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_probe_mcp(
    runtime: tauri::State<'_, SharedRuntime>,
    request: McpProbeRequestV2,
) -> Result<McpProbeResultV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_probe_mcp(request)
    })
    .await
    .map_err(|error| format!("MCP-probe worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_probe_external_agent(
    runtime: tauri::State<'_, SharedRuntime>,
    request: ExternalAgentProbeRequestV2,
) -> Result<ExternalAgentProbeResultV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_probe_external_agent(request)
    })
    .await
    .map_err(|error| format!("external-agent-probe worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_probe_project(
    runtime: tauri::State<'_, SharedRuntime>,
    request: ProjectProbeRequestV2,
) -> Result<ProjectProbeResultV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        Ok(runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_probe_project(request))
    })
    .await
    .map_err(|error| format!("project-probe worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_probe_tool(
    runtime: tauri::State<'_, SharedRuntime>,
    request: ToolProbeRequestV2,
) -> Result<ToolProbeResultV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        Ok(runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_probe_tool(request))
    })
    .await
    .map_err(|error| format!("tool-probe worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_inspect_extension(
    runtime: tauri::State<'_, SharedRuntime>,
    path: String,
) -> Result<ExtensionConfigurationV2, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_inspect_extension(&PathBuf::from(path))
    })
    .await
    .map_err(|error| format!("extension-inspection worker failed: {error}"))?
}

#[tauri::command]
async fn settings_v2_register_extension(
    runtime: tauri::State<'_, SharedRuntime>,
    command: ExtensionRegisterInputV2,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .settings_v2_register_extension(command)
    })
    .await
    .map_err(|error| format!("extension-registration worker failed: {error}"))?
}

#[tauri::command]
fn workflow_snapshot(runtime: tauri::State<'_, SharedRuntime>) -> Result<WorkflowSnapshot, String> {
    Ok(runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .workflow_snapshot())
}

#[tauri::command]
async fn workflow_commit(
    runtime: tauri::State<'_, SharedRuntime>,
    command: WorkflowCommitInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .workflow_commit(command)
    })
    .await
    .map_err(|error| format!("workflow command worker failed: {error}"))?
}

#[tauri::command]
fn management_repair_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
    after_sequence: u64,
) -> Result<ManagementRepairProjectionDto, String> {
    runtime
        .lock()
        .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
        .management_repair_snapshot(after_sequence)
}

#[tauri::command]
fn management_repair_command(
    runtime: tauri::State<'_, SharedRuntime>,
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
            let app_data_root = app
                .path()
                .app_data_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let repair_root = app_data_root.join("repair");
            let ledger = Arc::new(
                LocalRepairLedgerAdapter::for_store_root(repair_root, RedactionSet::default())
                    .map_err(|error| std::io::Error::other(error.to_string()))?,
            );
            let management = ManagementRepairGateway::with_durable_ledger(ledger);
            let runtime = DesktopRuntime::open(app_data_root.join("runtime"))
                .map_err(std::io::Error::other)?
                .with_management_repair(management);
            app.manage(Arc::new(Mutex::new(runtime)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_snapshot,
            desktop_command,
            settings_snapshot,
            settings_commit,
            settings_test_provider,
            settings_v2_snapshot,
            settings_v2_commit,
            settings_v2_store_credential,
            settings_v2_delete_credential,
            settings_v2_test_provider,
            settings_v2_discover_models,
            settings_v2_probe_mcp,
            settings_v2_probe_external_agent,
            settings_v2_probe_project,
            settings_v2_probe_tool,
            settings_v2_inspect_extension,
            settings_v2_register_extension,
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
