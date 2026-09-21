//! Delegated-subagent tab presentation preference.
//!
//! It is one global user preference, committed through its own dedicated
//! version-checked command so it never enters a Chat's frozen tool contract.
use crate::{SharedRuntime, runtime_worker};
use aworkit_desktop::runtime::SubagentViewPreferenceV1;
use serde::Serialize;
use std::sync::Arc;

/// The preference projection plus the Settings document version it was read
/// at, so the editor can detect that its own write went stale.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentViewSnapshotV1 {
    auto_open: bool,
    auto_close: bool,
    version: u64,
}

/// Reads the stored preference. A missing document section is the default.
#[tauri::command]
pub async fn desktop_subagent_view(
    runtime: tauri::State<'_, SharedRuntime>,
) -> Result<SubagentViewSnapshotV1, String> {
    runtime_worker(Arc::clone(runtime.inner()), "subagent view", |runtime| {
        let preference = runtime.subagent_view();
        Ok(SubagentViewSnapshotV1 {
            auto_open: preference.auto_open,
            auto_close: preference.auto_close,
            version: runtime.subagent_view_version(),
        })
    })
    .await
}

/// Commits the complete preference at the version the editor last projected.
#[tauri::command]
pub async fn desktop_subagent_view_commit(
    runtime: tauri::State<'_, SharedRuntime>,
    auto_open: bool,
    auto_close: bool,
    expected_version: u64,
) -> Result<(), String> {
    runtime_worker(
        Arc::clone(runtime.inner()),
        "subagent view commit",
        move |runtime| {
            runtime.settings_commit_subagent_view(
                SubagentViewPreferenceV1 {
                    auto_open,
                    auto_close,
                },
                expected_version,
            )
        },
    )
    .await
}
