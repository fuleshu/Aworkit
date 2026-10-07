use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{McpServerManifestV1, ModelToolDefinitionV1};
use aworkit_local_store::{
    CommitBatch, CommitOutcome, Deduplication, Event, LocalHistoryStore, OutboxEntry,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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

mod feed;
mod questions;
mod stream_cache;
use stream_cache::{DecodedStream, StreamCache, StreamCaches};

pub(crate) const CHAT_ID: &str = "chat.local";
const BRANCH_ID: &str = "main";
const SESSION_AGGREGATE_ID: &str = "chat.frozen-sessions";
const COMMITTED_EVENT_DESTINATION: &str = "chat.semantic.committed.v1";
// The local history adapter caps the entire serialized commit at 1 MiB. Leave
// ample headroom for the event, deduplication, and backend envelope.
/// Stored Chat records keep the shared runaway guard; they are not a limit on
/// what a user may type or paste. A user input is bounded only by the guard and
/// by what the model can accept.
const MAXIMUM_FROZEN_CONTEXT_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_PENDING_COMMAND_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_USER_INPUT_BYTES: usize = 32 * 1024 * 1024;

/// The committed facts that settle the effect-bearing command which produced
/// them, read from the Chat stream. A suspension the user has to answer is one
/// of them: an approval request and a question both park the Run on a durable
/// decision, so the command is waiting for the user rather than interrupted.
/// Omitting one of these kinds reports a live wait as an abandoned effect and
/// invites the user to replay or abandon a Run that is still working.
const SETTLING_COMMAND_FACT_KINDS: [&str; 7] = [
    "message.assistant",
    "context.manual-completed",
    "context.manual-failed",
    "approval.requested",
    "question.asked",
    "execution.failed",
    "chat.turn_stopped",
];

/// The exact durable Chat facts a snapshot's reducer folds: identity, title,
/// phase, evidence and the sidebar row. A snapshot reads only these kinds, so
/// the model-facing payloads (megabyte context checkpoints, span content and
/// deltas) are never decoded to project Chat state. The list is derived from
/// `projected_phase`, `sidebar_summary` and `evidence`; a fact those folds can
/// observe but this list omits would silently disappear from every snapshot.
const SNAPSHOT_PROJECTION_KINDS: [&str; 16] = [
    "chat.started",
    "chat.cancelled",
    "message.user",
    "message.assistant",
    "approval.requested",
    "approval.resolved",
    "chat.turn_stopped",
    "context.manual-completed",
    "context.manual-failed",
    "context.compaction-ended",
    "execution.failed",
    "tool.completed",
    "tool.failed",
    "question.asked",
    "question.answered",
    "question.cancelled",
];

/// Whether one committed Chat fact settles the effect-bearing command it names.
/// The kind list and the command identity are the whole rule, so both staged
/// records and the recovery fallback read the same predicate.
fn settles_staged_command(event: &Event, command_id: &str) -> bool {
    SETTLING_COMMAND_FACT_KINDS.contains(&event.kind.as_str())
        && event
            .payload
            .get("settlesCommandId")
            .or_else(|| event.payload.get("commandId"))
            .and_then(Value::as_str)
            == Some(command_id)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatIdentityV1 {
    pub chat_id: StableId,
    pub run_id: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenCredentialBindingV1 {
    pub credential_ref: StableId,
    pub field_names: BTreeSet<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenChatExecutionContextV1 {
    /// Absent in legacy Chats: preserve their original provider authority hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_version: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_target: Option<super::compaction::FrozenSummaryTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_configurations: Vec<FrozenMcpConfigurationV1>,
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
    /// Absent in legacy Chats; never retrofit workspace authority into old snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_workspace: Option<aworkit_trusted_core::WorkspaceBindingV1>,
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
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenChatExecutionRecordV1 {
    pub context: FrozenChatExecutionContextV1,
    pub context_hash: String,
}

/// The frozen-record schema version this build writes and validates with its
/// current rules.
pub(crate) const FROZEN_RECORD_SCHEMA_VERSION: u16 = 1;

/// How this build may use one stored frozen Chat record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoredFrozenRecordAdmission {
    /// Written at the version this build's rules describe.
    Current,
    /// Written by a newer build. Its stored bytes are still verified, and the
    /// Chat stays readable, but this build runs none of it and rewrites none of
    /// it: refusing the read instead would make the Chat unrunnable *and*
    /// unreadable, which is the outage this gate removes.
    ReadOnlyNewer { version: u16 },
}

/// Selects the rule set for one stored frozen-record version.
///
/// The version is never merely asserted: it chooses which rules run. A known
/// older version is lifted to the current shape by `migrate_stored_frozen_record`
/// and then validated as current; a version above this build's opens read-only
/// with a notice; a version below the oldest one this project ever wrote has no
/// migration path and is damage rather than evidence.
pub(crate) fn frozen_record_admission(version: u16) -> Result<StoredFrozenRecordAdmission, String> {
    match version {
        FROZEN_RECORD_SCHEMA_VERSION => Ok(StoredFrozenRecordAdmission::Current),
        version if FROZEN_RECORD_MIGRATIONS.contains(&version) => {
            Ok(StoredFrozenRecordAdmission::Current)
        }
        version if version < FROZEN_RECORD_SCHEMA_VERSION => Err(format!(
            "stored frozen Chat context schema version {version} has no migration path in this build"
        )),
        version => Ok(StoredFrozenRecordAdmission::ReadOnlyNewer { version }),
    }
}

/// Every older frozen-record version this build can still lift to the current
/// shape. Adding one means adding its number here and its step below.
///
/// Every frozen record this project has written is version 1, so there is no
/// older shape to lift yet: the table is empty on purpose, and the only older
/// version a corrupt or truncated record can name is refused with its own name.
const FROZEN_RECORD_MIGRATIONS: &[u16] = &[];

/// Lifts one stored record written at a known older version to the current
/// shape, before the current rules run over it.
///
/// This is the one place a version bump adds its migration, and admission routes
/// every older version through it, so a future bump cannot ship as an assertion
/// that silently rejects the profile it just upgraded.
fn migrate_stored_frozen_record(version: u16, value: Value) -> Result<Value, String> {
    if version == FROZEN_RECORD_SCHEMA_VERSION {
        return Ok(value);
    }
    if !FROZEN_RECORD_MIGRATIONS.contains(&version) {
        return Err(format!(
            "stored frozen Chat context schema version {version} has no migration path in this build"
        ));
    }
    // One arm per listed version, until there are none.
    Ok(value)
}

/// The one sentence a Chat this build may only read opens with.
pub(crate) fn newer_frozen_record_notice(version: u16) -> String {
    format!(
        "This Chat was written by a newer Aworkit (frozen record schema version {version}); it opens read-only in this build. Update Aworkit to continue this Chat."
    )
}

impl FrozenChatExecutionContextV1 {
    /// `None` when this build can execute the frozen Chat; `Some(notice)` when a
    /// newer build wrote it and this one may only read it.
    pub(crate) fn read_only_notice(&self) -> Option<String> {
        match frozen_record_admission(self.schema_version) {
            Ok(StoredFrozenRecordAdmission::Current) => None,
            Ok(StoredFrozenRecordAdmission::ReadOnlyNewer { version }) => {
                Some(newer_frozen_record_notice(version))
            }
            Err(error) => Some(error),
        }
    }
}

/// Secret-free MCP connection and exact credential metadata for reconnection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrozenMcpConfigurationV1 {
    pub server: super::settings_v2::McpServerConfigurationV2,
    /// Only opaque references, field names and revisions; never secret values.
    #[serde(
        default,
        rename = "opaqueBindings",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub credentials: Vec<super::settings_v2::CredentialMetadataConfigurationV2>,
}

/// Exact effect-bearing Chat command durably staged before the authority
/// pipeline is entered. It gives restart recovery the original idempotency ID,
/// expected history fence, and input for both first and follow-up messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    bound_identity: Option<ChatIdentityV1>,
    metadata_lock: Arc<Mutex<()>>,
    committed_events: Arc<dyn CommittedChatEventPort>,
    /// Validated view of the selected Chat's stream.
    ///
    /// Both span validation and decoded snapshots advance incrementally. Readers
    /// retain immutable snapshots whose payloads are shared across appends.
    stream: Arc<Mutex<StreamCache>>,
    streams: Arc<StreamCaches>,
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
            bound_identity: None,
            metadata_lock: Arc::new(Mutex::new(())),
            committed_events,
            stream: Arc::new(Mutex::new(StreamCache::default())),
            streams: Arc::new(StreamCaches::default()),
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

    /// Execution uses an immutable stream binding, independent of shell selection.
    pub(crate) fn for_chat(&self, chat_id: &str) -> Result<Self, String> {
        Ok(Self {
            bound_identity: Some(self.identity(chat_id)?),
            stream: self.streams.get(chat_id),
            ..self.clone()
        })
    }

    /// Read queries share the incrementally maintained decoded stream. Binding the
    /// identity avoids reloading navigation metadata for every event page.
    pub(crate) fn for_chat_query(&self, chat_id: &str) -> Result<Self, String> {
        Ok(Self {
            bound_identity: Some(self.identity(chat_id)?),
            stream: self.streams.get(chat_id),
            ..self.clone()
        })
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
        for event in self.events()?.iter() {
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

    pub(crate) fn replay_navigation(
        &self,
        command_id: &str,
        command_hash: &str,
    ) -> Result<Option<UiCommandReceipt>, String> {
        history_index::replay(&self.store, command_id, command_hash)
    }

    /// Commits the terminal facts of one Chat command under its own
    /// deduplication identity, so a retried command returns its first receipt
    /// instead of committing twice.
    pub(crate) fn append(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        facts: Vec<(&str, Value)>,
    ) -> Result<UiCommandReceipt, String> {
        self.append_as(
            "desktop.command",
            command_id,
            command_hash,
            expected_head,
            facts,
        )
    }

    /// Commits the facts that suspend a command at an approval, question or wait
    /// gate under a distinct identity.
    ///
    /// A command that suspends commits twice: once when it suspends and once
    /// when it finally settles. Sharing one deduplication key made the settle
    /// reuse the suspension's key with a different request hash, which the store
    /// refuses - so an approval-gated run committed its graph and then could
    /// never deliver its answer.
    pub(crate) fn append_suspension(
        &self,
        command_id: &str,
        command_hash: &str,
        expected_head: u64,
        facts: Vec<(&str, Value)>,
    ) -> Result<UiCommandReceipt, String> {
        self.append_as(
            "desktop.command.pending",
            command_id,
            command_hash,
            expected_head,
            facts,
        )
    }

    /// One commit under an explicit deduplication key type.
    fn append_as(
        &self,
        key_type: &str,
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
                    key_type: key_type.into(),
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
            .iter()
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
        // A delivered record is a copy of an event the store already holds, so the
        // queue is trimmed as it drains. Retention is a bound, not a promise: a
        // queue that cannot be trimmed keeps its records and stays usable.
        let _ = self
            .store
            .purge_delivered_outbox(super::history_retention::OUTBOX_RETAINED_V1);
        Ok(())
    }

    pub(crate) fn conversation(&self) -> Result<Vec<ConversationMessage>, String> {
        let events = self.events()?;
        events
            .iter()
            .filter_map(|event| match event.kind.as_str() {
                "message.user" => Some(message_from_event(event.as_ref().clone(), "user")),
                "message.assistant" => {
                    Some(message_from_event(event.as_ref().clone(), "assistant"))
                }
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
        // A failed or uncertain turn is terminal for that invocation, not for the
        // Chat. Refusing every later input would dead-end a Chat whose context is
        // still intact and still owes the user a next step, so a new message
        // simply starts the next turn under its own command identity.
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
            // The Chat identity is read from the stored bytes, so an unrelated
            // record this build cannot read is never decoded on this path.
            if stored_record_chat_id(&value) != Some(chat_id.as_str()) {
                continue;
            }
            let record = decode_stored_frozen_context_record(value)?;
            return Ok(Some(record));
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
            // Select from the stored bytes before decoding: only the selected
            // Chat's record at this head is read, and an unrelated unreadable
            // record cannot fail the read.
            if stored_record_chat_id(&value) != Some(selected_chat_id.as_str())
                || value
                    .get("context")
                    .and_then(|context| context.get("historyBaseHead"))
                    .and_then(Value::as_u64)
                    != Some(history_head)
            {
                continue;
            }
            let record = decode_stored_frozen_context_record(value)?;
            return Ok(Some(record));
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
        let chat_events = self
            .store
            .events_of_kinds(
                selected_chat_id.as_str(),
                BRANCH_ID,
                &SETTLING_COMMAND_FACT_KINDS,
            )
            .map_err(|e| e.to_string())?;
        // Decode the profile-level session aggregate exactly once. The former
        // nested lookup reopened and revalidated every frozen context for each
        // staged command, making every history selection quadratic in the
        // number of past commands and contexts.
        let session_events = self.session_events()?;
        let mut contexts = Vec::new();
        let mut contexts_by_hash = BTreeMap::new();
        let mut unrelated_context_hashes = BTreeSet::new();
        for event in &session_events {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored frozen Chat context is incomplete".into());
            };
            // Another Chat's record is remembered by hash only, so an
            // unreadable record this Chat does not use is never decoded.
            if stored_record_chat_id(&value) != Some(selected_chat_id.as_str()) {
                if let Some(hash) = stored_record_context_hash(&value) {
                    unrelated_context_hashes.insert(hash.to_owned());
                }
                continue;
            }
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
                // A staged command for another Chat: its context was filtered
                // out, and missing evidence for this Chat stays a failure.
                if unrelated_context_hashes.contains(&record.frozen_context_hash) {
                    continue;
                }
                return Err("stored pending Chat command has no frozen context".into());
            };
            if context.context.identity.chat_id != selected_chat_id {
                continue;
            }
            let settled = chat_events
                .iter()
                .any(|event| settles_staged_command(event, record.command.command_id.as_str()));
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
                let settled = chat_events
                    .iter()
                    .any(|event| settles_staged_command(event, command.command_id.as_str()));
                (!settled).then(|| PendingChatCommandV1 {
                    schema_version: 1,
                    frozen_context_hash: context.context_hash,
                    command_hash: context.context.start_command_hash,
                    command,
                })
            }))
    }

    /// Stages an exact effect-bearing command on the separate session
    /// aggregate before any provider effect. Retrying the same record is a
    /// durable no-op; reusing its command ID with different content is denied.
    pub(crate) fn stage_effect_command(
        &self,
        record: PendingChatCommandV1,
    ) -> Result<PendingChatCommandV1, String> {
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
        validate_pending_command_record(&record)?;
        for event in self.session_events()? {
            if event.kind != "chat.effect-command-staged" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                return Err("stored pending Chat command is incomplete".into());
            };
            // Only a record naming this exact command can collide with it, so an
            // unrelated unreadable staged command never has to decode.
            if value.pointer("/command/commandId").and_then(Value::as_str)
                != Some(record.command.command_id.as_str())
            {
                continue;
            }
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
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
        let context_hash = canonical_hash(&context)?;
        let record = FrozenChatExecutionRecordV1 {
            context,
            context_hash,
        };
        // The write path does not validate the record it just built. Every part
        // of it was validated where it was produced (the catalog, the frozen
        // tool bindings, the frozen project scope), and every reader verifies
        // the stored bytes. Re-validating here could only reject an artifact
        // this build itself wrote, from a schema a later build may extend.
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

    /// Compatibility-test seam: appends one already-encoded session-aggregate
    /// event, exactly as another build wrote it, so a test can replay stored
    /// records this build did not write.
    #[cfg(test)]
    pub(crate) fn stage_stored_session_event_for_test(
        &self,
        kind: &str,
        payload: Value,
    ) -> Result<(), String> {
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
        let existing_events = self.session_events()?;
        let expected_head = u64::try_from(existing_events.len())
            .map_err(|_| "frozen Chat context sequence is exhausted".to_owned())?;
        self.store
            .commit(&CommitBatch {
                chat_id: SESSION_AGGREGATE_ID.into(),
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: vec![Event {
                    event_id: format!("event.test.stored.{expected_head}"),
                    kind: kind.to_owned(),
                    payload,
                }],
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: Vec::new(),
            })
            .map_err(|error| format!("cannot stage stored session event: {error}"))?;
        Ok(())
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

    /// Rebuilds the database file so pages retention freed return to the OS.
    ///
    /// Returns the file size before and after, so the caller reports what was
    /// actually reclaimed. A vacuum rewrites the whole file, so it is only ever
    /// reached through an explicit reclaim.
    pub(crate) fn vacuum_store(&self) -> Result<(u64, u64), String> {
        self.store.vacuum().map_err(|error| error.to_string())
    }

    /// The bytes the history database and its write-ahead log occupy.
    pub(crate) fn file_bytes(&self) -> (u64, u64) {
        self.store.file_bytes()
    }

    /// The stored payload, grouped by event kind, largest kind first.
    pub(crate) fn payload_breakdown(&self) -> Result<Vec<(String, u64, u64)>, String> {
        self.store
            .payload_breakdown()
            .map_err(|error| error.to_string())
    }

    /// The payload bytes one Chat's events still hold.
    pub(crate) fn stream_payload_bytes(&self, chat_id: &str) -> Result<u64, String> {
        self.store
            .stream_payload_bytes(chat_id)
            .map_err(|error| error.to_string())
    }

    /// Rows, delivered rows and payload bytes the delivery queue holds.
    pub(crate) fn outbox_bytes(&self) -> Result<(u64, u64, u64), String> {
        self.store.outbox_bytes().map_err(|error| error.to_string())
    }

    /// The Chats the user deleted whose events are still stored.
    ///
    /// They are the only events retention removes rather than releases: the user
    /// already removed the Chat, and the index keeps its tombstone, so nothing
    /// disappears from the sidebar.
    pub(crate) fn deleted_chat_ids(&self) -> Result<Vec<String>, String> {
        Ok(self
            .index()?
            .entries
            .into_iter()
            .filter(|entry| entry.deleted)
            .map(|entry| entry.chat_id.to_string())
            .collect())
    }

    /// Reclaims store space from superseded snapshots and already-deleted Chats.
    ///
    /// The policy lives in [`super::history_retention`]: a Chat keeps the newest
    /// snapshot of every context scope and its newest turns, so it can still be
    /// continued from where it ended; the conversation, tool records, usage and
    /// timing are never a candidate. A Chat the user already deleted keeps its
    /// index tombstone and loses only its events, so nothing disappears without
    /// the user having asked for it.
    ///
    /// A pass rewrites payloads and a caller follows it with a vacuum, so it is
    /// deliberately explicit: it never runs at startup or on the interactive
    /// path.
    pub(crate) fn reclaim_space(
        &self,
        retained_turns: usize,
        pruned_at: &str,
        progress: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<super::history_retention::ReclaimReportV1, String> {
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable".to_owned())?;
        let deleted = self
            .deleted_chat_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut report = super::history_retention::ReclaimReportV1::default();
        let streams = self.store.stream_ids().map_err(|error| error.to_string())?;
        let total = u64::try_from(streams.len()).unwrap_or(u64::MAX);
        progress(super::history_retention::RECLAIM_PHASE_RELEASING, 0, total);
        for (position, chat_id) in streams.into_iter().enumerate() {
            progress(
                super::history_retention::RECLAIM_PHASE_RELEASING,
                u64::try_from(position).unwrap_or(u64::MAX),
                total,
            );
            if deleted.contains(&chat_id) {
                report.events_removed += self
                    .store
                    .delete_stream(&chat_id)
                    .map_err(|error| error.to_string())?;
                report.chats_purged += 1;
                continue;
            }
            let candidates = self
                .store
                .candidate_events(
                    &chat_id,
                    BRANCH_ID,
                    &super::history_retention::PRUNABLE_KINDS,
                )
                .map_err(|error| error.to_string())?;
            report.streams_scanned += 1;
            let borrowed = candidates
                .iter()
                .map(|event| super::history_retention::RetentionEventV1 {
                    event_id: &event.event_id,
                    sequence: event.sequence,
                    kind: &event.kind,
                    payload: &event.payload,
                })
                .collect::<Vec<_>>();
            let plan = super::history_retention::plan(&borrowed, retained_turns, pruned_at)?;
            if plan.is_empty() {
                continue;
            }
            let updates = plan
                .iter()
                .map(|pruned| (pruned.event_id.clone(), pruned.payload.clone()))
                .collect::<Vec<_>>();
            for pruned in &plan {
                report.payload_bytes_released += pruned
                    .payload
                    .get(super::history_retention::PRUNED_PAYLOAD_FIELD)
                    .and_then(|marker| marker.get("bytes"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            report.payloads_pruned += self
                .store
                .prune_payloads(&updates)
                .map_err(|error| error.to_string())?;
        }
        // The delivery queue's already-delivered records are the other half of
        // the duplication: each one is a copy of an event the store still holds.
        let (outbox_rows, outbox_bytes) = self
            .store
            .purge_delivered_outbox(super::history_retention::OUTBOX_RETAINED_V1)
            .map_err(|error| error.to_string())?;
        report.outbox_rows_removed = outbox_rows;
        report.outbox_bytes_released = outbox_bytes;
        Ok(report)
    }

    fn copy_legacy_chat(&self, chat_id: &StableId, events: &[Event]) -> Result<(), String> {
        let existing = self.events_for_chat(chat_id)?;
        // A released payload is not a divergence. The legacy stream and each Chat
        // migrated out of it are separate streams with separate retention windows,
        // so one side can hold a tombstone where the other still holds the bytes it
        // released. The kind still has to match, and a difference that is not a
        // release is still the partially migrated history this check exists to
        // catch — refusing the profile instead of silently rebuilding it.
        let diverged = existing.iter().zip(events).any(|(left, right)| {
            left.kind != right.kind
                || (left.payload != right.payload
                    && !super::history_retention::is_pruned(&left.payload)
                    && !super::history_retention::is_pruned(&right.payload))
        });
        if existing.len() > events.len() || diverged {
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
        if let Some(identity) = &self.bound_identity {
            return Ok(identity.clone());
        }
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

    /// Starts a new Chat.
    ///
    /// An unmaterialized draft is not a stored Chat: it holds no canonical
    /// facts and stays out of the sidebar until its first input. Pressing New
    /// Chat again therefore reuses the newest draft and tombstones any older
    /// leftovers instead of storing another empty Chat every time.
    pub(crate) fn create_chat(
        &self,
        command_id: &str,
        command_hash: &str,
        _expected_head: u64,
    ) -> Result<UiCommandReceipt, String> {
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
        let index = self.index()?;
        let mut drafts = Vec::new();
        for entry in &index.entries {
            if !entry.deleted && self.head_for_chat(&entry.chat_id)? == 0 {
                drafts.push(entry.clone());
            }
        }
        if let Some(draft) = drafts.pop() {
            let mut facts = drafts
                .iter()
                .map(|entry| {
                    (
                        "history.chat-deleted",
                        json!({"chatId": entry.chat_id, "createdAt": now_label()}),
                    )
                })
                .collect::<Vec<_>>();
            facts.push((
                "history.chat-selected",
                json!({"chatId": draft.chat_id, "createdAt": now_label()}),
            ));
            return history_index::append_command(&self.store, command_id, command_hash, 0, facts);
        }
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
        _expected_head: u64,
        chat_id: &str,
    ) -> Result<UiCommandReceipt, String> {
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
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
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
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
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
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
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
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

    /// Published sidebar rows.
    ///
    /// A Chat whose canonical stream is still empty is an unmaterialized draft
    /// placeholder for the open New Chat, not a stored Chat, so it is left out
    /// until its first input materializes it. Only deliberate deletion and the
    /// absence of content hide a row here.
    fn history_entries(
        &self,
        index: HistoryIndexState,
    ) -> Result<Vec<ChatHistoryEntryDto>, String> {
        let mut entries = Vec::new();
        for entry in index.entries {
            if entry.deleted {
                continue;
            }
            if self.head_for_chat(&entry.chat_id)? == 0 {
                continue;
            }
            entries.push(Self::history_entry(entry));
        }
        entries.sort_by(|left, right| {
            sortable_time(&right.updated_at)
                .cmp(&sortable_time(&left.updated_at))
                .then_with(|| right.chat_id.cmp(&left.chat_id))
        });
        Ok(entries)
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
        let _metadata = self
            .metadata_lock
            .lock()
            .map_err(|_| "Chat metadata lock unavailable")?;
        let mut index = self.index()?;
        if let Some(identity) = &self.bound_identity {
            index.selected_chat_id = identity.chat_id.clone();
        }
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
        let head = self.head_for_chat(&identity.chat_id)?;
        // The projection state is a bounded, kind-filtered read, never a decode of
        // the retained stream. An interactive snapshot of a huge Chat (megabyte
        // context checkpoints and span payloads) used to decode the entire stream
        // per call, pinning a core for minutes; task #185.
        let current: Arc<Vec<Arc<Event>>> = Arc::new(
            self.store
                .events_of_kinds(
                    identity.chat_id.as_str(),
                    BRANCH_ID,
                    &SNAPSHOT_PROJECTION_KINDS,
                )
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(Arc::new)
                .collect(),
        );
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
            // The reducer knows only the durable Chat events; the host publishes
            // the remembered New Chat selections in `snapshot_history`.
            remembered_workflow_id: None,
            remembered_project_id: None,
        };
        let state_hash = canonical_hash(&json!({
            "throughSequence": head,
            "reducerVersion": "chat.semantic.reducer.v1",
            "chat": &chat,
            "evidence": &evidence,
        }))?;
        let mut summary = sidebar_summary(&current, frozen.as_ref(), &indexed.created_at);
        summary.head_sequence = head;
        // The row timestamp is a store scalar read, not a payload decode, and both
        // the metadata and the delta snapshot resolve it the same way. Deriving it
        // from the decoded set instead made the two paths disagree by the newest
        // non-projection fact and rewrote the sidebar index on every poll.
        summary.updated_at = self
            .store
            .latest_event_time(identity.chat_id.as_str(), BRANCH_ID)
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| indexed.created_at.clone());
        if indexed.summary.as_ref() != Some(&summary) {
            history_index::append_summaries(
                &self.store,
                vec![(identity.chat_id.clone(), summary.clone())],
            )?;
            index.entries[indexed_position].summary = Some(summary);
        }
        let history = self.history_entries(index)?;
        // Only the events the caller has not seen, read from the store in bounded
        // windows: work stays proportional to the unseen tail instead of to the
        // whole retained Chat, and the metadata snapshot still returns no events.
        let events = if after_sequence == u64::MAX {
            Vec::new()
        } else {
            self.snapshot_delta_events(&identity.chat_id, after_sequence, head)?
        };
        Ok(RuntimeSnapshot {
            event_window: None,
            active_chat_ids: Vec::new(),
            context_model: frozen.as_ref().map(|record| super::dto::ContextModelDto {
                name: record.context.model_name.clone(),
                context_window: record.context.model_snapshot.context_window,
            }),
            version: head,
            through_sequence: head,
            reducer_version: "chat.semantic.reducer.v1".into(),
            state_hash,
            chat,
            history,
            projects: Vec::new(),
            evidence,
            events,
            subagents: Vec::new(),
        })
    }

    /// The committed events after `after`, in sequence order, read from the store
    /// in bounded windows.
    ///
    /// This is the interactive delta path: a caller that already holds everything
    /// through its last seen sequence reads only the tail, and a caller far behind
    /// still never materializes the whole stream at once. The store's own window
    /// bound (128 rows, 4 MiB of payload) is what keeps each read finite; the loop
    /// only advances a cursor, and the first row of every page is always returned,
    /// so it always progresses toward `head`.
    fn snapshot_delta_events(
        &self,
        chat_id: &StableId,
        after: u64,
        head: u64,
    ) -> Result<Vec<CoreEventEnvelope>, String> {
        let chat = chat_id.as_str();
        let mut events = Vec::new();
        let mut cursor = after.min(head);
        while cursor < head {
            let page = self
                .store
                .event_window(chat, BRANCH_ID, cursor, head, false)
                .map_err(|error| error.to_string())?;
            let Some((last, _)) = page.last() else { break };
            let last = *last;
            for (sequence, event) in page {
                events.push(envelope(
                    chat,
                    BRANCH_ID,
                    sequence,
                    SemanticEventDraft::new(event.kind, event.payload),
                ));
            }
            cursor = last;
        }
        Ok(events)
    }

    fn events(&self) -> Result<Arc<Vec<Arc<Event>>>, String> {
        let chat_id = self.selected_identity()?.chat_id;
        let mut cache = self.lock_stream();
        let head = self.head_for_chat(&chat_id)?;
        self.refresh_spans(&mut cache, &chat_id, head)?;
        let decoded = self.decoded_stream(&mut cache, &chat_id, head)?;
        Ok(Arc::clone(&decoded.events))
    }

    pub(crate) fn events_for_chat(&self, chat_id: &StableId) -> Result<Vec<Event>, String> {
        self.store
            .events(chat_id.as_str(), BRANCH_ID)
            .map_err(|error| format!("cannot read desktop Chat history: {error}"))
    }

    fn lock_stream(&self) -> std::sync::MutexGuard<'_, StreamCache> {
        self.stream
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Reconciles external commits from the previous durable head. A different
    /// Chat or truncated stream starts a new recovery view.
    fn refresh_spans(
        &self,
        cache: &mut StreamCache,
        chat_id: &StableId,
        head: u64,
    ) -> Result<(), String> {
        if cache.chat_id.as_deref() == Some(chat_id.as_str()) && cache.head == head {
            return Ok(());
        }
        cache.synchronize(&self.store, chat_id.as_str(), head)
    }

    /// Decoded events through `head`, extending the retained snapshot as needed.
    fn decoded_stream<'a>(
        &self,
        cache: &'a mut StreamCache,
        chat_id: &StableId,
        head: u64,
    ) -> Result<&'a mut DecodedStream, String> {
        cache.synchronize(&self.store, chat_id.as_str(), head)?;
        Ok(&mut cache.decoded)
    }

    /// Shared committed envelopes for read-only scans that must not deep-clone.
    pub(crate) fn committed_events_shared(
        &self,
    ) -> Result<super::semantic_events::SharedEvents, String> {
        let chat_id = self.selected_identity()?.chat_id;
        let mut cache = self.lock_stream();
        let head = self.head_for_chat(&chat_id)?;
        self.refresh_spans(&mut cache, &chat_id, head)?;
        let decoded = self.decoded_stream(&mut cache, &chat_id, head)?;
        if decoded.envelopes.is_none() {
            let stream_id = chat_id.as_str().to_owned();
            let built = decoded
                .events
                .iter()
                .enumerate()
                .map(|(offset, event)| {
                    let sequence = u64::try_from(offset)
                        .expect("bounded history offset")
                        .saturating_add(1);
                    Arc::new(envelope(
                        &stream_id,
                        BRANCH_ID,
                        sequence,
                        SemanticEventDraft::new(event.kind.clone(), event.payload.clone()),
                    ))
                })
                .collect::<Vec<_>>();
            decoded.envelopes = Some(Arc::new(built));
        }
        Ok(Arc::clone(
            decoded.envelopes.as_ref().expect("committed envelopes"),
        ))
    }

    fn session_events(&self) -> Result<Vec<Event>, String> {
        self.store
            .events(SESSION_AGGREGATE_ID, BRANCH_ID)
            .map_err(|error| format!("cannot read frozen Chat contexts: {error}"))
    }

    /// Decodes frozen contexts once while upgrading legacy sidebar summaries.
    ///
    /// This repair path stays exhaustive, but a record this build cannot read
    /// degrades to "no frozen context" for that Chat instead of failing the
    /// upgrade and with it application open. Ordinary snapshots resolve only the
    /// selected Chat's frozen context.
    fn frozen_contexts(&self) -> Result<BTreeMap<String, FrozenChatExecutionRecordV1>, String> {
        let mut contexts = BTreeMap::new();
        for event in self.session_events()? {
            if event.kind != "chat.execution-context-frozen" {
                continue;
            }
            let Some(value) = event.payload.get("record").cloned() else {
                continue;
            };
            let Ok(record) = decode_stored_frozen_context_record(value) else {
                continue;
            };
            contexts.insert(record.context.identity.chat_id.to_string(), record);
        }
        Ok(contexts)
    }
}

/// The Chat identity of a stored frozen record, read from the stored bytes
/// before that record is decoded or validated. `None` means this build cannot
/// tell which Chat the record belongs to, so it is never selected.
fn stored_record_chat_id(value: &Value) -> Option<&str> {
    value
        .get("context")?
        .get("identity")?
        .get("chatId")?
        .as_str()
}

/// The context hash named by a stored frozen record, read before decoding.
fn stored_record_context_hash(value: &Value) -> Option<&str> {
    value.get("contextHash").and_then(Value::as_str)
}

impl SemanticEventCommitter for ChatHistory {
    fn commit(&self, drafts: Vec<SemanticEventDraft>) -> Result<Vec<CoreEventEnvelope>, String> {
        if drafts.is_empty() {
            return Ok(Vec::new());
        }
        let chat_id = self.selected_identity()?.chat_id;
        let committed = {
            let mut cache = self.lock_stream();
            let expected_head = self.head_for_chat(&chat_id)?;
            match self.commit_locked(&mut cache, &chat_id, expected_head, drafts) {
                Ok(committed) => committed,
                Err(error) => {
                    // A rejected draft can leave the ledger half-applied, so the
                    // whole view is rebuilt from durable history next time.
                    *cache = StreamCache::default();
                    return Err(error);
                }
            }
        };
        self.drain_committed_outbox()?;
        self.release_displaced_payloads(&chat_id, &committed);
        Ok(committed)
    }

    fn committed_events(&self) -> Result<Vec<CoreEventEnvelope>, String> {
        Ok(self
            .committed_events_shared()?
            .iter()
            .map(|e| e.as_ref().clone())
            .collect())
    }

    fn committed_events_shared(&self) -> Result<super::semantic_events::SharedEvents, String> {
        ChatHistory::committed_events_shared(self)
    }
}

impl ChatHistory {
    /// Releases the payloads an append just displaced from the retained window.
    ///
    /// This is the write path's half of
    /// [`super::history_retention`]: a Chat that keeps working keeps its store
    /// bounded, one displaced snapshot at a time, instead of waiting for the
    /// explicit reclaim. Only the windows an append actually moved are examined,
    /// the scan walks the warm view newest first, and at most
    /// [`super::history_retention::MAX_RELEASES_PER_APPEND_V1`] payloads are
    /// released, so an append never digests a whole history.
    ///
    /// Retention is a bound, not a promise: a Chat whose bytes cannot be released
    /// keeps them and stays usable, so a failure here is never reported to the
    /// caller and can never end a Run
    /// (`aworkit.workflow_worker.failure_policy`).
    fn release_displaced_payloads(&self, chat_id: &StableId, committed: &[CoreEventEnvelope]) {
        use super::history_retention;
        let triggering = history_retention::triggering_kinds(
            committed
                .iter()
                .map(|event| (event.kind.as_str(), &event.payload)),
        );
        if triggering.is_empty() {
            return;
        }
        let mut cache = self.lock_stream();
        if cache.chat_id.as_deref() != Some(chat_id.as_str()) {
            return;
        }
        // The scan decides a window from what it can see, and it is only allowed to
        // see a complete view: a view that is missing its newest events could
        // mistake an old payload for the newest of its scope.
        if u64::try_from(cache.decoded.events.len()).ok() != Some(cache.head) {
            return;
        }
        let planned = {
            let borrowed = cache
                .decoded
                .events
                .iter()
                .enumerate()
                .map(|(position, event)| history_retention::RetentionEventV1 {
                    event_id: &event.event_id,
                    sequence: u64::try_from(position).unwrap_or(u64::MAX),
                    kind: &event.kind,
                    payload: &event.payload,
                })
                .collect::<Vec<_>>();
            history_retention::plan_displaced(
                &borrowed,
                &triggering,
                history_retention::RETAINED_TURNS_V1,
                &now_label(),
                history_retention::MAX_RELEASES_PER_APPEND_V1,
            )
        };
        let Ok(planned) = planned else {
            return;
        };
        if planned.is_empty() {
            return;
        }
        let released = planned
            .into_iter()
            .map(|pruned| (pruned.event_id, pruned.payload))
            .collect::<Vec<_>>();
        if self.store.prune_payloads(&released).is_err() {
            return;
        }
        cache.release_payloads(&released);
    }

    /// Validates and persists one semantic batch against the carried ledger.
    ///
    /// The ledger and shared snapshots advance only after durable commit; no
    /// successful append invalidates already decoded historical payloads.
    fn commit_locked(
        &self,
        cache: &mut StreamCache,
        chat_id: &StableId,
        expected_head: u64,
        drafts: Vec<SemanticEventDraft>,
    ) -> Result<Vec<CoreEventEnvelope>, String> {
        let stream_id = chat_id.as_str().to_owned();
        let event_count =
            u64::try_from(drafts.len()).map_err(|_| "bounded semantic batch".to_owned())?;
        self.refresh_spans(cache, chat_id, expected_head)?;
        apply_span_drafts(&mut cache.spans, &drafts)?;
        let committed = committed_envelopes(&stream_id, expected_head, &drafts);
        let local = local_events(&stream_id, expected_head, &drafts);
        let outcome = self
            .store
            .commit(&CommitBatch {
                chat_id: stream_id,
                branch_id: BRANCH_ID.into(),
                expected_head,
                events: local,
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: delivery_outbox(&committed)?,
            })
            .map_err(|error| format!("cannot commit semantic Chat events: {error}"))?;
        if matches!(outcome, CommitOutcome::Existing(_)) {
            return Err("semantic event commit unexpectedly resolved as an existing batch".into());
        }
        cache.append_committed(&committed, expected_head.saturating_add(event_count));
        Ok(committed)
    }
}

#[derive(Default)]
struct SpanLedgerState {
    started: BTreeSet<String>,
    terminal: BTreeSet<String>,
    parents: BTreeMap<String, String>,
}

fn validate_span_drafts(
    history: &[impl std::borrow::Borrow<Event>],
    drafts: &[SemanticEventDraft],
) -> Result<(), String> {
    let mut state = SpanLedgerState::default();
    for event in history.iter().map(std::borrow::Borrow::borrow) {
        observe_existing_span(&mut state, &event.kind, &event.payload);
    }
    apply_span_drafts(&mut state, drafts)
}

/// Validates drafts against the current ledger and advances it on success.
fn apply_span_drafts(
    state: &mut SpanLedgerState,
    drafts: &[SemanticEventDraft],
) -> Result<(), String> {
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
    let digest = super::digest::digest_bytes(format!("aworkit-chat-run-v1:{seed}").as_bytes());
    let suffix = &digest[..40];
    Ok(ChatIdentityV1 {
        chat_id: StableId::parse(format!("chat.{suffix}")).map_err(|error| error.to_string())?,
        run_id: StableId::parse(format!("run.{suffix}")).map_err(|error| error.to_string())?,
    })
}

/// Optional provider accounting a message fact may carry. A missing figure is
/// omitted from the payload rather than serialized as null, so a consumer can
/// tell "not reported" from a reported zero.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MessageUsageV1<'a> {
    pub model: Option<&'a str>,
    pub input_units: Option<u64>,
    pub output_units: Option<u64>,
    /// Whole-Run cached input tokens, when the Run reported an aggregate.
    pub cached_input_units: Option<u64>,
    /// Whole-Run uncached input tokens, when the Run reported an aggregate.
    pub uncached_input_units: Option<u64>,
}

pub(crate) fn message_fact(body: &str, created_at: &str, usage: MessageUsageV1<'_>) -> Value {
    let mut value = json!({
        "schemaVersion": 1,
        "body": body,
        "createdAt": created_at,
    });
    if let Some(object) = value.as_object_mut() {
        if let Some(model) = usage.model {
            object.insert("model".into(), Value::String(model.to_owned()));
        }
        if let Some(units) = usage.input_units {
            object.insert("inputUnits".into(), Value::from(units));
        }
        if let Some(units) = usage.output_units {
            object.insert("outputUnits".into(), Value::from(units));
        }
        if let Some(units) = usage.cached_input_units {
            object.insert("cachedInputUnits".into(), Value::from(units));
        }
        if let Some(units) = usage.uncached_input_units {
            object.insert("uncachedInputUnits".into(), Value::from(units));
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

fn projected_phase(events: &[impl std::borrow::Borrow<Event>]) -> &'static str {
    let events: Vec<&Event> = events.iter().map(std::borrow::Borrow::borrow).collect();
    if events.iter().any(|event| event.kind == "chat.cancelled") {
        return "cancelled";
    }
    // A failed or uncertain attempt is evidence about one turn, never a terminal
    // state of the Chat. Like the reference harness, the attempt stays in the log
    // for inspection while the Chat remains open for the next turn; only an
    // explicit cancellation stops it.
    // A question is the other way one Run waits for one user decision. It uses
    // the same open/resolved shape as an approval, so the phase is derived the
    // same way and an answered question leaves the waiting phase.
    let has_open_question = !questions::open_questions(events.iter().copied()).is_empty();
    let has_open_approval = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == "approval.requested")
        .any(|(index, event)| {
            let decision_id = event
                .payload
                .get("decisionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !events[index + 1..].iter().any(|resolved| {
                (resolved.kind == "approval.resolved"
                    && resolved.payload.get("decisionId").and_then(Value::as_str)
                        == Some(decision_id))
                    || questions::ends_questions(&resolved.kind)
            })
        });
    if has_open_question {
        "awaiting_answer"
    } else if has_open_approval {
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
    events: &[impl std::borrow::Borrow<Event>],
    frozen: Option<&FrozenChatExecutionRecordV1>,
    created_at: &str,
) -> ChatSummaryProjection {
    let title = events
        .iter()
        .map(std::borrow::Borrow::borrow)
        .find(|event| event.kind == "message.user")
        .and_then(|event| event.payload.get("body"))
        .and_then(Value::as_str)
        .map(compact_title)
        .unwrap_or_else(|| "New Chat".into());
    let updated_at = events
        .iter()
        .map(std::borrow::Borrow::borrow)
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

/// Decodes a durable context while verifying every hash against the exact
/// stored JSON. This keeps additive serde defaults backward compatible without
/// ever treating the re-serialized, default-expanded value as the original
/// bytes.
///
/// The record's schema version selects the rules that run over it: the current
/// version gets the full structural validation, a known older version is first
/// lifted by `migrate_stored_frozen_record`, and a newer version this build does
/// not know is verified and returned for reading only instead of failing the
/// read. Tamper detection is byte-exact in every case.
fn decode_stored_frozen_context_record(
    value: Value,
) -> Result<FrozenChatExecutionRecordV1, String> {
    let stored_context = value
        .get("context")
        .cloned()
        .ok_or_else(|| "stored frozen Chat context is incomplete".to_owned())?;
    let version = stored_context
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .and_then(|version| u16::try_from(version).ok())
        .ok_or_else(|| "stored frozen Chat context has no supported schema version".to_owned())?;
    let admission = frozen_record_admission(version)?;
    let value = match admission {
        StoredFrozenRecordAdmission::Current => migrate_stored_frozen_record(version, value)?,
        // A newer version is never rewritten, only verified and read.
        StoredFrozenRecordAdmission::ReadOnlyNewer { .. } => value,
    };
    let record: FrozenChatExecutionRecordV1 = serde_json::from_value(value)
        .map_err(|_| "stored frozen Chat context is invalid".to_owned())?;
    match admission {
        StoredFrozenRecordAdmission::Current => {
            validate_frozen_context_record(&record, Some(&stored_context))?;
        }
        StoredFrozenRecordAdmission::ReadOnlyNewer { .. } => {
            validate_readable_newer_frozen_context_record(&record, &stored_context)?;
        }
    }
    Ok(record)
}

/// Verifies a record written by a newer build as far as this build safely can:
/// the stored context hash still describes the stored bytes, and the record is
/// still a bounded, self-consistent piece of evidence. The version-1 structural
/// rules deliberately do not run, because a newer schema may legitimately hold
/// values this build's rules did not anticipate; running them would refuse the
/// upgrade's own records.
///
/// This never weakens tamper detection: every byte of the stored context is
/// covered by the hash compared here.
fn validate_readable_newer_frozen_context_record(
    record: &FrozenChatExecutionRecordV1,
    stored_context: &Value,
) -> Result<(), String> {
    if !is_sha256(&record.context_hash)
        || !hash_is_current(Some(stored_context), &record.context, &record.context_hash)?
    {
        return Err("stored frozen Chat context failed integrity validation".into());
    }
    if serde_json::to_vec(record)
        .map_err(|_| "stored frozen Chat context cannot be encoded".to_owned())?
        .len()
        > MAXIMUM_FROZEN_CONTEXT_BYTES
    {
        return Err("stored frozen Chat context exceeds its size bound".into());
    }
    Ok(())
}

/// Whether one stored hash still describes the value it names.
///
/// When the caller holds the stored JSON, the comparison runs over those exact
/// bytes (Rule A): a record a different build wrote with a field this one does
/// not know still verifies, and any change to a stored value still fails. The
/// write path holds no stored bytes and hashes the value it just built.
pub(crate) fn hash_is_current(
    stored: Option<&Value>,
    value: &impl Serialize,
    expected: &str,
) -> Result<bool, String> {
    let hash = match stored {
        Some(stored) => canonical_hash(stored)?,
        None => canonical_hash(value)?,
    };
    Ok(hash == expected)
}

/// One named sub-value of a stored record, when the caller holds stored bytes.
///
/// Every name here is a field serde always reads from stored JSON, so a missing
/// one is a damaged record rather than an absent optional field. Defaulted
/// fields that may legitimately be absent use `Value::get` directly.
pub(crate) fn stored_field<'a>(
    stored: Option<&'a Value>,
    path: &[&str],
) -> Result<Option<&'a Value>, String> {
    let Some(mut value) = stored else {
        return Ok(None);
    };
    for key in path {
        value = value
            .get(*key)
            .ok_or_else(|| "stored frozen Chat context is incomplete".to_owned())?;
    }
    Ok(Some(value))
}

/// Whether one frozen tool binding is a shape this build can still execute.
///
/// A built-in tool is admitted by identity. A dynamic tool — an MCP function or
/// a ComfyUI capability — is admitted only in the exact shape freeze writes:
/// it must carry its model-facing definition and the frozen configuration its
/// dispatcher reads, so a stored record proves what it will run instead of
/// naming a family this build no longer knows. A new tool family extends this
/// predicate with its own shape.
///
/// Failing this predicate never ends a message: the pass drops the binding and
/// reports a bounded notice, exactly as an unavailable Settings tool does. Only
/// tamper evidence (a stored hash that no longer matches its bytes) fails a
/// read.
pub(crate) fn frozen_tool_binding_is_executable(tool: &FrozenToolBindingV1) -> bool {
    if super::documents::builtin_tool_binding_ids().contains(&tool.tool_id) {
        return true;
    }
    if tool.definition.is_none() {
        return false;
    }
    let configuration = &tool.tool_snapshot.configuration;
    if tool.tool_id.starts_with("mcp://") {
        return configuration.len() == 2 + usize::from(configuration.contains_key("annotations"))
            && configuration.get("annotations").is_none_or(|hints| {
                serde_json::from_value::<aworkit_capability_host::McpToolAnnotationsV1>(
                    hints.clone(),
                )
                .is_ok()
            })
            && configuration.contains_key("serverId")
            && configuration.contains_key("tool");
    }
    if crate::runtime::comfyui::is_comfyui_workflow_capability(&tool.tool_id) {
        // The endpoint, the workflow file and every (node id, input name) pair.
        return configuration.len() == 3
            && ["endpoint", "workflowPath", "parameters"]
                .iter()
                .all(|key| configuration.contains_key(*key));
    }
    if crate::runtime::comfyui::is_comfyui_authoring_capability(&tool.tool_id) {
        // The endpoint, plus the workflow table the reader resolves.
        let expected = 1 + usize::from(
            tool.tool_id == crate::runtime::comfyui::COMFYUI_GET_WORKFLOW_CAPABILITY_ID,
        );
        return configuration.len() == expected && configuration.contains_key("endpoint");
    }
    false
}

fn validate_frozen_context_record(
    record: &FrozenChatExecutionRecordV1,
    stored_context: Option<&Value>,
) -> Result<(), String> {
    let context = &record.context;
    // Every nested hash is verified against the exact stored JSON sub-value
    // whenever stored bytes are available, so a record another build wrote with
    // a field this one does not know still loads and any change to a stored
    // value still fails. The write path has no stored bytes yet and hashes the
    // record it just built.
    let stored_tools = stored_context.and_then(|value| value.get("tools"));
    let stored_project = stored_context.and_then(|value| value.get("project"));
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
        || !hash_is_current(
            stored_field(stored_context, &["workflowSnapshot"])?,
            &context.workflow_snapshot,
            &context.workflow_snapshot_hash,
        )?
        || !hash_is_current(
            stored_field(stored_context, &["modelTierSnapshot"])?,
            &context.model_tier_snapshot,
            &context.model_tier_hash,
        )?
        || !hash_is_current(
            stored_field(stored_context, &["providerSnapshot"])?,
            &context.provider_snapshot,
            &context.provider_hash,
        )?
        || !hash_is_current(
            stored_field(stored_context, &["modelSnapshot"])?,
            &context.model_snapshot,
            &context.model_hash,
        )?
        || !hash_is_current(stored_context, context, &record.context_hash)?
        || context.model_tier_snapshot.id != context.model_tier_id
        || context.provider_snapshot.id != context.provider_id
        || context.provider_snapshot.name != context.provider_name
        || context.provider_snapshot.kind != context.provider_kind
        || context.provider_snapshot.base_url != context.provider_base_url
        || context.model_snapshot.id != context.model_id
        || context.model_snapshot.name != context.model_name
        || context.model_snapshot.remote_id != context.remote_model_id
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
        validate_frozen_project_scope(project, stored_project)?;
    }
    if let Some(workspace) = &context.chat_workspace {
        if context.project.is_some() {
            return Err(
                "stored Chat cannot bind both a project and a private working folder".into(),
            );
        }
        super::chat_workspace::validate_chat_workspace(workspace, &context.identity.chat_id)?;
    }
    validate_frozen_tool_bindings(&context.tools, stored_tools)?;
    if context
        .tools
        .iter()
        .any(|tool| tool.tool_snapshot.requires_project)
        && context.project.is_none()
        && context.chat_workspace.is_none()
        || !(30_000..=3_600_000).contains(&context.run_deadline_millis)
    {
        return Err("stored frozen Chat Agent execution context is invalid".into());
    }
    Ok(())
}

/// Validates the frozen tool set of one stored Chat: unique identity, matching
/// snapshot hash, credential references that agree with the snapshot.
///
/// This is integrity and tamper evidence, and it stays hard: a stored binding
/// whose bytes no longer match its recorded hash, whose identity is duplicated,
/// or whose credential references disagree is rejected byte-exactly.
///
/// Whether this build can still *execute* one binding is a different question
/// with a different outcome. That is `frozen_tool_binding_is_executable`, and
/// the run path answers it by dropping the binding and reporting a notice: a
/// stored record is evidence of what a Chat froze, never a gate that a later
/// build may fail.
///
/// `stored_tools` is the stored tool-bindings array when the caller holds the
/// stored record; each binding's tool hash is then verified against the exact
/// stored snapshot bytes.
pub(crate) fn validate_frozen_tool_bindings(
    tools: &[FrozenToolBindingV1],
    stored_tools: Option<&Value>,
) -> Result<(), String> {
    let mut tool_ids = BTreeSet::new();
    for (index, tool) in tools.iter().enumerate() {
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
        let stored_snapshot = match stored_tools {
            Some(stored_tools) => Some(
                stored_tools
                    .get(index)
                    .and_then(|stored| stored.get("toolSnapshot"))
                    .ok_or_else(|| "stored frozen Chat tool bindings are incomplete".to_owned())?,
            ),
            None => None,
        };
        if !tool_ids.insert(tool.tool_id.as_str())
            || tool.tool_id != tool.tool_snapshot.id
            || !tool.tool_snapshot.enabled
            || !hash_is_current(stored_snapshot, &tool.tool_snapshot, &tool.tool_hash)?
            || frozen_refs != configured_refs
            || tool.credentials.iter().any(|credential| {
                credential.revision == 0
                    || credential.field_names.is_empty()
                    || credential
                        .field_names
                        .iter()
                        .any(|field| field.is_empty() || field.chars().any(char::is_control))
            })
        {
            return Err("stored frozen Chat tool bindings failed integrity validation".into());
        }
    }
    Ok(())
}

fn validate_pending_command_record(record: &PendingChatCommandV1) -> Result<(), String> {
    let command = &record.command;
    let input = super::images::command_text(&command.payload).ok();
    let attachments_are_valid = super::images::command_images(&command.payload).is_ok();
    let action_shape_is_valid = match command.action.as_str() {
        "question" => {
            command.payload["questionId"]
                .as_str()
                .is_some_and(|id| StableId::parse(id.to_owned()).is_ok())
                && super::service::approval_control::parse_question_answer(&command.payload)
                    .is_ok_and(|answer| {
                        answer.cancelled
                            || answer.option_id.is_some()
                            || answer.free_text.is_some()
                            || answer.path.is_some()
                    })
        }
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
        "compact_context" => {
            command.payload["nodeId"]
                .as_str()
                .is_some_and(|id| StableId::parse(id.to_owned()).is_ok())
                && command.payload["baseSequence"].as_u64().is_some()
        }
        _ => false,
    };
    if record.schema_version != 1
        || !is_sha256(&record.frozen_context_hash)
        || !is_sha256(&record.command_hash)
        || command.schema_version != 1
        || StableId::parse(command.command_id.clone()).is_err()
        || !action_shape_is_valid
        || (!matches!(
            command.action.as_str(),
            "approval" | "question" | "compact_context"
        ) && input
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

/// The one durable digest rule, with the sentence this artifact already shows.
pub(crate) fn canonical_hash(value: &impl Serialize) -> Result<String, String> {
    super::digest::canonical_hash(value)
        .map_err(|error| format!("cannot canonicalize frozen Chat context: {error}"))
}

fn is_sha256(value: &str) -> bool {
    super::digest::is_sha256(value)
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

    mod frozen_record_compat;
    mod question;

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

    /// Task #185: an interactive snapshot of a huge Chat must not decode the
    /// retained stream. Pre-fix, a concrete `after_sequence` was routed through
    /// the full `events()` decode, so every poll re-read and re-parsed every
    /// megabyte context checkpoint and span payload the Chat had ever committed,
    /// pinning a core for minutes while the pass committed nothing.
    #[test]
    fn interactive_snapshot_reads_only_the_unseen_tail_of_a_huge_chat() {
        use crate::runtime::semantic_events::noop_committed_event_port;

        let root = tempfile::TempDir::new().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let mut drafts: Vec<(&str, Value)> = vec![
            ("chat.started", json!({"createdAt":"1"})),
            ("message.user", json!({"createdAt":"2","body":"hello"})),
        ];
        // Model-facing payloads the Chat projection never needs: four megabyte
        // context checkpoints and one megabyte span input.
        drafts.extend((0..4).map(|turn| {
            (
                "context.checkpoint",
                json!({
                    "createdAt": "3",
                    "turn": turn,
                    "body": "x".repeat(2 * 1024 * 1024),
                }),
            )
        }));
        drafts.push((
            "span.started",
            json!({
                "createdAt": "4",
                "spanId": "span.run.1",
                "input": {"document": "y".repeat(1024 * 1024)},
            }),
        ));
        drafts.push(("message.assistant", json!({"createdAt":"5","body":"done"})));
        let base = history.head().unwrap();
        let events = drafts
            .into_iter()
            .enumerate()
            .map(|(offset, (kind, payload))| Event {
                event_id: event_identity(
                    identity.chat_id.as_str(),
                    BRANCH_ID,
                    base + u64::try_from(offset).unwrap() + 1,
                ),
                kind: kind.to_owned(),
                payload,
            })
            .collect::<Vec<_>>();
        history
            .store
            .commit(&CommitBatch {
                chat_id: identity.chat_id.to_string(),
                branch_id: BRANCH_ID.into(),
                expected_head: base,
                events,
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: Vec::new(),
            })
            .unwrap();
        let head = history.head().unwrap();
        assert_eq!(head, base + 8);

        // The live shape: the desktop polls with the last sequence it already has.
        for _ in 0..4 {
            let snapshot = history.snapshot(head).unwrap();
            assert_eq!(snapshot.through_sequence, head);
            assert!(snapshot.events.is_empty(), "nothing unseen at the head");
        }
        assert!(
            history.lock_stream().decoded.events.is_empty(),
            "an interactive snapshot must not decode the retained stream"
        );

        // Mid-stream the delta is exactly the unseen tail, in committed order.
        let delta = history.snapshot(base + 3).unwrap();
        assert_eq!(
            delta
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            ((base + 4)..=head).collect::<Vec<_>>()
        );
        assert!(history.lock_stream().decoded.events.is_empty());

        // The guard above is real: the exhaustive path does decode every payload,
        // and this stream really is megabytes of it.
        assert_eq!(
            history.committed_events_shared().unwrap().len(),
            usize::try_from(head).unwrap()
        );
        assert!(!history.lock_stream().decoded.events.is_empty());
        let payload_bytes: usize = history
            .events_for_chat(&identity.chat_id)
            .unwrap()
            .iter()
            .map(|event| serde_json::to_string(&event.payload).unwrap().len())
            .sum();
        assert!(
            payload_bytes > 8 * 1024 * 1024,
            "the retained stream must be expensive to decode in full: {payload_bytes} bytes"
        );

        // The metadata and delta snapshots still agree on the projected state, so
        // the cheap projection is the same projection.
        assert_eq!(
            history.snapshot(u64::MAX).unwrap().state_hash,
            history.snapshot(0).unwrap().state_hash
        );

        // A delta longer than one store window still returns every unseen event,
        // once each, in committed order.
        let extra = (0..200u64)
            .map(|n| Event {
                event_id: event_identity(identity.chat_id.as_str(), BRANCH_ID, head + n + 1),
                kind: "message.user".into(),
                payload: json!({"createdAt":"6","body":format!("m{n}")}),
            })
            .collect::<Vec<_>>();
        history
            .store
            .commit(&CommitBatch {
                chat_id: identity.chat_id.to_string(),
                branch_id: BRANCH_ID.into(),
                expected_head: head,
                events: extra,
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: Vec::new(),
            })
            .unwrap();
        let grown = history.head().unwrap();
        assert_eq!(grown, head + 200);
        let windowed = history.snapshot(base + 3).unwrap();
        assert_eq!(
            windowed
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            ((base + 4)..=grown).collect::<Vec<_>>()
        );
    }

    /// Task #186: the legacy migration re-verifies its copy on every open, and a
    /// released payload is not a divergence. The legacy stream `chat.local` and
    /// every Chat migrated out of it are separate streams with separate retention
    /// windows, so one side holds a tombstone where the other still holds the
    /// bytes it released. Comparing the payload alone refused the whole profile
    /// with "partially migrated Chat history differs from its legacy source".
    #[test]
    fn a_released_payload_does_not_refuse_the_legacy_migration() {
        use crate::runtime::semantic_events::noop_committed_event_port;
        use aworkit_local_store::{CommitBatch, Event as StoredEvent, LocalHistoryStore};

        let root = tempfile::TempDir::new().unwrap();
        let legacy_store =
            LocalHistoryStore::open(root.path().join("history").join("aworkit.sqlite3")).unwrap();
        legacy_store
            .commit(&CommitBatch {
                chat_id: CHAT_ID.into(),
                branch_id: BRANCH_ID.into(),
                expected_head: 0,
                events: vec![
                    StoredEvent {
                        event_id: "legacy.event.1".into(),
                        kind: "chat.created".into(),
                        payload: json!({"chatId": "chat.legacy.1", "runId": "run.1"}),
                    },
                    StoredEvent {
                        event_id: "legacy.event.2".into(),
                        kind: "context.checkpoint".into(),
                        payload: json!({
                            "nodeId": "node.1",
                            "ownerKey": "owner.1",
                            "snapshot": {"document": "the legacy snapshot"},
                        }),
                    },
                ],
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: Vec::new(),
            })
            .unwrap();

        // The first open migrates the legacy stream into its own Chat.
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let migrated_chat = history
            .index()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.chat_id.clone())
            .find(|chat_id| chat_id.as_str() != CHAT_ID)
            .expect("the legacy stream was migrated into a Chat");
        assert_eq!(history.events_for_chat(&migrated_chat).unwrap().len(), 2);
        drop(history);

        // Release the legacy stream's own copy, as a retention pass may.
        let legacy = legacy_store
            .events(CHAT_ID, BRANCH_ID)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "context.checkpoint")
            .expect("the legacy checkpoint is stored");
        let released = crate::runtime::history_retention::plan(
            &[crate::runtime::history_retention::RetentionEventV1 {
                event_id: &legacy.event_id,
                sequence: 1,
                kind: &legacy.kind,
                payload: &legacy.payload,
            }],
            0,
            "2026-10-05T00:00:00Z",
        )
        .unwrap();
        assert_eq!(released.len(), 1);
        legacy_store
            .prune_payloads(&[(released[0].event_id.clone(), released[0].payload.clone())])
            .unwrap();

        // Opening again must re-verify the migration and still accept the profile.
        let reopened =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .expect("a released legacy payload does not refuse the profile");
        assert_eq!(
            reopened.events_for_chat(&migrated_chat).unwrap().len(),
            2,
            "a tolerated difference must not re-copy or truncate the migrated Chat"
        );
    }

    /// Task #186: a Chat that keeps working keeps its store bounded. Every
    /// append releases the snapshot it just displaced, so a live Chat never has
    /// to wait for the explicit reclaim — and the window it keeps is untouched.
    #[test]
    fn an_append_releases_the_checkpoint_snapshot_it_displaced() {
        use crate::runtime::history_retention::{
            PRUNABLE_KINDS, PRUNED_PAYLOAD_FIELD, RETAINED_TURNS_V1, is_pruned,
        };
        use crate::runtime::semantic_events::noop_committed_event_port;

        let root = tempfile::TempDir::new().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let appended = RETAINED_TURNS_V1 + 5;
        for turn in 0..appended {
            let mut drafts = vec![SemanticEventDraft::new(
                "context.checkpoint",
                json!({
                    "ownerKey": "owner.one",
                    "nodeId": "node.one",
                    "snapshot": {"turn": turn, "context": "c".repeat(4_096)},
                }),
            )];
            if turn == 0 {
                drafts.push(SemanticEventDraft::new(
                    "message.user",
                    json!({"createdAt": "1", "body": "hello"}),
                ));
            }
            history.commit(drafts).unwrap();
        }

        let candidates = history
            .store
            .candidate_events(identity.chat_id.as_str(), BRANCH_ID, &PRUNABLE_KINDS)
            .unwrap();
        assert_eq!(candidates.len(), appended);
        for (index, candidate) in candidates.iter().enumerate() {
            let released = index < appended - RETAINED_TURNS_V1;
            assert_eq!(
                is_pruned(&candidate.payload),
                released,
                "event {index} of {} must be {}",
                candidates.len(),
                if released { "released" } else { "intact" }
            );
        }

        // The window itself is byte-identical: only the displaced turns changed.
        for candidate in &candidates[appended - RETAINED_TURNS_V1..] {
            assert_eq!(
                candidate.payload["snapshot"]["context"],
                json!("c".repeat(4_096))
            );
        }

        // A released payload states what it dropped, and keeps everything else.
        let released = &candidates[0];
        let marker = &released.payload[PRUNED_PAYLOAD_FIELD];
        assert!(released.payload.get("snapshot").is_none());
        assert_eq!(released.payload["ownerKey"], json!("owner.one"));
        assert_eq!(marker["kind"], json!("context_checkpoint"));
        assert_eq!(marker["retainedTurns"], json!(RETAINED_TURNS_V1));
        assert!(marker["bytes"].as_u64().unwrap() > 4_096);
        assert!(
            marker["digestBefore"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );

        // The conversation is not a candidate and never moved.
        let conversation = history
            .events_for_chat(&identity.chat_id)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "message.user")
            .expect("the user message is still committed");
        assert_eq!(conversation.payload["body"], json!("hello"));
    }

    /// The resumption invariant: the newest snapshot of a scope that runs
    /// alongside another is kept however busy the other scope is. A per-Chat
    /// window would have released it, and the Chat would lose where it was.
    #[test]
    fn a_quiet_scope_keeps_its_newest_snapshot_beside_a_busy_one() {
        use crate::runtime::history_retention::{PRUNABLE_KINDS, RETAINED_TURNS_V1, is_pruned};
        use crate::runtime::semantic_events::noop_committed_event_port;

        let root = tempfile::TempDir::new().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let checkpoint = |owner: &str, node: &str| {
            SemanticEventDraft::new(
                "context.checkpoint",
                json!({
                    "ownerKey": owner,
                    "nodeId": node,
                    "snapshot": {"at": "the only one of this scope"},
                }),
            )
        };
        history
            .commit(vec![checkpoint("owner.quiet", "node.quiet")])
            .unwrap();
        for _ in 0..(RETAINED_TURNS_V1 + 6) {
            history
                .commit(vec![checkpoint("owner.busy", "node.busy")])
                .unwrap();
        }

        let candidates = history
            .store
            .candidate_events(identity.chat_id.as_str(), BRANCH_ID, &PRUNABLE_KINDS)
            .unwrap();
        let quiet = candidates
            .iter()
            .find(|candidate| candidate.payload["ownerKey"] == json!("owner.quiet"))
            .expect("the quiet scope's checkpoint is still stored");
        assert!(
            !is_pruned(&quiet.payload),
            "the newest snapshot of a scope is never released"
        );
        let busy = candidates
            .iter()
            .filter(|candidate| candidate.payload["ownerKey"] == json!("owner.busy"))
            .collect::<Vec<_>>();
        assert_eq!(
            busy.iter().filter(|c| !is_pruned(&c.payload)).count(),
            RETAINED_TURNS_V1
        );
    }

    /// A released payload is not released twice: the warm view moves with the
    /// store, so the next append's scan does not find it intact again.
    #[test]
    fn a_released_payload_keeps_its_tombstone_across_later_appends() {
        use crate::runtime::history_retention::{
            PRUNABLE_KINDS, PRUNED_PAYLOAD_FIELD, RETAINED_TURNS_V1,
        };
        use crate::runtime::semantic_events::noop_committed_event_port;

        let root = tempfile::TempDir::new().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let checkpoint = |turn: usize| {
            SemanticEventDraft::new(
                "context.checkpoint",
                json!({
                    "ownerKey": "owner.one",
                    "nodeId": "node.one",
                    "snapshot": {"turn": turn},
                }),
            )
        };
        for turn in 0..(RETAINED_TURNS_V1 + 1) {
            history.commit(vec![checkpoint(turn)]).unwrap();
        }
        let first = history
            .store
            .candidate_events(identity.chat_id.as_str(), BRANCH_ID, &PRUNABLE_KINDS)
            .unwrap()
            .remove(0);
        let pruned_at = first.payload[PRUNED_PAYLOAD_FIELD]["prunedAt"].clone();

        for turn in 0..3 {
            history
                .commit(vec![checkpoint(RETAINED_TURNS_V1 + 1 + turn)])
                .unwrap();
        }
        let again = history
            .store
            .candidate_events(identity.chat_id.as_str(), BRANCH_ID, &PRUNABLE_KINDS)
            .unwrap()
            .remove(0);
        assert_eq!(again.event_id, first.event_id);
        assert_eq!(
            again.payload[PRUNED_PAYLOAD_FIELD]["prunedAt"], pruned_at,
            "a released payload must not be rewritten by a later append"
        );
    }

    /// The same bound covers the other half of the duplication: the compiled
    /// model-call input a turn stores beside its checkpoint.
    #[test]
    fn an_append_releases_the_model_call_input_it_displaced() {
        use crate::runtime::history_retention::{
            PRUNABLE_KINDS, PRUNED_PAYLOAD_FIELD, RETAINED_TURNS_V1, is_pruned,
        };
        use crate::runtime::semantic_events::noop_committed_event_port;

        let root = tempfile::TempDir::new().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let appended = RETAINED_TURNS_V1 + 4;
        for call in 0..appended {
            history
                .commit(vec![SemanticEventDraft::new(
                    "span.started",
                    json!({
                        "spanId": format!("span.model.{call}"),
                        "spanKind": "model_call",
                        "hasInput": true,
                        "input": {"messages": "m".repeat(4_096)},
                    }),
                )])
                .unwrap();
        }

        let candidates = history
            .store
            .candidate_events(identity.chat_id.as_str(), BRANCH_ID, &PRUNABLE_KINDS)
            .unwrap();
        assert_eq!(candidates.len(), appended);
        let released = &candidates[0];
        assert!(is_pruned(&released.payload));
        assert!(released.payload.get("input").is_none());
        assert_eq!(
            released.payload["hasInput"],
            json!(true),
            "the call did carry a request; the marker is what says its bytes are gone"
        );
        assert_eq!(
            released.payload[PRUNED_PAYLOAD_FIELD]["kind"],
            json!("model_call_input")
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| !is_pruned(&candidate.payload))
                .count(),
            RETAINED_TURNS_V1
        );
    }

    #[test]
    fn frozen_comfyui_bindings_stay_executable_in_a_stored_chat() {
        // A Chat that froze a Settings-derived ComfyUI workflow tool and the two
        // authoring helpers must stay resumable: refusing the family here made
        // every message in that Chat fail with a tool-binding integrity error.
        let binding = |tool_id: &str, configuration: BTreeMap<String, Value>| {
            let snapshot = BuiltInToolConfigurationV2 {
                options: Default::default(),
                id: tool_id.to_owned(),
                name: "Frozen capability".into(),
                enabled: true,
                requires_project: false,
                credential_bindings: Vec::new(),
                configuration,
            };
            FrozenToolBindingV1 {
                tool_id: tool_id.to_owned(),
                tool_hash: canonical_hash(&snapshot).unwrap(),
                tool_snapshot: snapshot,
                credentials: Vec::new(),
                definition: Some(ModelToolDefinitionV1 {
                    capability_id: tool_id.to_owned(),
                    name: "frozen_capability".into(),
                    description: "Frozen for this Chat".into(),
                    input_schema: json!({"type": "object"}),
                }),
            }
        };
        let endpoint = || ("endpoint".to_owned(), json!("http://127.0.0.1:8188/"));

        let tools = vec![
            binding(
                "comfyui.krea-2-turbo-text-to-image-upscaled",
                BTreeMap::from([
                    endpoint(),
                    ("workflowPath".to_owned(), json!("D:/workflows/krea.json")),
                    ("parameters".to_owned(), json!([])),
                ]),
            ),
            binding("comfyui.list_node_types", BTreeMap::from([endpoint()])),
            binding(
                "comfyui.get_workflow",
                BTreeMap::from([endpoint(), ("workflows".to_owned(), json!({}))]),
            ),
        ];
        validate_frozen_tool_bindings(&tools, None)
            .expect("a Chat that froze ComfyUI capabilities must stay resumable");

        // A shape this build never writes still loads as evidence, and the pass
        // drops it with a notice instead of failing the read: only tamper
        // evidence (the hash check below) fails a stored record.
        let wrong_shape = vec![binding(
            "comfyui.krea-2-turbo-text-to-image-upscaled",
            BTreeMap::from([endpoint()]),
        )];
        validate_frozen_tool_bindings(&wrong_shape, None)
            .expect("an unexecutable shape is evidence, not corruption");
        assert!(!frozen_tool_binding_is_executable(&wrong_shape[0]));
        let mut undefined = binding("comfyui.list_node_types", BTreeMap::from([endpoint()]));
        undefined.definition = None;
        validate_frozen_tool_bindings(&[undefined.clone()], None)
            .expect("a binding without a definition is evidence, not corruption");
        assert!(!frozen_tool_binding_is_executable(&undefined));
        assert!(frozen_tool_binding_is_executable(&tools[1]));

        // Tampering with the snapshot hash is still caught.
        let mut tampered = tools;
        tampered[0].tool_hash = format!("sha256:{}", "b".repeat(64));
        assert!(validate_frozen_tool_bindings(&tampered, None).is_err());
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
    fn stream_cache_carries_the_span_ledger_and_observes_a_foreign_writer() {
        let root = TempDir::new().unwrap();
        let port = Arc::new(SwitchableEventPort {
            fail: AtomicBool::new(false),
            delivered: Mutex::new(Vec::new()),
        });
        let first = ChatHistory::open_with_committed_events(root.path(), port.clone()).unwrap();
        let run_start = SemanticEventDraft::new(
            "span.started",
            json!({
                "requestId":"request.cache",
                "runId":"run.cache",
                "spanId":"span.run.cache",
                "parentSpanId":Value::Null,
                "spanKind":"run",
                "semanticRole":"run",
            }),
        );
        let model_start = SemanticEventDraft::new(
            "span.started",
            json!({
                "requestId":"request.cache",
                "runId":"run.cache",
                "spanId":"span.model.cache",
                "parentSpanId":"span.run.cache",
                "spanKind":"model_call",
                "semanticRole":"model_call",
            }),
        );
        first
            .commit(vec![run_start.clone(), model_start.clone()])
            .unwrap();
        assert_eq!(first.events().unwrap().len(), 2);
        // A later chunk validates against the carried ledger: the decodable
        // stream is rebuilt, but history is not re-read for every commit.
        first
            .commit(vec![SemanticEventDraft::new(
                "span.content_delta",
                json!({"spanId":"span.model.cache","channel":"assistant_output","append":"hello"}),
            )])
            .unwrap();
        assert_eq!(first.events().unwrap().len(), 3);
        // The ledger really advanced: a repeated span start is still rejected.
        assert!(first.commit(vec![model_start]).is_err());
        assert_eq!(first.events().unwrap().len(), 3);

        // A writer outside this handle moves the durable head; the cached view
        // must observe it instead of serving a stale stream.
        let second = ChatHistory::open_with_committed_events(root.path(), port).unwrap();
        second
            .commit(vec![SemanticEventDraft::new(
                "span.completed",
                json!({"spanId":"span.model.cache","status":"completed"}),
            )])
            .unwrap();
        assert_eq!(first.events().unwrap().len(), 4);
        assert_eq!(first.committed_events().unwrap().len(), 4);
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

    #[test]
    fn message_fact_publishes_reported_run_cache_totals_and_omits_unknown_ones() {
        // A complete Run aggregate becomes the summary the Run details panel
        // reads; both keys must be present so the panel never falls back to a
        // loaded window.
        let aggregate = message_fact(
            "done",
            "1",
            MessageUsageV1 {
                model: Some("model.x"),
                input_units: Some(7_271_744),
                output_units: Some(188_758),
                cached_input_units: Some(7_207_552),
                uncached_input_units: Some(64_192),
            },
        );
        assert_eq!(aggregate["cachedInputUnits"], 7_207_552);
        assert_eq!(aggregate["uncachedInputUnits"], 64_192);
        assert_eq!(aggregate["inputUnits"], 7_271_744);

        // A Run whose provider reported no cache counters keeps the keys absent
        // rather than publishing a fabricated zero.
        let plain = message_fact("done", "1", MessageUsageV1::default());
        assert!(plain.get("cachedInputUnits").is_none());
        assert!(plain.get("uncachedInputUnits").is_none());
        assert!(plain.get("inputUnits").is_none());
    }
}

fn evidence(events: &[impl std::borrow::Borrow<Event>]) -> Vec<EvidenceRecordDto> {
    let events: Vec<&Event> = events.iter().map(std::borrow::Borrow::borrow).collect();
    events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "message.assistant" | "context.manual-completed" | "context.manual-failed" | "execution.failed" | "tool.completed" | "tool.failed"
            )
        })
        .map(|event| EvidenceRecordDto {
            id: format!("evidence.{}", event.event_id),
            category: match event.kind.as_str() {
                "message.assistant" | "context.manual-completed" => "usage",
                "tool.completed" | "tool.failed" => "provenance",
                _ => "error",
            }
            .into(),
            label: match event.kind.as_str() {
                "message.assistant" => "Authority-checked provider completion",
                "context.manual-completed" => "Authority-checked context maintenance",
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
                "cachedInputUnits": event.payload.get("cachedInputUnits").cloned().unwrap_or(Value::Null),
                "uncachedInputUnits": event.payload.get("uncachedInputUnits").cloned().unwrap_or(Value::Null),
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
