//! Hermetic CLI exercising the same durable runtime used by the Tauri host.

use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use aworkit_desktop::runtime::{
    DesktopRuntime, ProviderCommitInput, ProviderTestInput, SettingsCommitInput, UiCommandInput,
};
use aworkit_local_store::{DocumentKind, DocumentRepository, RepositoryRoot};
use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    CredentialRef, CredentialSecretV1, MemoryCredentialStore, PlatformCredentialStorePort,
};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FirstResult {
    schema_version: u16,
    phase: &'static str,
    chat_id: String,
    settings_version: u64,
    provider_tested: bool,
    assistant_reply: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReopenResult {
    schema_version: u16,
    phase: &'static str,
    chat_id: String,
    settings_version: u64,
    provider_tested: bool,
    prior_assistant_reply: String,
    assistant_reply: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("rescue workflow runner failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let [data_root, base_url, phase] = arguments.as_slice() else {
        return Err("usage: aworkit-rescue-e2e <data-root> <base-url> <first|reopen>".into());
    };
    let api_key = env::var("AWORKIT_RESCUE_E2E_API_KEY")
        .map_err(|_| "AWORKIT_RESCUE_E2E_API_KEY is required".to_owned())?;
    let model = env::var("AWORKIT_RESCUE_E2E_MODEL")
        .map_err(|_| "AWORKIT_RESCUE_E2E_MODEL is required".to_owned())?;
    let data_root = PathBuf::from(data_root);
    let store = Arc::new(MemoryCredentialStore::default());
    if phase == "reopen" {
        restore_hermetic_credential(&data_root, &store, &api_key)?;
    }
    let mut runtime = DesktopRuntime::open_with_credential_store(&data_root, store)?;
    if phase == "first" {
        configure(&mut runtime, base_url, &model, api_key, phase)?;
    } else {
        let saved = runtime.settings_snapshot();
        if saved.provider.base_url != base_url.as_str() || saved.provider.model != model.as_str() {
            return Err(
                "reopened runtime did not load the provider configuration saved by process one"
                    .into(),
            );
        }
        if !saved.provider.credential_configured {
            return Err("reopened runtime lost the saved opaque credential reference".into());
        }
    }
    // On reopen, reconstruct and inspect the committed Chat before making even
    // the read-only provider health request. This is the restart gate: loading
    // history must never replay or precede itself with an external effect.
    let reopened_prior = if phase == "reopen" {
        Some(last_assistant(&runtime.snapshot(0)?.timeline)?)
    } else {
        None
    };
    let provider_test = runtime.settings_test_provider(ProviderTestInput {
        base_url: base_url.clone(),
        model,
        api_key: None,
        use_stored_credential: true,
    });
    if !provider_test.ok {
        return Err(provider_test.message);
    }

    match phase.as_str() {
        "first" => {
            let expected = runtime.snapshot(0)?.version;
            let workflow_id = tool_free_workflow_id(&runtime)?;
            runtime.command(chat_command(
                "rescue.chat.first",
                expected,
                "start",
                "hello",
                Some(&workflow_id),
            ))?;
            let snapshot = runtime.snapshot(0)?;
            print_json(&FirstResult {
                schema_version: 1,
                phase: "first",
                chat_id: snapshot.chat.chat_id,
                settings_version: runtime.settings_snapshot().version,
                provider_tested: true,
                assistant_reply: last_assistant(&snapshot.timeline)?,
            })
        }
        "reopen" => {
            let before = runtime.snapshot(0)?;
            let prior_assistant_reply = reopened_prior
                .ok_or_else(|| "reopen did not reconstruct the prior assistant reply".to_owned())?;
            let expected = before.version;
            runtime.command(chat_command(
                "rescue.chat.reopen",
                expected,
                "enqueue",
                "again",
                None,
            ))?;
            let snapshot = runtime.snapshot(0)?;
            print_json(&ReopenResult {
                schema_version: 1,
                phase: "reopen",
                chat_id: snapshot.chat.chat_id,
                settings_version: runtime.settings_snapshot().version,
                provider_tested: true,
                prior_assistant_reply,
                assistant_reply: last_assistant(&snapshot.timeline)?,
            })
        }
        _ => Err("phase must be first or reopen".into()),
    }
}

fn restore_hermetic_credential(
    data_root: &Path,
    store: &MemoryCredentialStore,
    api_key: &str,
) -> Result<(), String> {
    let repository = RepositoryRoot::open(data_root.join("documents"))
        .map_err(|error| format!("cannot open saved rescue settings: {error}"))?;
    let settings = repository
        .load(DocumentKind::Configuration, "settings.desktop")
        .map_err(|error| format!("cannot load saved rescue settings: {error}"))?
        .ok_or_else(|| "saved rescue settings are missing".to_owned())?;
    let value = settings
        .document
        .value()
        .map_err(|error| format!("saved rescue settings are invalid: {error}"))?;
    let providers = value
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "saved rescue settings have no provider catalog".to_owned())?;
    let provider = providers
        .iter()
        .find(|provider| {
            provider.get("id").and_then(serde_json::Value::as_str) == Some("provider.primary")
                && provider.get("enabled").and_then(serde_json::Value::as_bool) == Some(true)
        })
        .or_else(|| {
            providers.iter().find(|provider| {
                provider.get("enabled").and_then(serde_json::Value::as_bool) == Some(true)
            })
        })
        .ok_or_else(|| "saved rescue settings have no enabled provider".to_owned())?;
    let reference = provider
        .get("credentialRef")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "saved rescue settings have no opaque credential reference".to_owned())?;
    let reference = CredentialRef(
        StableId::parse(reference.to_owned())
            .map_err(|error| format!("saved rescue credential reference is invalid: {error}"))?,
    );
    store
        .put(
            &reference,
            CredentialSecretV1::new(BTreeMap::from([(
                "api_key".to_owned(),
                api_key.as_bytes().to_vec(),
            )])),
        )
        .map_err(|error| format!("cannot restore hermetic rescue credential: {error}"))
}

fn configure(
    runtime: &mut DesktopRuntime,
    base_url: &str,
    model: &str,
    api_key: String,
    phase: &str,
) -> Result<(), String> {
    let settings = runtime.settings_snapshot();
    runtime.settings_commit(SettingsCommitInput {
        command_id: format!("rescue.settings.{phase}"),
        expected_version: settings.version,
        appearance: settings.appearance,
        portable_history_enabled: settings.portable_history_enabled,
        provider: ProviderCommitInput {
            base_url: base_url.into(),
            model: model.into(),
            credential_action: "replace".into(),
            api_key: Some(api_key.into()),
        },
    })?;
    Ok(())
}

fn chat_command(
    command_id: &str,
    expected_version: u64,
    action: &str,
    input: &str,
    workflow_id: Option<&str>,
) -> UiCommandInput {
    let mut payload = json!({
        "input": input,
        "attachments": [],
    });
    if let Some(workflow_id) = workflow_id {
        payload["workflowId"] = workflow_id.into();
    }
    UiCommandInput {
        schema_version: 1,
        command_id: command_id.into(),
        expected_version,
        action: action.into(),
        target_id: Some("chat.local".into()),
        payload,
    }
}

fn tool_free_workflow_id(runtime: &DesktopRuntime) -> Result<String, String> {
    for entry in runtime.workflow_library().entries {
        let document = runtime.workflow_snapshot_for(entry.id.clone()).document;
        let tool_free = document
            .get("nodes")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|nodes| {
                nodes.iter().all(|node| {
                    node.get("type").and_then(serde_json::Value::as_str) != Some("agent")
                        || node
                            .get("configuration")
                            .and_then(|configuration| configuration.get("toolIds"))
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(Vec::is_empty)
                })
            });
        if tool_free {
            return Ok(entry.id);
        }
    }
    Err("workflow library has no tool-free workflow for the hermetic runner".into())
}

fn last_assistant(
    timeline: &[aworkit_desktop::runtime::TimelineItemDto],
) -> Result<String, String> {
    timeline
        .iter()
        .rev()
        .find(|item| item.title == "Aworkit")
        .map(|item| item.body.clone())
        .ok_or_else(|| "runtime snapshot has no assistant reply".into())
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|error| format!("cannot serialize rescue result: {error}"))?
    );
    Ok(())
}
