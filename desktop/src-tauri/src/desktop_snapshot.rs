//! Snapshot metadata and exact event pages use separate IPC responses so a
//! long Chat never becomes a single browser-sized JSON string at startup.
use crate::{SharedRuntime, runtime_worker};
use aworkit_desktop::runtime::RuntimeSnapshot;
use std::sync::Arc;

/// Metadata is cheap; event loading releases the coordinator before touching
/// page payloads so navigation, Settings and other Chats can proceed.
#[tauri::command]
pub async fn desktop_chat_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
    after_sequence: u64,
    chat_id: Option<String>,
) -> Result<RuntimeSnapshot, String> {
    let host = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let (mut snapshot, reader) = {
            let runtime = host.lock()?;
            let snapshot = runtime.snapshot_for_chat(u64::MAX, chat_id.as_deref())?;
            let reader = runtime.chat_feed_reader(&snapshot.chat.chat_id)?;
            (snapshot, reader)
        };
        let page = reader.page(after_sequence, None, snapshot.through_sequence)?;
        snapshot.events = page.events;
        snapshot.event_window = Some(page.window);
        Ok(snapshot)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn desktop_chat_events(
    runtime: tauri::State<'_, SharedRuntime>,
    chat_id: String,
    after_sequence: u64,
    before_sequence: Option<u64>,
    through_sequence: u64,
) -> Result<aworkit_desktop::runtime::ChatEventPage, String> {
    let host = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let reader = host.lock()?.chat_feed_reader(&chat_id)?;
        reader.page(after_sequence, before_sequence, through_sequence)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn desktop_snapshot(
    runtime: tauri::State<'_, SharedRuntime>,
    after_sequence: u64,
    chat_id: Option<String>,
    paged: Option<bool>,
) -> Result<RuntimeSnapshot, String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "desktop snapshot",
        move |runtime| {
            runtime.snapshot_for_chat(
                if paged == Some(true) {
                    u64::MAX
                } else {
                    after_sequence
                },
                chat_id.as_deref(),
            )
        },
    )
    .await
}
