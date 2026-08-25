use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{McpServerManifestV1, ModelToolDefinitionV1};
use aworkit_local_store::{CommitBatch, CommitOutcome, Deduplication, Event, LocalHistoryStore};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::dto::{
    ChatProjectionDto, EvidenceRecordDto, RuntimeSnapshot, TimelineItemDto, UiCommandInput,
    UiCommandReceipt,
};
use super::project_scope::{FrozenProjectScopeV1, validate_frozen_project_scope};
use super::settings_v2::{
    BuiltInToolConfigurationV2, ModelConfigurationV2, ModelTierConfigurationV2,
    ProviderConfigurationV2,
};

pub(crate) const CHAT_ID: &str = "chat.local";
const BRANCH_ID: &str = "main";
const SESSION_AGGREGATE_ID: &str = "chat.frozen-sessions";
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
    #[serde(default = "default_agent_maximum_turns")]
    pub agent_maximum_turns: u32,
    #[serde(default)]
    pub maximum_tool_calls: u64,
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
}

pub(crate) struct ChatHistory {
    store: LocalHistoryStore,
}

impl ChatHistory {
    pub(crate) fn open(data_root: &Path) -> Result<Self, String> {
        let store = LocalHistoryStore::open(data_root.join("history").join("aworkit.sqlite3"))
            .map_err(|error| format!("cannot open desktop Chat history: {error}"))?;
        Ok(Self { store })
    }

    pub(crate) fn head(&self) -> Result<u64, String> {
        u64::try_from(self.events()?.len())
            .map_err(|_| "Chat history sequence is exhausted".to_owned())
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
        let events = facts
            .into_iter()
            .enumerate()
            .map(|(offset, (kind, payload))| {
                let sequence =
                    expected_head + u64::try_from(offset).expect("bounded event batch") + 1;
                Event {
                    event_id: format!("event.chat.{sequence}"),
                    kind: kind.to_owned(),
                    payload: receipt_payload(payload, command_id, command_hash, result_head),
                }
            })
            .collect();
        let outcome = self
            .store
            .commit(&CommitBatch {
                chat_id: CHAT_ID.into(),
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
                outbox: Vec::new(),
            })
            .map_err(|error| format!("cannot commit desktop Chat history: {error}"))?;
        let durable_head = match outcome {
            CommitOutcome::Committed(receipt) | CommitOutcome::Existing(receipt) => {
                receipt.head_sequence
            }
        };
        Ok(UiCommandReceipt {
            command_id: command_id.to_owned(),
            accepted: true,
            current_version: durable_head,
            reason: None,
            credential_mutation: None,
        })
    }

    pub(crate) fn conversation(&self) -> Result<Vec<ConversationMessage>, String> {
        let events = current_chat_events(self.events()?);
        Ok(events
            .into_iter()
            .filter_map(|event| match event.kind.as_str() {
                "message.user" => message_from_event(event, "user"),
                "message.assistant" => message_from_event(event, "assistant"),
                _ => None,
            })
            .collect())
    }

    pub(crate) fn current_chat_identity(&self) -> Result<Option<ChatIdentityV1>, String> {
        let current = current_chat_events(self.events()?);
        current
            .iter()
            .rev()
            .find(|event| matches!(event.kind.as_str(), "chat.created" | "chat.started"))
            .map(identity_from_event)
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn ensure_accepts_follow_up(&self) -> Result<(), String> {
        let current = current_chat_events(self.events()?);
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
        let current = current_chat_events(self.events()?);
        if !current.iter().any(|event| event.kind == "chat.started") {
            return Err("cannot cancel a draft Chat".into());
        }
        if current.iter().any(|event| event.kind == "chat.cancelled") {
            return Err("the current Chat is already cancelled".into());
        }
        if current.iter().any(|event| event.kind == "execution.failed") {
            return Err("the current Chat already failed and cannot be cancelled".into());
        }
        Ok(())
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
            let record: FrozenChatExecutionRecordV1 = serde_json::from_value(value)
                .map_err(|_| "stored frozen Chat context is invalid".to_owned())?;
            validate_frozen_context_record(&record)?;
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
        for event in self.session_events()?.into_iter().rev() {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored frozen Chat context is incomplete".into());
            };
            let record: FrozenChatExecutionRecordV1 = serde_json::from_value(value)
                .map_err(|_| "stored frozen Chat context is invalid".to_owned())?;
            validate_frozen_context_record(&record)?;
            if record.context.history_base_head == history_head {
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
        for event in self.session_events()?.into_iter().rev() {
            if event.kind != "chat.effect-command-staged" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored pending Chat command is incomplete".into());
            };
            let record: PendingChatCommandV1 = serde_json::from_value(value)
                .map_err(|_| "stored pending Chat command is invalid".to_owned())?;
            validate_pending_command_record(&record)?;
            if record.command.expected_version == history_head {
                return Ok(Some(record));
            }
        }
        let Some(context) = self.pending_context_at_head(history_head)? else {
            return Ok(None);
        };
        Ok(context
            .context
            .pending_start_command
            .clone()
            .map(|command| PendingChatCommandV1 {
                schema_version: 1,
                frozen_context_hash: context.context_hash,
                command_hash: context.context.start_command_hash,
                command,
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
        if let Some(existing) =
            self.pending_effect_command_at_head(record.command.expected_version)?
        {
            return if existing == record {
                Ok(existing)
            } else {
                Err(
                    "another effect-bearing Chat command is already pending at this history fence"
                        .into(),
                )
            };
        }
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
        validate_frozen_context_record(&record)?;
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

    pub(crate) fn snapshot(&self, after_sequence: u64) -> Result<RuntimeSnapshot, String> {
        let all_events = self.events()?;
        let head = u64::try_from(all_events.len())
            .map_err(|_| "Chat history sequence is exhausted".to_owned())?;
        let current = current_chat_events(all_events.clone());
        let identity = current
            .iter()
            .rev()
            .find(|event| matches!(event.kind.as_str(), "chat.created" | "chat.started"))
            .map(identity_from_event)
            .transpose()?
            .flatten();
        let frozen = identity
            .as_ref()
            .map(|identity| self.frozen_context(&identity.chat_id))
            .transpose()?
            .flatten();
        let timeline = timeline(&current);
        let evidence = evidence(&current);
        let has_exchange = current
            .iter()
            .any(|event| event.kind == "message.assistant");
        let started = current.iter().any(|event| event.kind == "chat.started");
        let failed = current.iter().any(|event| event.kind == "execution.failed");
        let cancelled = current.iter().any(|event| event.kind == "chat.cancelled");
        let open_approvals = current
            .iter()
            .filter(|event| event.kind == "approval.requested")
            .filter(|event| {
                let decision_id = event
                    .payload
                    .get("decisionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                !current.iter().any(|resolved| {
                    resolved.kind == "approval.resolved"
                        && resolved.payload.get("decisionId").and_then(Value::as_str)
                            == Some(decision_id)
                })
            })
            .count();
        let title = timeline
            .iter()
            .find(|item| item.title == "You")
            .map(|item| compact_title(&item.body))
            .unwrap_or_else(|| "New Chat".into());
        let phase = if cancelled {
            "cancelled"
        } else if failed {
            "failed"
        } else if open_approvals > 0 {
            "awaiting_approval"
        } else if has_exchange {
            "waiting_input"
        } else {
            "draft"
        };
        let events = all_events
            .into_iter()
            .enumerate()
            .filter_map(|(offset, event)| {
                let sequence = u64::try_from(offset).ok()?.checked_add(1)?;
                (sequence > after_sequence).then(|| {
                    json!({
                        "sequence": sequence,
                        "eventId": event.event_id,
                        "kind": event.kind,
                        "payload": event.payload,
                    })
                })
            })
            .collect();
        Ok(RuntimeSnapshot {
            version: head,
            last_sequence: head,
            chat: ChatProjectionDto {
                chat_id: identity
                    .as_ref()
                    .map_or_else(|| CHAT_ID.into(), |identity| identity.chat_id.to_string()),
                run_id: identity.as_ref().map_or_else(
                    || {
                        if started {
                            "run.legacy".into()
                        } else {
                            "run.draft".into()
                        }
                    },
                    |identity| identity.run_id.to_string(),
                ),
                title,
                scope: frozen
                    .as_ref()
                    .and_then(|record| record.context.project.as_ref())
                    .map_or_else(
                        || "No project".into(),
                        |project| project.project_name.clone(),
                    ),
                workflow_name: frozen
                    .as_ref()
                    .map(|record| record.context.workflow_name.clone())
                    .or_else(|| started.then(|| "Simple Chat".into())),
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
            },
            projects: Vec::new(),
            timeline,
            evidence,
            events,
        })
    }

    fn events(&self) -> Result<Vec<Event>, String> {
        self.store
            .events(CHAT_ID, BRANCH_ID)
            .map_err(|error| format!("cannot read desktop Chat history: {error}"))
    }

    fn session_events(&self) -> Result<Vec<Event>, String> {
        self.store
            .events(SESSION_AGGREGATE_ID, BRANCH_ID)
            .map_err(|error| format!("cannot read frozen Chat contexts: {error}"))
    }
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

fn current_chat_events(events: Vec<Event>) -> Vec<Event> {
    let start = events
        .iter()
        .rposition(|event| event.kind == "chat.created")
        .unwrap_or(0);
    if events
        .get(start)
        .is_some_and(|event| event.kind == "chat.created")
    {
        events.into_iter().skip(start).collect()
    } else {
        events
    }
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

fn validate_frozen_context_record(record: &FrozenChatExecutionRecordV1) -> Result<(), String> {
    let context = &record.context;
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
        || canonical_hash(context)? != record.context_hash
    {
        return Err("stored frozen Chat context failed integrity validation".into());
    }
    if let Some(command) = &context.pending_start_command {
        let input = command
            .payload
            .get("input")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let attachments_are_empty = match command.payload.get("attachments") {
            None => true,
            Some(Value::Array(values)) => values.is_empty(),
            Some(_) => false,
        };
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
            || !attachments_are_empty
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
        !tool_ids.insert(tool.tool_id.as_str())
            || tool.tool_id != tool.tool_snapshot.id
            || !tool.tool_snapshot.enabled
            || canonical_hash(&tool.tool_snapshot).ok().as_deref() != Some(&tool.tool_hash)
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
        || context.agent_maximum_turns == 0
        || context.agent_maximum_turns > 12
        || context.maximum_tool_calls > 64
        || (context.tools.is_empty()
            && (context.agent_maximum_turns != 1 || context.maximum_tool_calls != 0))
    {
        return Err("stored frozen Chat Agent tool budget is invalid".into());
    }
    Ok(())
}

fn validate_pending_command_record(record: &PendingChatCommandV1) -> Result<(), String> {
    let command = &record.command;
    let input = command
        .payload
        .get("input")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let attachments_are_empty = match command.payload.get("attachments") {
        None => true,
        Some(Value::Array(values)) => values.is_empty(),
        Some(_) => false,
    };
    let action_shape_is_valid = match command.action.as_str() {
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
        || input
            .map(|value| value.len() > MAXIMUM_USER_INPUT_BYTES || value.contains('\0'))
            .unwrap_or(true)
        || !attachments_are_empty
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

const fn default_agent_maximum_turns() -> u32 {
    1
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

fn message_from_event(event: Event, role: &str) -> Option<ConversationMessage> {
    event
        .payload
        .get("body")
        .and_then(Value::as_str)
        .map(|body| ConversationMessage {
            role: role.into(),
            content: body.into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

fn timeline(events: &[Event]) -> Vec<TimelineItemDto> {
    events
        .iter()
        .filter_map(|event| {
            let (title, kind, status, action) = match event.kind.as_str() {
                "message.user" => ("You", "message", "completed", None),
                "message.assistant" => ("Aworkit", "message", "completed", None),
                "tool.completed" => ("Project file tool", "tool", "completed", None),
                "tool.failed" => ("Project file tool", "tool", "failed", None),
                "approval.requested" => ("Approval required", "approval", "pending", Some("approve")),
                "approval.resolved" => ("Approval resolved", "approval", "completed", None),
                "node.completed" => {
                    let node_type = event
                        .payload
                        .get("nodeType")
                        .and_then(Value::as_str)
                        .unwrap_or("node");
                    (
                        event
                            .payload
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or("Workflow node"),
                        match node_type {
                            "agent" | "model_call" => "model",
                            "tool" => "tool",
                            "condition" => "route",
                            "approval" => "approval",
                            "input" | "output" | "wait" | "completion" | "parallel" => "route",
                            _ => "unknown",
                        },
                        "completed",
                        None,
                    )
                }
                "node.failed" => (
                    event
                        .payload
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("Workflow node"),
                    "error",
                    "failed",
                    None,
                ),
                "node.waiting" => (
                    event
                        .payload
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("Workflow node"),
                    "approval",
                    "pending",
                    None,
                ),
                "execution.failed" => {
                    let title = if event.payload.get("status").and_then(Value::as_str)
                        == Some("outcome_uncertain")
                    {
                        "Provider outcome uncertain"
                    } else {
                        "Execution failed"
                    };
                    (title, "error", "failed", None)
                }
                _ => return None,
            };
            let body = event.payload.get("body")?.as_str()?.to_owned();
            let created_at = event
                .payload
                .get("createdAt")
                .and_then(Value::as_str)
                .unwrap_or("time-unavailable")
                .to_owned();
            let metadata = if matches!(event.kind.as_str(), "tool.completed" | "tool.failed") {
                event.payload.clone()
            } else if event.kind == "message.assistant" {
                json!({
                    "model": event.payload.get("model").cloned().unwrap_or(Value::Null),
                    "providerId": event.payload.get("providerId").cloned().unwrap_or(Value::Null),
                    "modelId": event.payload.get("modelId").cloned().unwrap_or(Value::Null),
                    "modelTierId": event.payload.get("modelTierId").cloned().unwrap_or(Value::Null),
                    "frozenContextHash": event.payload.get("frozenContextHash").cloned().unwrap_or(Value::Null),
                    "inputUnits": event.payload.get("inputUnits").cloned().unwrap_or(Value::Null),
                    "outputUnits": event.payload.get("outputUnits").cloned().unwrap_or(Value::Null),
                    "snapshotId": event.payload.get("snapshotId").cloned().unwrap_or(Value::Null),
                    "snapshotHash": event.payload.get("snapshotHash").cloned().unwrap_or(Value::Null),
                    "authorityManifestId": event.payload.get("authorityManifestId").cloned().unwrap_or(Value::Null),
                    "invocationId": event.payload.get("invocationId").cloned().unwrap_or(Value::Null),
                    "outcomeHash": event.payload.get("outcomeHash").cloned().unwrap_or(Value::Null),
                    "replayed": event.payload.get("replayed").cloned().unwrap_or(Value::Bool(false)),
                })
            } else if matches!(
                event.kind.as_str(),
                "execution.failed"
                    | "approval.requested"
                    | "approval.resolved"
                    | "node.completed"
                    | "node.failed"
                    | "node.waiting"
            ) {
                event.payload.clone()
            } else {
                json!({"commandId": event.payload.get("commandId").cloned().unwrap_or(Value::Null)})
            };
            Some(TimelineItemDto {
                id: event.event_id.clone(),
                kind: kind.into(),
                title: title.into(),
                body,
                created_at,
                status: Some(status.into()),
                action: action.map(str::to_owned),
                metadata,
            })
        })
        .collect()
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
