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
    CommittedChatEventPort, CoreEventEnvelope, CredentialDeleteInputV2, CredentialStoreInputV2,
    DesktopRuntime, ExtensionConfigurationV2, ExtensionRegisterInputV2,
    ExternalAgentProbeRequestV2, ExternalAgentProbeResultV2, McpProbeRequestV2, McpProbeResultV2,
    ModelDiscoveryRequestV2, ModelDiscoveryResultV2, ProjectProbeRequestV2, ProjectProbeResultV2,
    ProviderProbeRequestV2, ProviderProbeResultV2, ProviderTestInput, ProviderTestResult,
    RuntimeSnapshot, SettingsCommitInput, SettingsSnapshot, SettingsV2CommitInput,
    SettingsV2Snapshot, ToolProbeRequestV2, ToolProbeResultV2, UiCommandInput, UiCommandReceipt,
    WorkflowCancellationController, WorkflowCommitInput, WorkflowCreateInput,
    WorkflowCreateReceipt, WorkflowDuplicateInput, WorkflowLibrarySnapshot, WorkflowRenameInput,
    WorkflowSnapshot, WorkflowTargetInput,
};
use aworkit_local_store::RedactionSet;
use tauri::{Emitter, Manager};

type SharedRuntime = Arc<Mutex<DesktopRuntime>>;

/// Runs every potentially contended runtime access away from Tauri's IPC/UI
/// dispatcher. A Chat execution deliberately owns the mutable runtime while it
/// settles, but waiting for that ownership must never stall WebView rendering
/// or delivery of live activity events.
async fn runtime_worker<T, F>(
    runtime: SharedRuntime,
    operation: &'static str,
    access: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut DesktopRuntime) -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let mut runtime = runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?;
        access(&mut runtime)
    })
    .await
    .map_err(|error| format!("{operation} worker failed: {error}"))?
}

struct TauriCommittedChatEvents {
    app: tauri::AppHandle,
}

impl CommittedChatEventPort for TauriCommittedChatEvents {
    fn publish(&self, event: CoreEventEnvelope) -> Result<(), String> {
        self.app
            .emit("aworkit:chat-event", event)
            .map_err(|error| format!("cannot publish committed Chat event: {error}"))
    }
}

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
async fn desktop_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
    after_sequence: u64,
) -> Result<RuntimeSnapshot, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "desktop snapshot",
        move |runtime| runtime.snapshot(after_sequence),
    )
    .await
}

/// Thumbnail I/O must not wait for an active model request's runtime mutex.
#[tauri::command]
async fn approval_project_grants(
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<Vec<aworkit_desktop::runtime::ProjectApprovalGrant>, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "project approvals",
        |runtime| runtime.project_approval_grants(),
    )
    .await
}

#[tauri::command]
async fn approval_revoke_project_grant(
    runtime: tauri::State<'_, SharedRuntime>,
    id: String,
) -> Result<(), String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "revoke project approval",
        move |runtime| runtime.revoke_project_approval(&id),
    )
    .await
}

/// Thumbnail I/O must not wait for an active model request's runtime mutex.
#[tauri::command]
async fn chat_image_import(
    store: tauri::State<'_, aworkit_desktop::runtime::ChatImageStore>,
    name: String,
    data: String,
) -> Result<aworkit_capability_host::model_images::ImageAttachmentV1, String> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.import(name, data))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn chat_image_preview(
    store: tauri::State<'_, aworkit_desktop::runtime::ChatImageStore>,
    image: aworkit_capability_host::model_images::ImageAttachmentV1,
) -> Result<String, String> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.preview(&image))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn chat_image_thumbnail(
    store: tauri::State<'_, aworkit_desktop::runtime::ChatImageStore>,
    image: aworkit_capability_host::model_images::ImageAttachmentV1,
) -> Result<String, String> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.thumbnail(&image))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn desktop_command(
    runtime: tauri::State<'_, SharedRuntime>,
    cancellation: tauri::State<'_, WorkflowCancellationController>,
    command: UiCommandInput,
) -> Result<UiCommandReceipt, String> {
    let stop_command_id = if command.action == "cancel" {
        let target = command
            .target_id
            .as_deref()
            .ok_or_else(|| "Stop requires the exact active Chat target".to_owned())?;
        cancellation.request_stop(target, &command.command_id)?;
        Some(command.command_id.clone())
    } else {
        None
    };
    let cancellation = cancellation.inner().clone();
    let runtime = Arc::clone(runtime.inner());
    let result = tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .command(command)
    })
    .await
    .map_err(|error| format!("desktop command worker failed: {error}"))
    .and_then(|result| result);
    if let Some(command_id) = stop_command_id {
        cancellation.discard_request(&command_id);
    }
    result
}

#[tauri::command]
async fn settings_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<SettingsSnapshot, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "settings snapshot",
        |runtime| Ok(runtime.settings_snapshot()),
    )
    .await
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
async fn settings_v2_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<SettingsV2Snapshot, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "settings-v2 snapshot",
        |runtime| Ok(runtime.settings_v2_snapshot()),
    )
    .await
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
async fn workflow_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
    workflow_id: Option<String>,
) -> Result<WorkflowSnapshot, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "workflow snapshot",
        move |runtime| {
            Ok(match workflow_id {
                Some(workflow_id) => runtime.workflow_snapshot_for(workflow_id),
                None => runtime.workflow_snapshot(),
            })
        },
    )
    .await
}

#[tauri::command]
async fn workflow_library(
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<WorkflowLibrarySnapshot, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "workflow library snapshot",
        |runtime| Ok(runtime.workflow_library()),
    )
    .await
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
async fn workflow_create(
    runtime: tauri::State<'_, SharedRuntime>,
    command: WorkflowCreateInput,
) -> Result<WorkflowCreateReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .workflow_create(command)
    })
    .await
    .map_err(|error| format!("workflow create worker failed: {error}"))?
}

#[tauri::command]
async fn workflow_duplicate(
    runtime: tauri::State<'_, SharedRuntime>,
    command: WorkflowDuplicateInput,
) -> Result<WorkflowCreateReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .workflow_duplicate(command)
    })
    .await
    .map_err(|error| format!("workflow duplicate worker failed: {error}"))?
}

#[tauri::command]
async fn workflow_delete(
    runtime: tauri::State<'_, SharedRuntime>,
    command: WorkflowTargetInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .workflow_delete(command)
    })
    .await
    .map_err(|error| format!("workflow delete worker failed: {error}"))?
}

#[tauri::command]
async fn workflow_rename(
    runtime: tauri::State<'_, SharedRuntime>,
    command: WorkflowRenameInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .workflow_rename(command)
    })
    .await
    .map_err(|error| format!("workflow rename worker failed: {error}"))?
}

#[tauri::command]
async fn workflow_set_default(
    runtime: tauri::State<'_, SharedRuntime>,
    command: WorkflowTargetInput,
) -> Result<UiCommandReceipt, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".to_owned())?
            .workflow_set_default(command)
    })
    .await
    .map_err(|error| format!("workflow default worker failed: {error}"))?
}

#[tauri::command]
async fn management_repair_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
    after_sequence: u64,
) -> Result<ManagementRepairProjectionDto, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "management snapshot",
        move |runtime| runtime.management_repair_snapshot(after_sequence),
    )
    .await
}

#[tauri::command]
async fn management_repair_command(
    runtime: tauri::State<'_, SharedRuntime>,
    command: ManagementRepairCommandInput,
    expected_version: u64,
) -> Result<ManagementRepairReceipt, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "management command",
        move |runtime| runtime.management_repair_command(command, expected_version),
    )
    .await
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
    if let Some(result) = aworkit_desktop::live_qa::run_from_arguments(std::env::args().skip(1)) {
        if let Err(error) = result {
            eprintln!("Aworkit live-model QA failed: {error}");
            std::process::exit(1);
        }
        return;
    }
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
            // Debug-only profile isolation for native WebView regression QA.
            #[cfg(debug_assertions)]
            let app_data_root = std::env::var_os("AWORKIT_QA_PROFILE")
                .map(PathBuf::from)
                .unwrap_or(app_data_root);
            let repair_root = app_data_root.join("repair");
            let ledger = Arc::new(
                LocalRepairLedgerAdapter::for_store_root(repair_root, RedactionSet::default())
                    .map_err(|error| std::io::Error::other(error.to_string()))?,
            );
            let management = ManagementRepairGateway::with_durable_ledger(ledger);
            let committed_events: Arc<dyn CommittedChatEventPort> =
                Arc::new(TauriCommittedChatEvents {
                    app: app.handle().clone(),
                });
            let runtime = DesktopRuntime::open_with_committed_events(
                app_data_root.join("runtime"),
                committed_events,
            )
            .map_err(std::io::Error::other)?
            .with_management_repair(management);
            app.manage(runtime.cancellation_controller());
            app.manage(runtime.image_store());
            app.manage(Arc::new(Mutex::new(runtime)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            chat_image_import,
            chat_image_preview,
            chat_image_thumbnail,
            desktop_snapshot,
            desktop_command,
            approval_project_grants,
            approval_revoke_project_grant,
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
            workflow_library,
            workflow_commit,
            workflow_create,
            workflow_duplicate,
            workflow_delete,
            workflow_rename,
            workflow_set_default,
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
