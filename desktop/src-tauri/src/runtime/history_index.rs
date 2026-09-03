//! Durable profile-level Chat navigation metadata.
//!
//! Chat content remains in one canonical stream per Chat. This small separate
//! aggregate records only shell concerns that span streams: selection,
//! pinning, deletion tombstones, fork lineage, and a rebuildable sidebar
//! summary. The summary is a read model: Chat content remains authoritative.

use std::collections::BTreeMap;

use aworkit_local_store::{CommitBatch, CommitOutcome, Deduplication, Event, LocalHistoryStore};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::dto::UiCommandReceipt;

pub(crate) const HISTORY_INDEX_ID: &str = "chat.history-index";
const BRANCH_ID: &str = "main";

/// Compact, rebuildable projection used to render one sidebar row without
/// opening and decoding the Chat's complete canonical event stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ChatSummaryProjection {
    pub head_sequence: u64,
    pub title: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub phase: String,
    pub updated_at: String,
}

impl ChatSummaryProjection {
    /// Creates the complete projection for an unmaterialized draft Chat.
    pub(crate) fn draft(created_at: &str) -> Self {
        Self {
            head_sequence: 0,
            title: "New Chat".into(),
            project_id: None,
            project_name: None,
            phase: "draft".into(),
            updated_at: created_at.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexedChat {
    pub chat_id: StableId,
    pub run_id: StableId,
    pub created_at: String,
    pub parent_chat_id: Option<StableId>,
    pub pinned: bool,
    pub deleted: bool,
    pub ordinal: u64,
    pub summary: Option<ChatSummaryProjection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryIndexState {
    pub entries: Vec<IndexedChat>,
    pub selected_chat_id: StableId,
}

pub(crate) fn events(store: &LocalHistoryStore) -> Result<Vec<Event>, String> {
    store
        .events(HISTORY_INDEX_ID, BRANCH_ID)
        .map_err(|error| format!("cannot read Chat history index: {error}"))
}

/// Folds the immutable metadata aggregate into the current sidebar state.
pub(crate) fn load(store: &LocalHistoryStore) -> Result<Option<HistoryIndexState>, String> {
    let events = events(store)?;
    if events.is_empty() {
        return Ok(None);
    }
    let mut entries = BTreeMap::<String, IndexedChat>::new();
    let mut selected = None::<StableId>;
    for (offset, event) in events.into_iter().enumerate() {
        let chat_id = event
            .payload
            .get("chatId")
            .and_then(Value::as_str)
            .map(|value| StableId::parse(value.to_owned()))
            .transpose()
            .map_err(|_| "stored Chat history index has an invalid Chat ID".to_owned())?;
        match event.kind.as_str() {
            "history.chat-created" => {
                let chat_id = chat_id
                    .ok_or_else(|| "stored Chat creation is missing its Chat ID".to_owned())?;
                let run_id = event
                    .payload
                    .get("runId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "stored Chat creation is missing its Run ID".to_owned())
                    .and_then(|value| {
                        StableId::parse(value.to_owned())
                            .map_err(|_| "stored Chat creation has an invalid Run ID".to_owned())
                    })?;
                let created_at = event
                    .payload
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .unwrap_or("time-unavailable")
                    .to_owned();
                let parent_chat_id = event
                    .payload
                    .get("parentChatId")
                    .and_then(Value::as_str)
                    .map(|value| StableId::parse(value.to_owned()))
                    .transpose()
                    .map_err(|_| "stored Chat fork parent is invalid".to_owned())?;
                entries.entry(chat_id.to_string()).or_insert(IndexedChat {
                    chat_id: chat_id.clone(),
                    run_id,
                    created_at,
                    parent_chat_id,
                    pinned: false,
                    deleted: false,
                    ordinal: u64::try_from(offset)
                        .map_err(|_| "Chat history index is exhausted".to_owned())?,
                    summary: optional_summary(&event.payload),
                });
                selected = Some(chat_id);
            }
            "history.chat-selected" => {
                let chat_id = chat_id
                    .ok_or_else(|| "stored Chat selection is missing its Chat ID".to_owned())?;
                let entry = entries
                    .get(chat_id.as_str())
                    .ok_or_else(|| "stored Chat selection targets an unknown Chat".to_owned())?;
                if entry.deleted {
                    return Err("stored Chat selection targets a deleted Chat".into());
                }
                selected = Some(chat_id);
            }
            "history.chat-pin-changed" => {
                let chat_id =
                    chat_id.ok_or_else(|| "stored pin change is missing its Chat ID".to_owned())?;
                let pinned = event
                    .payload
                    .get("pinned")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| "stored pin change is missing its state".to_owned())?;
                let entry = entries
                    .get_mut(chat_id.as_str())
                    .ok_or_else(|| "stored pin change targets an unknown Chat".to_owned())?;
                entry.pinned = pinned;
            }
            "history.chat-deleted" => {
                let chat_id = chat_id
                    .ok_or_else(|| "stored Chat deletion is missing its Chat ID".to_owned())?;
                let entry = entries
                    .get_mut(chat_id.as_str())
                    .ok_or_else(|| "stored Chat deletion targets an unknown Chat".to_owned())?;
                entry.deleted = true;
                entry.pinned = false;
            }
            "history.chat-summary-updated" => {
                let chat_id = chat_id
                    .ok_or_else(|| "stored Chat summary is missing its Chat ID".to_owned())?;
                let entry = entries
                    .get_mut(chat_id.as_str())
                    .ok_or_else(|| "stored Chat summary targets an unknown Chat".to_owned())?;
                // Summary bytes are explicitly rebuildable. Invalid or older
                // shapes invalidate only this read model and are repaired from
                // the canonical Chat stream during startup.
                entry.summary = optional_summary(&event.payload);
            }
            other => {
                return Err(format!(
                    "stored Chat history event '{other}' is unsupported"
                ));
            }
        }
    }
    let selected_chat_id = selected
        .filter(|chat_id| {
            entries
                .get(chat_id.as_str())
                .is_some_and(|entry| !entry.deleted)
        })
        .or_else(|| {
            entries
                .values()
                .filter(|entry| !entry.deleted)
                .max_by_key(|entry| entry.ordinal)
                .map(|entry| entry.chat_id.clone())
        })
        .ok_or_else(|| "Chat history index has no selectable Chat".to_owned())?;
    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.ordinal);
    Ok(Some(HistoryIndexState {
        entries,
        selected_chat_id,
    }))
}

/// Appends rebuildable summary rows in bounded batches. These facts never
/// carry desktop command receipts and therefore cannot participate in command
/// replay or alter a Chat stream's optimistic version.
pub(crate) fn append_summaries(
    store: &LocalHistoryStore,
    summaries: Vec<(StableId, ChatSummaryProjection)>,
) -> Result<(), String> {
    if summaries.is_empty() {
        return Ok(());
    }
    let mut expected_head = u64::try_from(events(store)?.len())
        .map_err(|_| "Chat history index sequence is exhausted".to_owned())?;
    for chunk in summaries.chunks(64) {
        let projected = chunk
            .iter()
            .enumerate()
            .map(|(offset, (chat_id, summary))| {
                let sequence = expected_head
                    .checked_add(u64::try_from(offset).expect("bounded summary batch"))
                    .and_then(|value| value.checked_add(1))
                    .expect("validated history index sequence");
                Event {
                    event_id: format!("event.chat-history.{sequence}"),
                    kind: "history.chat-summary-updated".into(),
                    payload: json!({
                        "schemaVersion": 1,
                        "chatId": chat_id,
                        "summary": summary,
                    }),
                }
            })
            .collect::<Vec<_>>();
        let outcome = store
            .commit(&CommitBatch {
                chat_id: HISTORY_INDEX_ID.into(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: projected,
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: Vec::new(),
            })
            .map_err(|error| format!("cannot update Chat sidebar summaries: {error}"))?;
        expected_head = match outcome {
            CommitOutcome::Committed(receipt) | CommitOutcome::Existing(receipt) => {
                receipt.head_sequence
            }
        };
    }
    Ok(())
}

/// Initializes an empty index in bounded batches. Creation order also defines
/// the selected Chat, so no separate final selection event is required.
pub(crate) fn initialize(
    store: &LocalHistoryStore,
    chats: &[(StableId, StableId, String)],
) -> Result<(), String> {
    if chats.is_empty() {
        return Err("Chat history initialization requires at least one Chat".into());
    }
    let state = load(store)?;
    let missing = chats
        .iter()
        .filter(|(chat_id, _, _)| {
            !state
                .as_ref()
                .is_some_and(|state| state.entries.iter().any(|entry| entry.chat_id == *chat_id))
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let mut expected_head = u64::try_from(events(store)?.len())
        .map_err(|_| "Chat history index sequence is exhausted".to_owned())?;
    for chunk in missing.chunks(64) {
        let events = chunk
            .iter()
            .enumerate()
            .map(|(offset, &(chat_id, run_id, created_at))| {
                let sequence = expected_head
                    .checked_add(u64::try_from(offset).expect("bounded history index batch"))
                    .and_then(|value| value.checked_add(1))
                    .expect("validated history index sequence");
                Event {
                    event_id: format!("event.chat-history.{sequence}"),
                    kind: "history.chat-created".into(),
                    payload: json!({
                        "schemaVersion": 1,
                        "chatId": chat_id,
                        "runId": run_id,
                        "createdAt": created_at,
                    }),
                }
            })
            .collect::<Vec<_>>();
        let outcome = store
            .commit(&CommitBatch {
                chat_id: HISTORY_INDEX_ID.into(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events,
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: Vec::new(),
            })
            .map_err(|error| format!("cannot initialize Chat history index: {error}"))?;
        expected_head = match outcome {
            CommitOutcome::Committed(receipt) | CommitOutcome::Existing(receipt) => {
                receipt.head_sequence
            }
        };
    }
    Ok(())
}

pub(crate) fn replay(
    store: &LocalHistoryStore,
    command_id: &str,
    command_hash: &str,
) -> Result<Option<UiCommandReceipt>, String> {
    for event in events(store)? {
        if event.payload.get("commandId").and_then(Value::as_str) != Some(command_id) {
            continue;
        }
        if event.payload.get("commandHash").and_then(Value::as_str) != Some(command_hash) {
            return Err("desktop command ID was reused with different content".into());
        }
        let current_version = event
            .payload
            .get("resultHead")
            .and_then(Value::as_u64)
            .ok_or_else(|| "committed Chat history receipt is incomplete".to_owned())?;
        return Ok(Some(UiCommandReceipt {
            command_id: command_id.to_owned(),
            accepted: true,
            current_version,
            reason: None,
            credential_mutation: None,
        }));
    }
    Ok(None)
}

/// Appends one version-fenced history mutation and stores the resulting active
/// Chat head in the idempotent receipt metadata.
pub(crate) fn append_command(
    store: &LocalHistoryStore,
    command_id: &str,
    command_hash: &str,
    result_head: u64,
    facts: Vec<(&str, Value)>,
) -> Result<UiCommandReceipt, String> {
    if facts.is_empty() {
        return Err("Chat history command requires at least one fact".into());
    }
    let existing = events(store)?;
    let expected_head = u64::try_from(existing.len())
        .map_err(|_| "Chat history index sequence is exhausted".to_owned())?;
    let events = facts
        .into_iter()
        .enumerate()
        .map(|(offset, (kind, payload))| {
            let sequence = expected_head
                .checked_add(u64::try_from(offset).expect("bounded history index batch"))
                .and_then(|value| value.checked_add(1))
                .expect("validated history index sequence");
            Event {
                event_id: format!("event.chat-history.{sequence}"),
                kind: kind.to_owned(),
                payload: receipt_payload(payload, command_id, command_hash, result_head),
            }
        })
        .collect::<Vec<_>>();
    let outcome = store
        .commit(&CommitBatch {
            chat_id: HISTORY_INDEX_ID.into(),
            branch_id: BRANCH_ID.into(),
            expected_head,
            events,
            attempt: None,
            checkpoint: None,
            deduplication: Some(Deduplication {
                key_type: "desktop.history-command".into(),
                key: command_id.into(),
                request_hash: command_hash.into(),
            }),
            outbox: Vec::new(),
        })
        .map_err(|error| format!("cannot commit Chat history index: {error}"))?;
    match outcome {
        CommitOutcome::Committed(_) | CommitOutcome::Existing(_) => Ok(UiCommandReceipt {
            command_id: command_id.to_owned(),
            accepted: true,
            current_version: result_head,
            reason: None,
            credential_mutation: None,
        }),
    }
}

fn receipt_payload(
    payload: Value,
    command_id: &str,
    command_hash: &str,
    result_head: u64,
) -> Value {
    let mut object = payload.as_object().cloned().unwrap_or_else(Map::new);
    object.insert("schemaVersion".into(), Value::from(1));
    object.insert("commandId".into(), Value::String(command_id.to_owned()));
    object.insert("commandHash".into(), Value::String(command_hash.to_owned()));
    object.insert("resultHead".into(), Value::from(result_head));
    Value::Object(object)
}

fn optional_summary(payload: &Value) -> Option<ChatSummaryProjection> {
    payload
        .get("summary")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .filter(valid_summary)
}

fn valid_summary(summary: &ChatSummaryProjection) -> bool {
    if summary.title.trim().is_empty() || summary.updated_at.trim().is_empty() {
        return false;
    }
    if !matches!(
        summary.phase.as_str(),
        "draft"
            | "running"
            | "waiting_input"
            | "paused"
            | "awaiting_approval"
            | "cancelling"
            | "cancelled"
            | "completed"
            | "failed"
    ) {
        return false;
    }
    if summary.project_id.is_some() != summary.project_name.is_some() {
        return false;
    }
    true
}
