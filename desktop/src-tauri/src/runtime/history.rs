use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{McpServerManifestV1, ModelToolDefinitionV1};
use aworkit_local_store::{
    CommitBatch, CommitOutcome, Deduplication, Event, LocalHistoryStore, OutboxEntry,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::dto::{
    ChatHistoryEntryDto, ChatProjectionDto, EvidenceRecordDto, RuntimeSnapshot, UiCommandInput,
    UiCommandReceipt,
};
use super::history_index::{self, ChatSummaryProjection, HistoryIndexState, IndexedChat};
use super::project_scope::{FrozenProjectScopeV1, validate_frozen_project_scope};
use super::semantic_events::{
    CommittedChatEventPort, CoreEventEnvelope, SemanticEventCommitter, SemanticEventDraft,
    envelope, event_identity,
};
use super::settings_v2::{
    BuiltInToolConfigurationV2, ModelConfigurationV2, ModelTierConfigurationV2,
    ProviderConfigurationV2,
};

pub(crate) const CHAT_ID: &str = "chat.local";
const BRANCH_ID: &str = "main";
const SESSION_AGGREGATE_ID: &str = "chat.frozen-sessions";
const COMMITTED_EVENT_DESTINATION: &str = "chat.semantic.committed.v1";
// The local history adapter caps the entire serialized commit at 1 MiB. Leave
// ample headroom for the event, deduplication, and backend envelope.
const MAXIMUM_FROZEN_CONTEXT_BYTES: usize = 512 * 1024;
const MAXIMUM_PENDING_COMMAND_BYTES: usize = 320 * 1024;
const MAXIMUM_USER_INPUT_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ChatIdentityV1 {
    pub chat_id: StableId,
    pub run_id: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenCredentialBindingV1 {
    pub credential_ref: StableId,
    pub field_names: BTreeSet<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenToolBindingV1 {
    pub tool_id: String,
    pub tool_hash: String,
    pub tool_snapshot: BuiltInToolConfigurationV2,
    /// Opaque credential-lease metadata frozen with the tool. Values remain
    /// solely in the operating-system credential store and are never part of
    /// Chat history. The durable field deliberately avoids a secret-bearing
    /// name so the semantic-history guard can continue rejecting actual
    /// credential material without rejecting these permitted references.
    #[serde(
        default,
        rename = "opaqueBindings",
        alias = "credentials",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub credentials: Vec<FrozenCredentialBindingV1>,
    /// Exact model-facing definition discovered at freeze for dynamic tools
    /// (MCP). Absent for compile-time owned built-ins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<ModelToolDefinitionV1>,
}

/// Secret-free execution inputs resolved exactly once when the first message
/// starts a Chat/Run. Saved Settings and workflow documents may subsequently
/// change without mutating this record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenChatExecutionContextV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<super::approvals::ApprovalMode>,
    pub schema_version: u16,
    pub identity: ChatIdentityV1,
    pub history_base_head: u64,
    pub start_command_id: StableId,
    pub start_command_hash: String,
    /// Complete, secret-store-free first command needed to recover a crash
    /// after this context commit but before the semantic Chat commit. Legacy
    /// completed contexts legitimately omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_start_command: Option<UiCommandInput>,
    pub settings_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<FrozenProjectScopeV1>,
    pub workflow_id: String,
    pub workflow_name: String,
    pub workflow_version: u64,
    pub workflow_snapshot_hash: String,
    pub workflow_snapshot: Value,
    /// Compatibility sinks for contexts frozen before Agent model turn caps
    /// were removed. New contexts omit both obsolete fields.
    #[serde(default, rename = "agentMaximumTurns", skip_serializing)]
    pub legacy_agent_maximum_turns: Option<u32>,
    #[serde(default, rename = "maximumToolCalls", skip_serializing)]
    pub legacy_maximum_tool_calls: Option<u64>,
    /// Frozen wall-clock allowance for each command in this Chat/Run.
    #[serde(default = "default_run_deadline_millis")]
    pub run_deadline_millis: u64,
    #[serde(default)]
    pub tools: Vec<FrozenToolBindingV1>,
    pub model_tier_id: String,
    pub model_tier_hash: String,
    pub model_tier_snapshot: ModelTierConfigurationV2,
    pub provider_id: String,
    pub provider_name: String,
    pub provider_kind: String,
    pub provider_base_url: String,
    pub provider_hash: String,
    pub provider_snapshot: ProviderConfigurationV2,
    pub model_id: String,
    pub model_name: String,
    pub remote_model_id: String,
    pub model_hash: String,
    pub model_snapshot: ModelConfigurationV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "opaqueBinding")]
    pub credential: Option<FrozenCredentialBindingV1>,
    /// Core-attested MCP manifests frozen with this Run, keyed by server id.
    /// Sessions open from these exact manifests; binding drift fails closed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_manifests: BTreeMap<String, McpServerManifestV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenChatExecutionRecordV1 {
    pub context: FrozenChatExecutionContextV1,
    pub context_hash: String,
}

/// Exact effect-bearing Chat command durably staged before the authority
/// pipeline is entered. It gives restart recovery the original idempotency ID,
/// expected history fence, and input for both first and follow-up messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PendingChatCommandV1 {
    pub schema_version: u16,
    pub frozen_context_hash: String,
    pub command_hash: String,
    pub command: UiCommandInput,
}

#[derive(Clone, Debug)]
pub(crate) struct ConversationMessage {
    pub role: String,
    pub content: String,
    pub images: Vec<aworkit_capability_host::model_images::ImageAttachmentV1>,
}

#[derive(Clone)]
pub(crate) struct ChatHistory {
    store: LocalHistoryStore,
    committed_events: Arc<dyn CommittedChatEventPort>,
}

impl ChatHistory {
    pub(crate) fn open_with_committed_events(
        data_root: &Path,
        committed_events: Arc<dyn CommittedChatEventPort>,
    ) -> Result<Self, String> {
        let store = LocalHistoryStore::open(data_root.join("history").join("aworkit.sqlite3"))
            .map_err(|error| format!("cannot open desktop Chat history: {error}"))?;
        let history = Self {
            store,
            committed_events,
        };
        history.initialize_history_index(data_root)?;
        history.ensure_history_summaries()?;
        history.drain_committed_outbox()?;
        Ok(history)
    }

    pub(crate) fn head(&self) -> Result<u64, String> {
        let chat_id = self.selected_identity()?.chat_id;
        self.head_for_chat(&chat_id)
    }

    /// Reads the indexed stream head without loading or parsing event payloads.
    fn head_for_chat(&self, chat_id: &StableId) -> Result<u64, String> {
        self.store
            .head_sequence(chat_id.as_str(), BRANCH_ID)
            .map(|head| head.unwrap_or(0))
            .map_err(|error| format!("cannot read desktop Chat head: {error}"))
    }

    pub(crate) fn ensure_expected(&self, expected: u64) -> Result<(), String> {
        let actual = self.head()?;
        if expected == actual {
            Ok(())
        } else {
            Err(format!(
                "desktop version conflict: expected {expected}, actual {actual}"
            ))
        }
    }

    pub(crate) fn replay(
        &self,
        command_id: &str,
        command_hash: &str,
    ) -> Result<Option<UiCommandReceipt>, String> {
        if let Some(receipt) = history_index::replay(&self.store, command_id, command_hash)? {
            return Ok(Some(receipt));
        }
        for event in self.events()? {
            if event.payload.get("commandId").and_then(Value::as_str) == Some(command_id) {
                if event.payload.get("commandHash").and_then(Value::as_str) != Some(command_hash) {
                    return Err("desktop command ID was reused with different content".into());
                }
                let current_version = event
                    .payload
                    .get("resultHead")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "committed desktop receipt is incomplete".to_owned())?;
                return Ok(Some(UiCommandReceipt {
                    command_id: command_id.to_owned(),
                    accepted: true,
                    current_version,
                    reason: None,
                    credential_mutation: None,
                }));
            }
        }
        Ok(None)
    }

    pub(crate) fn append(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        facts: Vec<(&str, Value)>,
    ) -> Result<UiCommandReceipt, String> {
        if facts.is_empty() {
            return Err("desktop commit requires at least one semantic fact".into());
        }
        self.ensure_expected(expected_head)?;
        let event_count = u64::try_from(facts.len())
            .map_err(|_| "desktop event count is exhausted".to_owned())?;
        let result_head = expected_head
            .checked_add(event_count)
            .ok_or_else(|| "desktop history sequence is exhausted".to_owned())?;
        let drafts = facts
            .into_iter()
            .map(|(kind, payload)| {
                SemanticEventDraft::new(
                    kind,
                    receipt_payload(payload, command_id, command_hash, result_head),
                )
            })
            .collect::<Vec<_>>();
        let stream_id = self.selected_identity()?.chat_id.to_string();
        validate_span_drafts(&self.events()?, &drafts)?;
        let committed = committed_envelopes(&stream_id, expected_head, &drafts);
        let events = local_events(&stream_id, expected_head, &drafts);
        let outcome = self
            .store
            .commit(&CommitBatch {
                chat_id: stream_id,
                branch_id: BRANCH_ID.into(),
                expected_head,
                events,
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: "desktop.command".into(),
                    key: command_id.into(),
                    request_hash: command_hash.into(),
                }),
                outbox: delivery_outbox(&committed)?,
            })
            .map_err(|error| format!("cannot commit desktop Chat history: {error}"))?;
        let (durable_head, committed) = match outcome {
            CommitOutcome::Committed(receipt) => (receipt.head_sequence, true),
            CommitOutcome::Existing(receipt) => (receipt.head_sequence, false),
        };
        if committed {
            self.drain_committed_outbox()?;
        }
        Ok(UiCommandReceipt {
            command_id: command_id.to_owned(),
            accepted: true,
            current_version: durable_head,
            reason: None,
            credential_mutation: None,
        })
    }

    /// Commits the user-visible beginning of an effect-bearing command before
    /// provider execution starts. The separate deduplication identity makes a
    /// crash/retry a no-op without pretending the command itself has settled.
    pub(crate) fn begin_effect_command(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        facts: Vec<(&str, Value)>,
    ) -> Result<u64, String> {
        if facts.is_empty() {
            return Ok(expected_head);
        }
        self.ensure_expected(expected_head)?;
        let drafts = facts
            .into_iter()
            .map(|(kind, payload)| SemanticEventDraft::new(kind, payload))
            .collect::<Vec<_>>();
        let stream_id = self.selected_identity()?.chat_id.to_string();
        validate_span_drafts(&self.events()?, &drafts)?;
        let committed = committed_envelopes(&stream_id, expected_head, &drafts);
        let outcome = self
            .store
            .commit(&CommitBatch {
                chat_id: stream_id.clone(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: local_events(&stream_id, expected_head, &drafts),
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: "desktop.command.start".into(),
                    key: command_id.into(),
                    request_hash: command_hash.into(),
                }),
                outbox: delivery_outbox(&committed)?,
            })
            .map_err(|error| format!("cannot begin desktop Chat command: {error}"))?;
        match outcome {
            CommitOutcome::Committed(receipt) => {
                self.drain_committed_outbox()?;
                Ok(receipt.head_sequence)
            }
            CommitOutcome::Existing(receipt) => {
                self.drain_committed_outbox()?;
                Ok(receipt.head_sequence)
            }
        }
    }

    pub(crate) fn command_started(&self, command_id: &str) -> Result<bool, String> {
        Ok(self.events()?.iter().any(|event| {
            event.payload.get("requestId").and_then(Value::as_str) == Some(command_id)
                && event.kind == "command.started"
        }))
    }

    /// Produces child-first terminal facts for every span left open in the
    /// current Chat. Cancellation and explicit uncertain abandonment use this
    /// instead of leaving durable cards spinning forever.
    pub(crate) fn open_span_terminal_facts(
        &self,
        status: &str,
        body: &str,
        created_at: &str,
    ) -> Result<Vec<Value>, String> {
        let events = self.events()?;
        let mut terminal = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "span.completed" | "span.failed" | "span.cancelled"
                )
            })
            .filter_map(|event| {
                event
                    .payload
                    .get("spanId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<BTreeSet<_>>();
        let mut facts = Vec::new();
        for event in events
            .into_iter()
            .rev()
            .filter(|event| event.kind == "span.started")
        {
            let Some(span_id) = event.payload.get("spanId").and_then(Value::as_str) else {
                continue;
            };
            if terminal.insert(span_id.to_owned()) {
                facts.push(json!({
                    "schemaVersion": 1,
                    "requestId": event.payload.get("requestId").cloned().unwrap_or(Value::Null),
                    "runId": event.payload.get("runId").cloned().unwrap_or(Value::Null),
                    "spanId": span_id,
                    "status": status,
                    "body": body,
                    "createdAt": created_at,
                    "hasOutput": false,
                    "output": Value::Null,
                }));
            }
        }
        Ok(facts)
    }

    fn drain_committed_outbox(&self) -> Result<(), String> {
        for pending in self
            .store
            .pending_outbox(512)
            .map_err(|error| format!("cannot read committed Chat event outbox: {error}"))?
        {
            if pending.destination != COMMITTED_EVENT_DESTINATION {
                continue;
            }
            let event: CoreEventEnvelope = serde_json::from_value(pending.payload)
                .map_err(|error| format!("committed Chat event outbox is invalid: {error}"))?;
            if self.committed_events.publish(event).is_err() {
                break;
            }
            self.store
                .mark_outbox_delivered(&pending.outbox_id)
                .map_err(|error| format!("cannot acknowledge committed Chat event: {error}"))?;
        }
        Ok(())
    }

    pub(crate) fn conversation(&self) -> Result<Vec<ConversationMessage>, String> {
        let events = self.events()?;
        events
            .into_iter()
            .filter_map(|event| match event.kind.as_str() {
                "message.user" => Some(message_from_event(event, "user")),
                "message.assistant" => Some(message_from_event(event, "assistant")),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn current_chat_identity(&self) -> Result<Option<ChatIdentityV1>, String> {
        Ok(Some(self.selected_identity()?))
    }

    pub(crate) fn ensure_accepts_follow_up(&self) -> Result<(), String> {
        let current = self.events()?;
        if current.iter().any(|event| event.kind == "chat.cancelled") {
            return Err("the current Chat is cancelled and cannot accept more input".into());
        }
        if current.iter().any(|event| event.kind == "execution.failed") {
            return Err("the current Chat failed and cannot accept more input".into());
        }
        if !current.iter().any(|event| event.kind == "chat.started") {
            return Err("cannot enqueue before the current Chat is started".into());
        }
        Ok(())
    }

    pub(crate) fn ensure_cancellable(&self) -> Result<(), String> {
        let current = self.events()?;
        if !current.iter().any(|event| event.kind == "chat.started") {
            return Err("cannot cancel a draft Chat".into());
        }
        if current.iter().any(|event| event.kind == "chat.cancelled") {
            return Err("the current Chat is already cancelled".into());
        }
        if current.iter().any(|event| event.kind == "execution.failed") {
            return Err("the current Chat already failed and cannot be cancelled".into());
        }
        let open_spans = current
            .iter()
            .filter(|event| event.kind == "span.started")
            .filter(|started| {
                let span_id = started.payload.get("spanId").and_then(Value::as_str);
                !current.iter().any(|terminal| {
                    matches!(
                        terminal.kind.as_str(),
                        "span.completed" | "span.failed" | "span.cancelled"
                    ) && terminal.payload.get("spanId").and_then(Value::as_str) == span_id
                })
            })
            .count();
        let open_approval = current
            .iter()
            .filter(|event| event.kind == "approval.requested")
            .any(|requested| {
                let decision_id = requested.payload.get("decisionId").and_then(Value::as_str);
                !current.iter().any(|resolved| {
                    resolved.kind == "approval.resolved"
                        && resolved.payload.get("decisionId").and_then(Value::as_str) == decision_id
                })
            });
        if open_spans == 0 && !open_approval {
            return Err("the current Chat has no running turn to stop".into());
        }
        Ok(())
    }

    pub(crate) fn was_stopped_by(&self, command_id: &str) -> Result<bool, String> {
        Ok(self.events()?.iter().any(|event| {
            event.kind == "chat.turn_stopped"
                && event.payload.get("stopCommandId").and_then(Value::as_str) == Some(command_id)
        }))
    }

    pub(crate) fn current_frozen_context(
        &self,
    ) -> Result<Option<FrozenChatExecutionRecordV1>, String> {
        let Some(identity) = self.current_chat_identity()? else {
            return Ok(None);
        };
        self.frozen_context(&identity.chat_id)
    }

    pub(crate) fn frozen_context(
        &self,
        chat_id: &StableId,
    ) -> Result<Option<FrozenChatExecutionRecordV1>, String> {
        for event in self.session_events()? {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored frozen Chat context is incomplete".into());
            };
            let record = decode_stored_frozen_context_record(value)?;
            if &record.context.identity.chat_id == chat_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    pub(crate) fn pending_context_at_head(
        &self,
        history_head: u64,
    ) -> Result<Option<FrozenChatExecutionRecordV1>, String> {
        let selected_chat_id = self.selected_identity()?.chat_id;
        for event in self.session_events()?.into_iter().rev() {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored frozen Chat context is incomplete".into());
            };
            let record = decode_stored_frozen_context_record(value)?;
            if record.context.identity.chat_id == selected_chat_id
                && record.context.history_base_head == history_head
            {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    /// Returns the one exact staged effect command whose semantic history
    /// fence is still current. A first-start context is also a recovery source
    /// for the narrow crash window before the separate command-stage commit.
    pub(crate) fn pending_effect_command_at_head(
        &self,
        history_head: u64,
    ) -> Result<Option<PendingChatCommandV1>, String> {
        let selected_chat_id = self.selected_identity()?.chat_id;
        let chat_events = self.events_for_chat(&selected_chat_id)?;
        // Decode the profile-level session aggregate exactly once. The former
        // nested lookup reopened and revalidated every frozen context for each
        // staged command, making every history selection quadratic in the
        // number of past commands and contexts.
        let session_events = self.session_events()?;
        let mut contexts = Vec::new();
        let mut contexts_by_hash = BTreeMap::new();
        for event in &session_events {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored frozen Chat context is incomplete".into());
            };
            let context = decode_stored_frozen_context_record(value)?;
            contexts_by_hash.insert(context.context_hash.clone(), context.clone());
            contexts.push(context);
        }
        for event in session_events.iter().rev() {
            if event.kind != "chat.effect-command-staged" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored pending Chat command is incomplete".into());
            };
            let record: PendingChatCommandV1 = serde_json::from_value(value)
                .map_err(|_| "stored pending Chat command is invalid".to_owned())?;
            validate_pending_command_record(&record)?;
            let Some(context) = contexts_by_hash.get(&record.frozen_context_hash) else {
                return Err("stored pending Chat command has no frozen context".into());
            };
            if context.context.identity.chat_id != selected_chat_id {
                continue;
            }
            let settled = chat_events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    "message.assistant"
                        | "approval.requested"
                        | "execution.failed"
                        | "chat.turn_stopped"
                ) && event
                    .payload
                    .get("settlesCommandId")
                    .or_else(|| event.payload.get("commandId"))
                    .and_then(Value::as_str)
                    == Some(record.command.command_id.as_str())
            });
            if !settled {
                return Ok(Some(record));
            }
        }
        let Some(context) = contexts.into_iter().rev().find(|context| {
            context.context.identity.chat_id == selected_chat_id
                && context.context.history_base_head == history_head
        }) else {
            return Ok(None);
        };
        Ok(context
            .context
            .pending_start_command
            .clone()
            .and_then(|command| {
                let settled = chat_events.iter().any(|event| {
                    matches!(
                        event.kind.as_str(),
                        "message.assistant"
                            | "approval.requested"
                            | "execution.failed"
                            | "chat.turn_stopped"
                    ) && event
                        .payload
                        .get("settlesCommandId")
                        .or_else(|| event.payload.get("commandId"))
                        .and_then(Value::as_str)
                        == Some(command.command_id.as_str())
                });
                (!settled).then(|| PendingChatCommandV1 {
                    schema_version: 1,
                    frozen_context_hash: context.context_hash,
                    command_hash: context.context.start_command_hash,
                    command,
                })
            }))
    }

    /// Stages an exact start/enqueue command on the separate session
    /// aggregate before any provider effect. Retrying the same record is a
    /// durable no-op; reusing its command ID with different content is denied.
    pub(crate) fn stage_effect_command(
        &self,
        record: PendingChatCommandV1,
    ) -> Result<PendingChatCommandV1, String> {
        validate_pending_command_record(&record)?;
        for event in self.session_events()? {
            if event.kind != "chat.effect-command-staged" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored pending Chat command is incomplete".into());
            };
            let existing: PendingChatCommandV1 = serde_json::from_value(value)
                .map_err(|_| "stored pending Chat command is invalid".to_owned())?;
            validate_pending_command_record(&existing)?;
            if existing.command.command_id == record.command.command_id {
                return if existing == record {
                    Ok(existing)
                } else {
                    Err("Chat effect command ID was reused with different content".into())
                };
            }
        }
        let existing_events = self.session_events()?;
        let expected_head = u64::try_from(existing_events.len())
            .map_err(|_| "pending Chat command sequence is exhausted".to_owned())?;
        let event_id = format!(
            "event.command.{}",
            record.command_hash.trim_start_matches("sha256:")
        );
        let payload = json!({"schemaVersion":1,"record":record});
        self.store
            .commit(&CommitBatch {
                chat_id: SESSION_AGGREGATE_ID.into(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: vec![Event {
                    event_id,
                    kind: "chat.effect-command-staged".into(),
                    payload,
                }],
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: "chat.effect-command".into(),
                    key: record.command.command_id.clone(),
                    request_hash: record.command_hash.clone(),
                }),
                outbox: Vec::new(),
            })
            .map_err(|error| format!("cannot stage pending Chat command: {error}"))?;
        Ok(record)
    }

    /// Persists one immutable session context before the first provider effect.
    /// This uses a separate aggregate, so the UI history head remains unchanged.
    pub(crate) fn freeze_context(
        &self,
        context: FrozenChatExecutionContextV1,
    ) -> Result<FrozenChatExecutionRecordV1, String> {
        let context_hash = canonical_hash(&context)?;
        let record = FrozenChatExecutionRecordV1 {
            context,
            context_hash,
        };
        validate_frozen_context_record(&record, None)?;
        if let Some(existing) = self.frozen_context(&record.context.identity.chat_id)? {
            return if existing == record {
                Ok(existing)
            } else {
                Err("Chat identity was reused with different frozen execution context".into())
            };
        }
        let existing_events = self.session_events()?;
        let expected_head = u64::try_from(existing_events.len())
            .map_err(|_| "frozen Chat context sequence is exhausted".to_owned())?;
        let event_id = format!(
            "event.session.{}",
            record.context_hash.trim_start_matches("sha256:")
        );
        let payload = json!({"schemaVersion":1,"record":record});
        self.store
            .commit(&CommitBatch {
                chat_id: SESSION_AGGREGATE_ID.into(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: vec![Event {
                    event_id,
                    kind: "chat.execution-context-frozen".into(),
                    payload,
                }],
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: "chat.execution-context".into(),
                    key: record.context.identity.chat_id.to_string(),
                    request_hash: record.context_hash.clone(),
                }),
                outbox: Vec::new(),
            })
            .map_err(|error| format!("cannot freeze Chat execution context: {error}"))?;
        Ok(record)
    }

    fn initialize_history_index(&self, data_root: &Path) -> Result<(), String> {
        let legacy_events = self
            .store
            .events(CHAT_ID, BRANCH_ID)
            .map_err(|error| format!("cannot inspect legacy desktop Chat history: {error}"))?;
        if legacy_events.is_empty() {
            if history_index::load(&self.store)?.is_some() {
                return Ok(());
            }
            let identity =
                identity_for_seed(&format!("initial-chat:{}", data_root.to_string_lossy()))?;
            return history_index::initialize(
                &self.store,
                &[(identity.chat_id, identity.run_id, now_label())],
            );
        }

        let segments = legacy_chat_segments(legacy_events)?;
        let mut chats = Vec::with_capacity(segments.len());
        for (identity, events) in segments {
            self.copy_legacy_chat(&identity.chat_id, &events)?;
            let created_at = events
                .iter()
                .find_map(event_created_at)
                .unwrap_or_else(now_label);
            chats.push((identity.chat_id, identity.run_id, created_at));
        }
        history_index::initialize(&self.store, &chats)
    }

    /// One-time upgrade for history indexes created before compact sidebar
    /// summaries existed. Later snapshots only refresh the selected Chat.
    fn ensure_history_summaries(&self) -> Result<(), String> {
        let missing = self
            .index()?
            .entries
            .into_iter()
            .filter(|entry| !entry.deleted && entry.summary.is_none())
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }
        let frozen = self.frozen_contexts()?;
        let mut summaries = Vec::with_capacity(missing.len());
        for entry in missing {
            let events = self.events_for_chat(&entry.chat_id)?;
            summaries.push((
                entry.chat_id.clone(),
                sidebar_summary(
                    &events,
                    frozen.get(entry.chat_id.as_str()),
                    &entry.created_at,
                ),
            ));
        }
        history_index::append_summaries(&self.store, summaries)
    }

    fn copy_legacy_chat(&self, chat_id: &StableId, events: &[Event]) -> Result<(), String> {
        let existing = self.events_for_chat(chat_id)?;
        if existing.len() > events.len()
            || existing
                .iter()
                .zip(events)
                .any(|(left, right)| left.kind != right.kind || left.payload != right.payload)
        {
            return Err("partially migrated Chat history differs from its legacy source".into());
        }
        let stream_id = chat_id.to_string();
        for chunk in events[existing.len()..].chunks(64) {
            let expected_head = u64::try_from(self.events_for_chat(chat_id)?.len())
                .map_err(|_| "migrated Chat history sequence is exhausted".to_owned())?;
            let copied = chunk
                .iter()
                .enumerate()
                .map(|(offset, event)| {
                    let sequence = expected_head
                        .checked_add(u64::try_from(offset).expect("bounded migration batch"))
                        .and_then(|value| value.checked_add(1))
                        .expect("validated migration sequence");
                    Event {
                        event_id: event_identity(&stream_id, BRANCH_ID, sequence),
                        kind: event.kind.clone(),
                        payload: event.payload.clone(),
                    }
                })
                .collect::<Vec<_>>();
            self.store
                .commit(&CommitBatch {
                    chat_id: stream_id.clone(),
                    branch_id: BRANCH_ID.into(),
                    expected_head,
                    events: copied,
                    attempt: None,
                    checkpoint: None,
                    deduplication: None,
                    outbox: Vec::new(),
                })
                .map_err(|error| format!("cannot migrate legacy Chat history: {error}"))?;
        }
        Ok(())
    }

    fn index(&self) -> Result<HistoryIndexState, String> {
        history_index::load(&self.store)?
            .ok_or_else(|| "Chat history index is not initialized".to_owned())
    }

    fn selected_identity(&self) -> Result<ChatIdentityV1, String> {
        let index = self.index()?;
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.chat_id == index.selected_chat_id && !entry.deleted)
            .ok_or_else(|| "selected Chat is unavailable".to_owned())?;
        Ok(ChatIdentityV1 {
            chat_id: entry.chat_id.clone(),
            run_id: entry.run_id.clone(),
        })
    }

    pub(crate) fn identity(&self, chat_id: &str) -> Result<ChatIdentityV1, String> {
        let entry = self.require_visible_entry(chat_id)?;
        Ok(ChatIdentityV1 {
            chat_id: entry.chat_id,
            run_id: entry.run_id,
        })
    }

    fn require_visible_entry(&self, chat_id: &str) -> Result<IndexedChat, String> {
        self.index()?
            .entries
            .into_iter()
            .find(|entry| entry.chat_id.as_str() == chat_id && !entry.deleted)
            .ok_or_else(|| format!("Chat '{chat_id}' does not exist or was deleted"))
    }

    pub(crate) fn create_chat(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
    ) -> Result<UiCommandReceipt, String> {
        self.ensure_expected(expected_head)?;
        let identity = identity_for_seed(command_id)?;
        let created_at = now_label();
        let summary = ChatSummaryProjection::draft(&created_at);
        history_index::append_command(
            &self.store,
            command_id,
            command_hash,
            0,
            vec![(
                "history.chat-created",
                json!({
                    "chatId": identity.chat_id,
                    "runId": identity.run_id,
                    "createdAt": created_at,
                    "summary": summary,
                }),
            )],
        )
    }

    pub(crate) fn select_chat(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        chat_id: &str,
    ) -> Result<UiCommandReceipt, String> {
        self.ensure_expected(expected_head)?;
        let target = self.require_visible_entry(chat_id)?;
        let target_head = self.head_for_chat(&target.chat_id)?;
        history_index::append_command(
            &self.store,
            command_id,
            command_hash,
            target_head,
            vec![(
                "history.chat-selected",
                json!({"chatId": target.chat_id, "createdAt": now_label()}),
            )],
        )
    }

    pub(crate) fn set_chat_pinned(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        chat_id: &str,
        pinned: bool,
    ) -> Result<UiCommandReceipt, String> {
        self.ensure_expected(expected_head)?;
        let target = self.require_visible_entry(chat_id)?;
        history_index::append_command(
            &self.store,
            command_id,
            command_hash,
            expected_head,
            vec![(
                "history.chat-pin-changed",
                json!({
                    "chatId": target.chat_id,
                    "pinned": pinned,
                    "createdAt": now_label(),
                }),
            )],
        )
    }

    pub(crate) fn delete_chat(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        chat_id: &str,
    ) -> Result<UiCommandReceipt, String> {
        self.ensure_expected(expected_head)?;
        let target = self.require_visible_entry(chat_id)?;
        let index = self.index()?;
        let mut facts = vec![(
            "history.chat-deleted",
            json!({"chatId": target.chat_id, "createdAt": now_label()}),
        )];
        let result_head = if index.selected_chat_id == target.chat_id {
            if let Some(next) = index
                .entries
                .iter()
                .filter(|entry| !entry.deleted && entry.chat_id != target.chat_id)
                .max_by_key(|entry| entry.ordinal)
            {
                facts.push((
                    "history.chat-selected",
                    json!({"chatId": next.chat_id, "createdAt": now_label()}),
                ));
                self.head_for_chat(&next.chat_id)?
            } else {
                let replacement = identity_for_seed(&format!("{command_id}.replacement"))?;
                let created_at = now_label();
                let summary = ChatSummaryProjection::draft(&created_at);
                facts.push((
                    "history.chat-created",
                    json!({
                        "chatId": replacement.chat_id,
                        "runId": replacement.run_id,
                        "createdAt": created_at,
                        "summary": summary,
                    }),
                ));
                0
            }
        } else {
            expected_head
        };
        history_index::append_command(&self.store, command_id, command_hash, result_head, facts)
    }

    pub(crate) fn append_fork_content(
        &self,
        identity: &ChatIdentityV1,
        command_id: &str,
        command_hash: &str,
        facts: Vec<(&str, Value)>,
    ) -> Result<u64, String> {
        let stream_id = identity.chat_id.to_string();
        let result_head = u64::try_from(facts.len())
            .map_err(|_| "forked Chat sequence is exhausted".to_owned())?;
        let drafts = facts
            .into_iter()
            .map(|(kind, payload)| {
                SemanticEventDraft::new(
                    kind,
                    receipt_payload(payload, command_id, command_hash, result_head),
                )
            })
            .collect::<Vec<_>>();
        let existing = self.events_for_chat(&identity.chat_id)?;
        if existing.len() > drafts.len()
            || existing
                .iter()
                .zip(&drafts)
                .any(|(event, draft)| event.kind != draft.kind || event.payload != draft.payload)
        {
            return Err("partially created fork differs from its source Chat".into());
        }
        let mut expected_head = u64::try_from(existing.len())
            .map_err(|_| "forked Chat sequence is exhausted".to_owned())?;
        for chunk in drafts[existing.len()..].chunks(64) {
            self.store
                .commit(&CommitBatch {
                    chat_id: stream_id.clone(),
                    branch_id: BRANCH_ID.into(),
                    expected_head,
                    events: local_events(&stream_id, expected_head, chunk),
                    attempt: None,
                    checkpoint: None,
                    deduplication: Some(Deduplication {
                        key_type: "desktop.fork-content".into(),
                        key: format!("{command_id}.{expected_head}"),
                        request_hash: command_hash.into(),
                    }),
                    outbox: Vec::new(),
                })
                .map_err(|error| format!("cannot create forked Chat history: {error}"))?;
            expected_head = expected_head
                .checked_add(u64::try_from(chunk.len()).expect("bounded fork batch"))
                .ok_or_else(|| "forked Chat sequence is exhausted".to_owned())?;
        }
        Ok(result_head)
    }

    pub(crate) fn record_fork(
        &self,
        command_id: &str,
        command_hash: &str,
        parent_chat_id: &StableId,
        child: &ChatIdentityV1,
        child_head: u64,
    ) -> Result<UiCommandReceipt, String> {
        self.require_visible_entry(parent_chat_id.as_str())?;
        history_index::append_command(
            &self.store,
            command_id,
            command_hash,
            child_head,
            vec![(
                "history.chat-created",
                json!({
                    "chatId": child.chat_id,
                    "runId": child.run_id,
                    "parentChatId": parent_chat_id,
                    "createdAt": now_label(),
                }),
            )],
        )
    }

    fn history_entries(index: HistoryIndexState) -> Vec<ChatHistoryEntryDto> {
        let mut entries = Vec::new();
        for entry in index.entries {
            if entry.deleted {
                continue;
            }
            entries.push(Self::history_entry(entry));
        }
        entries.sort_by(|left, right| {
            sortable_time(&right.updated_at)
                .cmp(&sortable_time(&left.updated_at))
                .then_with(|| right.chat_id.cmp(&left.chat_id))
        });
        entries
    }

    fn history_entry(entry: IndexedChat) -> ChatHistoryEntryDto {
        let summary = entry
            .summary
            .unwrap_or_else(|| ChatSummaryProjection::draft(&entry.created_at));
        ChatHistoryEntryDto {
            chat_id: entry.chat_id.to_string(),
            run_id: entry.run_id.to_string(),
            title: summary.title,
            project_id: summary.project_id,
            project_name: summary.project_name,
            phase: summary.phase,
            pinned: entry.pinned,
            parent_chat_id: entry.parent_chat_id.map(|value| value.to_string()),
            created_at: entry.created_at,
            updated_at: summary.updated_at,
        }
    }

    pub(crate) fn snapshot(&self, after_sequence: u64) -> Result<RuntimeSnapshot, String> {
        let mut index = self.index()?;
        let indexed_position = index
            .entries
            .iter()
            .position(|entry| entry.chat_id == index.selected_chat_id && !entry.deleted)
            .ok_or_else(|| "selected Chat is unavailable".to_owned())?;
        let indexed = index.entries[indexed_position].clone();
        let identity = ChatIdentityV1 {
            chat_id: indexed.chat_id.clone(),
            run_id: indexed.run_id.clone(),
        };
        let all_events = self.events_for_chat(&identity.chat_id)?;
        let head = u64::try_from(all_events.len())
            .map_err(|_| "Chat history sequence is exhausted".to_owned())?;
        let current = all_events.clone();
        let frozen = self.frozen_context(&identity.chat_id)?;
        let evidence = evidence(&current);
        let started = current.iter().any(|event| event.kind == "chat.started");
        let title = current
            .iter()
            .find(|event| event.kind == "message.user")
            .and_then(|event| event.payload.get("body"))
            .and_then(Value::as_str)
            .map(compact_title)
            .unwrap_or_else(|| "New Chat".into());
        let phase = projected_phase(&current);
        let chat = ChatProjectionDto {
            approval_mode: Default::default(),
            chat_id: identity.chat_id.to_string(),
            run_id: identity.run_id.to_string(),
            title,
            scope: frozen
                .as_ref()
                .and_then(|record| record.context.project.as_ref())
                .map_or_else(
                    || "No project".into(),
                    |project| project.project_name.clone(),
                ),
            workflow_id: frozen
                .as_ref()
                .map(|record| record.context.workflow_id.clone()),
            workflow_name: frozen
                .as_ref()
                .map(|record| record.context.workflow_name.clone())
                .or_else(|| started.then(|| "Legacy workflow".into())),
            branch: frozen
                .as_ref()
                .and_then(|record| record.context.project.as_ref())
                .and_then(|project| project.branch.clone()),
            project_id: frozen
                .as_ref()
                .and_then(|record| record.context.project.as_ref())
                .map(|project| project.project_id.clone()),
            phase: phase.into(),
            locked_workflow: frozen.is_some() || started,
            queued_inputs: Vec::new(),
            expected_version: head,
            disabled_reason: None,
            recovery_pending: false,
        };
        let state_hash = canonical_hash(&json!({
            "throughSequence": head,
            "reducerVersion": "chat.semantic.reducer.v1",
            "chat": &chat,
            "evidence": &evidence,
        }))?;
        let summary = sidebar_summary(&current, frozen.as_ref(), &indexed.created_at);
        if indexed.summary.as_ref() != Some(&summary) {
            history_index::append_summaries(
                &self.store,
                vec![(identity.chat_id.clone(), summary.clone())],
            )?;
            index.entries[indexed_position].summary = Some(summary);
        }
        let history = Self::history_entries(index);
        let stream_id = identity.chat_id.to_string();
        let events = all_events
            .into_iter()
            .enumerate()
            .filter_map(|(offset, event)| {
                let sequence = u64::try_from(offset).ok()?.checked_add(1)?;
                (sequence > after_sequence).then(|| {
                    envelope(
                        &stream_id,
                        BRANCH_ID,
                        sequence,
                        SemanticEventDraft::new(event.kind, event.payload),
                    )
                })
            })
            .collect();
        Ok(RuntimeSnapshot {
            version: head,
            through_sequence: head,
            reducer_version: "chat.semantic.reducer.v1".into(),
            state_hash,
            chat,
            history,
            projects: Vec::new(),
            evidence,
            events,
        })
    }

    fn events(&self) -> Result<Vec<Event>, String> {
        let chat_id = self.selected_identity()?.chat_id;
        self.events_for_chat(&chat_id)
    }

    pub(crate) fn events_for_chat(&self, chat_id: &StableId) -> Result<Vec<Event>, String> {
        self.store
            .events(chat_id.as_str(), BRANCH_ID)
            .map_err(|error| format!("cannot read desktop Chat history: {error}"))
    }

    fn session_events(&self) -> Result<Vec<Event>, String> {
        self.store
            .events(SESSION_AGGREGATE_ID, BRANCH_ID)
            .map_err(|error| format!("cannot read frozen Chat contexts: {error}"))
    }

    /// Decodes frozen contexts once while upgrading legacy sidebar summaries.
    /// Ordinary snapshots resolve only the selected Chat's frozen context.
    fn frozen_contexts(&self) -> Result<BTreeMap<String, FrozenChatExecutionRecordV1>, String> {
        let mut contexts = BTreeMap::new();
        for event in self.session_events()? {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let value = event
                .payload
                .get("record")
                .cloned()
                .ok_or_else(|| "stored frozen Chat context is incomplete".to_owned())?;
            let record = decode_stored_frozen_context_record(value)?;
            contexts.insert(record.context.identity.chat_id.to_string(), record);
        }
        Ok(contexts)
    }
}

impl SemanticEventCommitter for ChatHistory {
    fn commit(&self, drafts: Vec<SemanticEventDraft>) -> Result<Vec<CoreEventEnvelope>, String> {
        if drafts.is_empty() {
            return Ok(Vec::new());
        }
        let expected_head = self.head()?;
        let stream_id = self.selected_identity()?.chat_id.to_string();
        validate_span_drafts(&self.events()?, &drafts)?;
        let committed = committed_envelopes(&stream_id, expected_head, &drafts);
        let outcome = self
            .store
            .commit(&CommitBatch {
                chat_id: stream_id.clone(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: local_events(&stream_id, expected_head, &drafts),
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: delivery_outbox(&committed)?,
            })
            .map_err(|error| format!("cannot commit semantic Chat events: {error}"))?;
        if matches!(outcome, CommitOutcome::Existing(_)) {
            return Err("semantic event commit unexpectedly resolved as an existing batch".into());
        }
        self.drain_committed_outbox()?;
        Ok(committed)
    }

    fn committed_events(&self) -> Result<Vec<CoreEventEnvelope>, String> {
        let stream_id = self.selected_identity()?.chat_id.to_string();
        Ok(self
            .events()?
            .into_iter()
            .enumerate()
            .map(|(offset, event)| {
                envelope(
                    &stream_id,
                    BRANCH_ID,
                    u64::try_from(offset)
                        .expect("bounded history offset")
                        .saturating_add(1),
                    SemanticEventDraft::new(event.kind, event.payload),
                )
            })
            .collect())
    }
}

#[derive(Default)]
struct SpanLedgerState {
    started: BTreeSet<String>,
    terminal: BTreeSet<String>,
    parents: BTreeMap<String, String>,
}

fn validate_span_drafts(history: &[Event], drafts: &[SemanticEventDraft]) -> Result<(), String> {
    let mut state = SpanLedgerState::default();
    for event in history {
        observe_existing_span(&mut state, &event.kind, &event.payload);
    }
    for draft in drafts {
        let span_id = draft.payload.get("spanId").and_then(Value::as_str);
        match draft.kind.as_str() {
            "span.started" => {
                let span_id = span_id.ok_or_else(|| "span.started requires spanId".to_owned())?;
                if !state.started.insert(span_id.to_owned()) {
                    return Err(format!("span '{span_id}' started more than once"));
                }
                if let Some(parent) = draft.payload.get("parentSpanId").and_then(Value::as_str) {
                    if !state.started.contains(parent) || state.terminal.contains(parent) {
                        return Err(format!(
                            "span '{span_id}' has missing or terminal parent '{parent}'"
                        ));
                    }
                    state.parents.insert(span_id.to_owned(), parent.to_owned());
                }
            }
            "span.completed" | "span.failed" | "span.cancelled" => {
                let span_id = span_id.ok_or_else(|| format!("{} requires spanId", draft.kind))?;
                if !state.started.contains(span_id) {
                    return Err(format!("span '{span_id}' terminated before it started"));
                }
                if state.terminal.contains(span_id) {
                    return Err(format!("span '{span_id}' terminated more than once"));
                }
                if let Some(open_child) = state.parents.iter().find_map(|(child, parent)| {
                    (parent == span_id && !state.terminal.contains(child)).then_some(child)
                }) {
                    return Err(format!(
                        "span '{span_id}' cannot terminate while child '{open_child}' is open"
                    ));
                }
                state.terminal.insert(span_id.to_owned());
            }
            "span.updated" | "span.content_delta" | "span.usage" | "tool.requested" => {
                let span_id = span_id.ok_or_else(|| format!("{} requires spanId", draft.kind))?;
                if !state.started.contains(span_id) || state.terminal.contains(span_id) {
                    return Err(format!(
                        "{} targets missing or terminal span '{span_id}'",
                        draft.kind
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn observe_existing_span(state: &mut SpanLedgerState, kind: &str, payload: &Value) {
    let Some(span_id) = payload.get("spanId").and_then(Value::as_str) else {
        return;
    };
    if kind == "span.started" {
        state.started.insert(span_id.to_owned());
        if let Some(parent) = payload.get("parentSpanId").and_then(Value::as_str) {
            state.parents.insert(span_id.to_owned(), parent.to_owned());
        }
    } else if matches!(kind, "span.completed" | "span.failed" | "span.cancelled") {
        state.terminal.insert(span_id.to_owned());
    }
}

fn committed_envelopes(
    stream_id: &str,
    expected_head: u64,
    drafts: &[SemanticEventDraft],
) -> Vec<CoreEventEnvelope> {
    drafts
        .iter()
        .cloned()
        .enumerate()
        .map(|(offset, draft)| {
            let sequence = expected_head
                .checked_add(u64::try_from(offset).expect("bounded semantic batch"))
                .and_then(|value| value.checked_add(1))
                .expect("validated semantic sequence");
            envelope(stream_id, BRANCH_ID, sequence, draft)
        })
        .collect()
}

fn delivery_outbox(events: &[CoreEventEnvelope]) -> Result<Vec<OutboxEntry>, String> {
    events
        .iter()
        .map(|event| {
            Ok(OutboxEntry {
                outbox_id: format!("outbox.{}", event.event_id),
                destination: COMMITTED_EVENT_DESTINATION.into(),
                payload: serde_json::to_value(event)
                    .map_err(|error| format!("cannot encode committed Chat event: {error}"))?,
            })
        })
        .collect()
}

fn local_events(stream_id: &str, expected_head: u64, drafts: &[SemanticEventDraft]) -> Vec<Event> {
    drafts
        .iter()
        .enumerate()
        .map(|(offset, draft)| {
            let sequence = expected_head
                .checked_add(u64::try_from(offset).expect("bounded semantic batch"))
                .and_then(|value| value.checked_add(1))
                .expect("validated semantic sequence");
            Event {
                event_id: event_identity(stream_id, BRANCH_ID, sequence),
                kind: draft.kind.clone(),
                payload: draft.payload.clone(),
            }
        })
        .collect()
}

pub(crate) fn identity_for_seed(seed: &str) -> Result<ChatIdentityV1, String> {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("aworkit-chat-run-v1:{seed}").as_bytes())
    );
    let suffix = &digest[..40];
    Ok(ChatIdentityV1 {
        chat_id: StableId::parse(format!("chat.{suffix}")).map_err(|error| error.to_string())?,
        run_id: StableId::parse(format!("run.{suffix}")).map_err(|error| error.to_string())?,
    })
}

pub(crate) fn message_fact(
    body: &str,
    created_at: &str,
    model: Option<&str>,
    input_units: Option<u64>,
    output_units: Option<u64>,
) -> Value {
    let mut value = json!({
        "schemaVersion": 1,
        "body": body,
        "createdAt": created_at,
    });
    if let Some(object) = value.as_object_mut() {
        if let Some(model) = model {
            object.insert("model".into(), Value::String(model.to_owned()));
        }
        if let Some(units) = input_units {
            object.insert("inputUnits".into(), Value::from(units));
        }
        if let Some(units) = output_units {
            object.insert("outputUnits".into(), Value::from(units));
        }
    }
    value
}

pub(crate) fn now_label() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| "time-unavailable".into(),
        |value| value.as_secs().to_string(),
    )
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

fn legacy_chat_segments(events: Vec<Event>) -> Result<Vec<(ChatIdentityV1, Vec<Event>)>, String> {
    let mut segments = Vec::<Vec<Event>>::new();
    for event in events {
        if event.kind == "chat.created" && segments.last().is_some_and(|items| !items.is_empty()) {
            segments.push(Vec::new());
        }
        if segments.is_empty() {
            segments.push(Vec::new());
        }
        segments.last_mut().expect("segment exists").push(event);
    }
    segments
        .into_iter()
        .enumerate()
        .map(|(ordinal, events)| {
            let identity = events
                .iter()
                .find(|event| matches!(event.kind.as_str(), "chat.created" | "chat.started"))
                .map(identity_from_event)
                .transpose()?
                .flatten()
                .unwrap_or(identity_for_seed(&format!("legacy-chat-{ordinal}"))?);
            Ok((identity, events))
        })
        .collect()
}

fn projected_phase(events: &[Event]) -> &'static str {
    if events.iter().any(|event| event.kind == "chat.cancelled") {
        return "cancelled";
    }
    if events.iter().any(|event| event.kind == "execution.failed") {
        return "failed";
    }
    let has_open_approval = events
        .iter()
        .filter(|event| event.kind == "approval.requested")
        .any(|event| {
            let decision_id = event
                .payload
                .get("decisionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !events.iter().any(|resolved| {
                resolved.kind == "approval.resolved"
                    && resolved.payload.get("decisionId").and_then(Value::as_str)
                        == Some(decision_id)
            })
        });
    if has_open_approval {
        "awaiting_approval"
    } else if events.iter().any(|event| event.kind == "chat.turn_stopped") {
        "waiting_input"
    } else if events.iter().any(|event| event.kind == "message.assistant") {
        "waiting_input"
    } else {
        "draft"
    }
}

/// Folds only the selected canonical stream into the compact sidebar row.
/// Other Chat streams are never opened during an ordinary snapshot.
fn sidebar_summary(
    events: &[Event],
    frozen: Option<&FrozenChatExecutionRecordV1>,
    created_at: &str,
) -> ChatSummaryProjection {
    let title = events
        .iter()
        .find(|event| event.kind == "message.user")
        .and_then(|event| event.payload.get("body"))
        .and_then(Value::as_str)
        .map(compact_title)
        .unwrap_or_else(|| "New Chat".into());
    let updated_at = events
        .iter()
        .rev()
        .find_map(event_created_at)
        .unwrap_or_else(|| created_at.to_owned());
    let project = frozen.and_then(|record| record.context.project.as_ref());
    ChatSummaryProjection {
        head_sequence: u64::try_from(events.len()).unwrap_or(u64::MAX),
        title,
        project_id: project.map(|project| project.project_id.clone()),
        project_name: project.map(|project| project.project_name.clone()),
        phase: projected_phase(events).into(),
        updated_at,
    }
}

fn event_created_at(event: &Event) -> Option<String> {
    event
        .payload
        .get("createdAt")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn sortable_time(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

fn identity_from_event(event: &Event) -> Result<Option<ChatIdentityV1>, String> {
    let chat_id = event.payload.get("chatId").and_then(Value::as_str);
    let run_id = event.payload.get("runId").and_then(Value::as_str);
    match (chat_id, run_id) {
        (None, None) => Ok(None),
        (Some(chat_id), Some(run_id)) => Ok(Some(ChatIdentityV1 {
            chat_id: StableId::parse(chat_id.to_owned())
                .map_err(|_| "stored Chat identity is invalid".to_owned())?,
            run_id: StableId::parse(run_id.to_owned())
                .map_err(|_| "stored Run identity is invalid".to_owned())?,
        })),
        _ => Err("stored Chat/Run identity is incomplete".into()),
    }
}

/// Decodes a durable context while verifying the hash against the exact stored
/// JSON. This keeps additive serde defaults backward compatible without ever
/// treating the re-serialized, default-expanded value as the original bytes.
fn decode_stored_frozen_context_record(
    value: Value,
) -> Result<FrozenChatExecutionRecordV1, String> {
    let stored_context_hash = value
        .get("context")
        .ok_or_else(|| "stored frozen Chat context is incomplete".to_owned())
        .and_then(canonical_hash)?;
    let record: FrozenChatExecutionRecordV1 = serde_json::from_value(value)
        .map_err(|_| "stored frozen Chat context is invalid".to_owned())?;
    validate_frozen_context_record(&record, Some(&stored_context_hash))?;
    Ok(record)
}

fn validate_frozen_context_record(
    record: &FrozenChatExecutionRecordV1,
    stored_context_hash: Option<&str>,
) -> Result<(), String> {
    let context = &record.context;
    let context_hash_matches = match stored_context_hash {
        Some(hash) => hash == record.context_hash,
        None => canonical_hash(context)? == record.context_hash,
    };
    if context.schema_version != 1
        || context.settings_version == 0
        || context.workflow_version == 0
        || context.workflow_id.is_empty()
        || context.workflow_name.trim().is_empty()
        || context.model_tier_id.is_empty()
        || context.provider_id.is_empty()
        || context.provider_name.trim().is_empty()
        || context.provider_base_url.is_empty()
        || context.model_id.is_empty()
        || context.model_name.trim().is_empty()
        || context.remote_model_id.is_empty()
        || !matches!(
            context.provider_kind.as_str(),
            "openai_compatible" | "anthropic" | "gemini"
        )
        || !is_sha256(&context.workflow_snapshot_hash)
        || !is_sha256(&context.start_command_hash)
        || !is_sha256(&context.model_tier_hash)
        || !is_sha256(&context.provider_hash)
        || !is_sha256(&context.model_hash)
        || !is_sha256(&record.context_hash)
        || canonical_hash(&context.workflow_snapshot)? != context.workflow_snapshot_hash
        || canonical_hash(&context.model_tier_snapshot)? != context.model_tier_hash
        || canonical_hash(&context.provider_snapshot)? != context.provider_hash
        || canonical_hash(&context.model_snapshot)? != context.model_hash
        || context.model_tier_snapshot.id != context.model_tier_id
        || context.provider_snapshot.id != context.provider_id
        || context.provider_snapshot.name != context.provider_name
        || context.provider_snapshot.kind != context.provider_kind
        || context.provider_snapshot.base_url != context.provider_base_url
        || context.model_snapshot.id != context.model_id
        || context.model_snapshot.name != context.model_name
        || context.model_snapshot.remote_id != context.remote_model_id
        || !context_hash_matches
    {
        return Err("stored frozen Chat context failed integrity validation".into());
    }
    if let Some(command) = &context.pending_start_command {
        let input = super::images::command_text(&command.payload).ok();
        let attachments_are_valid = super::images::command_images(&command.payload).is_ok();
        let selected_project_id = match command.payload.get("projectId") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.trim().is_empty() => Some(value.as_str()),
            _ => return Err("stored pending Chat start has an invalid project selection".into()),
        };
        if command.schema_version != 1
            || command.action != "start"
            || command.command_id != context.start_command_id.as_str()
            || command.expected_version != context.history_base_head
            || command.payload.get("workflowId").and_then(Value::as_str)
                != Some(context.workflow_id.as_str())
            || input
                .map(|value| value.len() > MAXIMUM_USER_INPUT_BYTES || value.contains('\0'))
                .unwrap_or(true)
            || !attachments_are_valid
            || selected_project_id
                != context
                    .project
                    .as_ref()
                    .map(|project| project.project_id.as_str())
        {
            return Err("stored pending Chat start failed structural validation".into());
        }
    }
    if serde_json::to_vec(record)
        .map_err(|_| "stored frozen Chat context cannot be encoded".to_owned())?
        .len()
        > MAXIMUM_FROZEN_CONTEXT_BYTES
    {
        return Err("stored frozen Chat context exceeds its size bound".into());
    }
    if let Some(credential) = &context.credential
        && (credential.revision == 0
            || credential.field_names.is_empty()
            || credential
                .field_names
                .iter()
                .any(|field| field.is_empty() || field.chars().any(char::is_control)))
    {
        return Err("stored frozen Chat credential metadata is invalid".into());
    }
    if let Some(project) = &context.project {
        validate_frozen_project_scope(project)?;
    }
    let mut tool_ids = BTreeSet::new();
    if context.tools.iter().any(|tool| {
        let frozen_refs = tool
            .credentials
            .iter()
            .map(|credential| credential.credential_ref.as_str())
            .collect::<BTreeSet<_>>();
        let configured_refs = tool
            .tool_snapshot
            .credential_bindings
            .iter()
            .map(|binding| binding.credential_ref.as_str())
            .collect::<BTreeSet<_>>();
        !tool_ids.insert(tool.tool_id.as_str())
            || tool.tool_id != tool.tool_snapshot.id
            || !tool.tool_snapshot.enabled
            || canonical_hash(&tool.tool_snapshot).ok().as_deref() != Some(&tool.tool_hash)
            || frozen_refs != configured_refs
            || tool.credentials.iter().any(|credential| {
                credential.revision == 0
                    || credential.field_names.is_empty()
                    || credential
                        .field_names
                        .iter()
                        .any(|field| field.is_empty() || field.chars().any(char::is_control))
            })
            || !(super::documents::builtin_tool_binding_ids().contains(&tool.tool_id)
                || (tool.tool_id.starts_with("mcp://")
                    && tool.definition.is_some()
                    && tool.tool_snapshot.configuration.len() == 2
                    && tool.tool_snapshot.configuration.contains_key("serverId")
                    && tool.tool_snapshot.configuration.contains_key("tool")))
    }) {
        return Err("stored frozen Chat tool bindings failed integrity validation".into());
    }
    if context
        .tools
        .iter()
        .any(|tool| tool.tool_snapshot.requires_project)
        && context.project.is_none()
        || !(30_000..=3_600_000).contains(&context.run_deadline_millis)
    {
        return Err("stored frozen Chat Agent execution context is invalid".into());
    }
    Ok(())
}

fn validate_pending_command_record(record: &PendingChatCommandV1) -> Result<(), String> {
    let command = &record.command;
    let input = super::images::command_text(&command.payload).ok();
    let attachments_are_valid = super::images::command_images(&command.payload).is_ok();
    let action_shape_is_valid = match command.action.as_str() {
        "approval" => {
            command
                .payload
                .get("decisionId")
                .and_then(Value::as_str)
                .is_some_and(|id| StableId::parse(id.to_owned()).is_ok())
                && super::service::approval_control::parse_approval_resolution(&command.payload)
                    .is_ok()
        }
        "start" => {
            command
                .payload
                .get("workflowId")
                .and_then(Value::as_str)
                .is_some_and(|workflow_id| StableId::parse(workflow_id.to_owned()).is_ok())
                && matches!(
                    command.payload.get("projectId"),
                    None | Some(Value::Null) | Some(Value::String(_))
                )
        }
        "enqueue" => {
            command.payload.get("workflowId").is_none()
                && command.payload.get("projectId").is_none()
        }
        _ => false,
    };
    if record.schema_version != 1
        || !is_sha256(&record.frozen_context_hash)
        || !is_sha256(&record.command_hash)
        || command.schema_version != 1
        || StableId::parse(command.command_id.clone()).is_err()
        || !action_shape_is_valid
        || (command.action != "approval"
            && input
                .map(|value| value.len() > MAXIMUM_USER_INPUT_BYTES || value.contains('\0'))
                .unwrap_or(true))
        || !attachments_are_valid
    {
        return Err("stored pending Chat command failed integrity validation".into());
    }
    if serde_json::to_vec(record)
        .map_err(|_| "stored pending Chat command cannot be encoded".to_owned())?
        .len()
        > MAXIMUM_PENDING_COMMAND_BYTES
    {
        return Err("stored pending Chat command exceeds its size bound".into());
    }
    Ok(())
}

const fn default_run_deadline_millis() -> u64 {
    60_000
}

pub(crate) fn canonical_hash(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_jcs::to_vec(value)
        .map_err(|error| format!("cannot canonicalize frozen Chat context: {error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn message_from_event(event: Event, role: &str) -> Result<ConversationMessage, String> {
    let body = event
        .payload
        .get("body")
        .and_then(Value::as_str)
        .ok_or("Stored Chat message has no text body")?;
    let images = super::images::command_images(&event.payload)?;
    if role != "user" && !images.is_empty() {
        return Err("Stored assistant message contains user attachments".into());
    }
    Ok(ConversationMessage {
        role: role.into(),
        content: body.into(),
        images,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use tempfile::TempDir;

    struct SwitchableEventPort {
        fail: AtomicBool,
        delivered: Mutex<Vec<CoreEventEnvelope>>,
    }

    impl CommittedChatEventPort for SwitchableEventPort {
        fn publish(&self, event: CoreEventEnvelope) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("listener unavailable".into());
            }
            self.delivered.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[test]
    fn pending_start_accepts_the_standard_agent_workflow() {
        let record = PendingChatCommandV1 {
            schema_version: 1,
            frozen_context_hash: format!("sha256:{}", "a".repeat(64)),
            command_hash: format!("sha256:{}", "b".repeat(64)),
            command: UiCommandInput {
                schema_version: 1,
                command_id: "chat.standard-agent-start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: None,
                payload: json!({
                    "workflowId": "workflow.standard-agent",
                    "projectId": "project.aworkit",
                    "input": "Can you see my project?",
                    "attachments": [],
                }),
            },
        };

        validate_pending_command_record(&record).expect("valid Standard Agent start command");
    }

    #[test]
    fn pending_start_rejects_an_invalid_workflow_identifier() {
        let record = PendingChatCommandV1 {
            schema_version: 1,
            frozen_context_hash: format!("sha256:{}", "a".repeat(64)),
            command_hash: format!("sha256:{}", "b".repeat(64)),
            command: UiCommandInput {
                schema_version: 1,
                command_id: "chat.invalid-workflow-start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: None,
                payload: json!({
                    "workflowId": "workflow with spaces",
                    "input": "hello",
                    "attachments": [],
                }),
            },
        };

        assert_eq!(
            validate_pending_command_record(&record).unwrap_err(),
            "stored pending Chat command failed integrity validation"
        );
    }

    #[test]
    fn committed_delivery_retries_from_the_transactional_outbox() {
        let root = TempDir::new().unwrap();
        let port = Arc::new(SwitchableEventPort {
            fail: AtomicBool::new(true),
            delivered: Mutex::new(Vec::new()),
        });
        let history = ChatHistory::open_with_committed_events(root.path(), port.clone()).unwrap();
        history
            .commit(vec![SemanticEventDraft::new(
                "span.started",
                json!({
                    "requestId":"request.outbox",
                    "runId":"run.outbox",
                    "spanId":"span.run.outbox",
                    "parentSpanId":Value::Null,
                    "spanKind":"run",
                    "semanticRole":"run",
                }),
            )])
            .unwrap();
        assert!(port.delivered.lock().unwrap().is_empty());
        assert_eq!(history.store.pending_outbox(10).unwrap().len(), 1);

        port.fail.store(false, Ordering::SeqCst);
        history.drain_committed_outbox().unwrap();
        assert_eq!(port.delivered.lock().unwrap().len(), 1);
        assert!(history.store.pending_outbox(10).unwrap().is_empty());
    }

    #[test]
    fn span_validation_rejects_parent_termination_with_an_open_child() {
        let history = vec![
            Event {
                event_id: "event.chat.1".into(),
                kind: "span.started".into(),
                payload: json!({"spanId":"span.parent","parentSpanId":Value::Null}),
            },
            Event {
                event_id: "event.chat.2".into(),
                kind: "span.started".into(),
                payload: json!({"spanId":"span.child","parentSpanId":"span.parent"}),
            },
        ];
        let error = validate_span_drafts(
            &history,
            &[SemanticEventDraft::new(
                "span.completed",
                json!({"spanId":"span.parent"}),
            )],
        )
        .unwrap_err();
        assert!(error.contains("child 'span.child' is open"));
    }
}

fn evidence(events: &[Event]) -> Vec<EvidenceRecordDto> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "message.assistant" | "execution.failed" | "tool.completed" | "tool.failed"
            )
        })
        .map(|event| EvidenceRecordDto {
            id: format!("evidence.{}", event.event_id),
            category: match event.kind.as_str() {
                "message.assistant" => "usage",
                "tool.completed" | "tool.failed" => "provenance",
                _ => "error",
            }
            .into(),
            label: match event.kind.as_str() {
                "message.assistant" => "Authority-checked provider completion",
                "tool.completed" => "Authority-settled project file tool",
                "tool.failed" => "Denied or failed project file tool",
                _ => "Authority pipeline failure",
            }
            .into(),
            state: "available".into(),
            value: json!({
                "model": event.payload.get("model").cloned().unwrap_or(Value::Null),
                "providerId": event.payload.get("providerId").cloned().unwrap_or(Value::Null),
                "modelId": event.payload.get("modelId").cloned().unwrap_or(Value::Null),
                "modelTierId": event.payload.get("modelTierId").cloned().unwrap_or(Value::Null),
                "frozenContextHash": event.payload.get("frozenContextHash").cloned().unwrap_or(Value::Null),
                "inputUnits": event.payload.get("inputUnits").cloned().unwrap_or(Value::Null),
                "outputUnits": event.payload.get("outputUnits").cloned().unwrap_or(Value::Null),
                "status": event.payload.get("status").cloned().unwrap_or(Value::Null),
                "snapshotId": event.payload.get("snapshotId").cloned().unwrap_or(Value::Null),
                "snapshotHash": event.payload.get("snapshotHash").cloned().unwrap_or(Value::Null),
                "authorityManifestId": event.payload.get("authorityManifestId").cloned().unwrap_or(Value::Null),
                "invocationId": event.payload.get("invocationId").cloned().unwrap_or(Value::Null),
                "outcomeHash": event.payload.get("outcomeHash").cloned().unwrap_or(Value::Null),
                "automaticReplayAllowed": event.payload.get("automaticReplayAllowed").cloned().unwrap_or(Value::Null),
                "callId": event.payload.get("callId").cloned().unwrap_or(Value::Null),
                "capabilityId": event.payload.get("capabilityId").cloned().unwrap_or(Value::Null),
                "path": event.payload.get("path").cloned().unwrap_or(Value::Null),
                "frozenToolHash": event.payload.get("frozenToolHash").cloned().unwrap_or(Value::Null),
                "workspaceIdentityHash": event.payload.get("workspaceIdentityHash").cloned().unwrap_or(Value::Null),
            }),
        })
        .collect()
}

fn compact_title(body: &str) -> String {
    let title = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = title.chars();
    let compact: String = characters.by_ref().take(48).collect();
    if characters.next().is_some() {
        format!("{compact}…")
    } else if compact.is_empty() {
        "New Chat".into()
    } else {
        compact
    }
}
