//! A small Tauri-facing adapter over the real trusted-core desktop and Chat contracts.

use std::collections::{HashMap, HashSet};

use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    ChatAggregate, ChatCommand, ChatState, DesktopApi, DesktopCommand, DesktopReceipt,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiCommandInput {
    pub schema_version: u16,
    pub command_id: String,
    pub expected_version: u64,
    pub action: String,
    pub target_id: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCommandReceipt {
    pub command_id: String,
    pub accepted: bool,
    pub current_version: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProjectionDto {
    pub chat_id: String,
    pub run_id: String,
    pub title: String,
    pub scope: String,
    pub workflow_name: Option<String>,
    pub branch: Option<String>,
    pub phase: String,
    pub locked_workflow: bool,
    pub queued_inputs: Vec<String>,
    pub expected_version: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItemDto {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub created_at: String,
    pub status: Option<String>,
    pub action: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRecordDto {
    pub id: String,
    pub category: String,
    pub label: String,
    pub state: String,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub version: u64,
    pub last_sequence: u64,
    pub chat: ChatProjectionDto,
    pub timeline: Vec<TimelineItemDto>,
    pub evidence: Vec<EvidenceRecordDto>,
    pub events: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsCommitInput {
    pub command_id: String,
    pub expected_version: u64,
    pub appearance: String,
    pub configured_capabilities: Vec<String>,
    pub portable_history_enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    pub version: u64,
    pub appearance: String,
    pub configured_capabilities: Vec<String>,
    pub portable_history_enabled: bool,
    pub project_roots: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowCommitInput {
    pub command_id: String,
    pub expected_version: u64,
    pub document: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSnapshot {
    pub version: u64,
    pub document: Value,
}

/// Owns one local Chat aggregate and exposes only typed, receipt-oriented UI operations.
pub struct DesktopRuntime {
    api: DesktopApi,
    chat: ChatAggregate,
    timeline: Vec<TimelineItemDto>,
    evidence: Vec<EvidenceRecordDto>,
    processed: HashMap<String, ProcessedCommand>,
    settings: SettingsSnapshot,
    workflow: WorkflowSnapshot,
}

#[derive(Clone)]
struct ProcessedCommand {
    fingerprint: String,
    receipt: UiCommandReceipt,
}

impl Default for DesktopRuntime {
    fn default() -> Self {
        Self::demo().expect("the bundled demo projection uses valid stable identifiers")
    }
}

impl DesktopRuntime {
    #[must_use]
    pub fn draft() -> Self {
        Self {
            api: DesktopApi::default(),
            chat: ChatAggregate::new(id("chat.local")),
            timeline: Vec::new(),
            evidence: default_evidence(),
            processed: HashMap::new(),
            settings: SettingsSnapshot {
                version: 3,
                appearance: "system".into(),
                configured_capabilities: vec![
                    "agent.codex".into(),
                    "model.local".into(),
                    "model.standard".into(),
                    "tool.files".into(),
                    "tool.shell".into(),
                ],
                portable_history_enabled: false,
                project_roots: vec!["/workspace/project-atlas".into()],
            },
            workflow: WorkflowSnapshot {
                version: 1,
                document: default_workflow(),
            },
        }
    }

    fn demo() -> Result<Self, String> {
        let mut runtime = Self::draft();
        runtime
            .chat
            .apply(ChatCommand::Start {
                snapshot_hash: "snapshot.release-readiness.v1".into(),
            })
            .map_err(|error| error.to_string())?;
        runtime.timeline = demo_timeline();
        runtime.publish("chat.started", json!({"workflow":"Repository Engineer"}))?;
        runtime.publish("timeline.ready", json!({"count":runtime.timeline.len()}))?;
        Ok(runtime)
    }

    pub fn snapshot(&self, after_sequence: u64) -> Result<RuntimeSnapshot, String> {
        let snapshot = self
            .api
            .snapshot_after(after_sequence)
            .map_err(|error| error.to_string())?;
        Ok(RuntimeSnapshot {
            version: snapshot.version,
            last_sequence: snapshot.last_sequence,
            chat: self.chat_projection(snapshot.version),
            timeline: self.timeline.clone(),
            evidence: self.evidence.clone(),
            events: snapshot
                .events
                .into_iter()
                .map(|event| {
                    json!({
                        "sequence": event.sequence,
                        "eventId": event.event_id,
                        "kind": event.name,
                        "payload": event.payload,
                    })
                })
                .collect(),
        })
    }

    pub fn command(&mut self, input: UiCommandInput) -> Result<UiCommandReceipt, String> {
        if input.schema_version != 1 {
            return Err(format!(
                "unsupported UI command schema {}",
                input.schema_version
            ));
        }
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let command_id =
            StableId::parse(input.command_id.clone()).map_err(|error| error.to_string())?;
        let command = DesktopCommand {
            command_id: command_id.clone(),
            expected_version: input.expected_version,
            name: input.action.clone(),
            payload: input.payload.clone(),
        };
        let event_payload = json!({"action":input.action,"targetId":input.target_id});
        let mut next_chat = self.chat.clone();
        let mut next_timeline = self.timeline.clone();
        let api = self.api.clone();
        let transaction = api
            .transact_committed(&command, command_id, "chat.updated", event_payload, || {
                apply_action(&mut next_chat, &mut next_timeline, &input)
            })
            .map_err(|error| error.to_string())?;
        if !transaction.duplicate {
            self.chat = next_chat;
            self.timeline = next_timeline;
        }
        let result = receipt_after_commit(transaction.receipt, transaction.event.sequence);
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: result.clone(),
            },
        );
        Ok(result)
    }

    #[must_use]
    pub fn settings_snapshot(&self) -> SettingsSnapshot {
        self.settings.clone()
    }

    pub fn settings_commit(
        &mut self,
        input: SettingsCommitInput,
    ) -> Result<UiCommandReceipt, String> {
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        StableId::parse(input.command_id.clone()).map_err(|error| error.to_string())?;
        if input.expected_version != self.settings.version {
            return Err(format!(
                "settings version conflict: expected {}, actual {}",
                input.expected_version, self.settings.version
            ));
        }
        if !matches!(input.appearance.as_str(), "system" | "light" | "dark") {
            return Err("settings appearance must be system, light, or dark".into());
        }
        let unique: HashSet<&String> = input.configured_capabilities.iter().collect();
        if unique.len() != input.configured_capabilities.len()
            || input
                .configured_capabilities
                .iter()
                .any(|id| StableId::parse(id.clone()).is_err())
        {
            return Err("settings capability IDs must be unique stable identifiers".into());
        }
        let next_version = self
            .settings
            .version
            .checked_add(1)
            .ok_or_else(|| "settings version is exhausted".to_owned())?;
        let next_settings = SettingsSnapshot {
            version: next_version,
            appearance: input.appearance,
            configured_capabilities: input.configured_capabilities,
            portable_history_enabled: input.portable_history_enabled,
            project_roots: self.settings.project_roots.clone(),
        };
        self.publish(
            "settings.committed",
            json!({"settingsVersion": next_version}),
        )?;
        self.settings = next_settings;
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    #[must_use]
    pub fn workflow_snapshot(&self) -> WorkflowSnapshot {
        self.workflow.clone()
    }

    pub fn workflow_commit(
        &mut self,
        input: WorkflowCommitInput,
    ) -> Result<UiCommandReceipt, String> {
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        StableId::parse(input.command_id.clone()).map_err(|error| error.to_string())?;
        if input.expected_version != self.workflow.version {
            return Err(format!(
                "workflow version conflict: expected {}, actual {}",
                input.expected_version, self.workflow.version
            ));
        }
        validate_workflow_document(&input.document)?;
        let next_version = self
            .workflow
            .version
            .checked_add(1)
            .ok_or_else(|| "workflow version is exhausted".to_owned())?;
        let next_workflow = WorkflowSnapshot {
            version: next_version,
            document: input.document,
        };
        self.publish(
            "workflow.committed",
            json!({"workflowVersion": next_version}),
        )?;
        self.workflow = next_workflow;
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    fn publish(
        &self,
        name: &str,
        payload: Value,
    ) -> Result<aworkit_trusted_core::DesktopEvent, String> {
        let next = self
            .api
            .snapshot_after(0)
            .map_err(|error| error.to_string())?
            .last_sequence
            .saturating_add(1);
        self.api
            .publish_committed(id(&format!("event.{next}")), name, payload)
            .map_err(|error| error.to_string())
    }

    fn chat_projection(&self, version: u64) -> ChatProjectionDto {
        let draft = self.chat.state == ChatState::Draft;
        ChatProjectionDto {
            chat_id: self.chat.chat_id.to_string(),
            run_id: if draft { "run.draft" } else { "run.local" }.into(),
            title: if draft {
                "New Chat"
            } else {
                "Release readiness"
            }
            .into(),
            scope: "Project Atlas".into(),
            workflow_name: self
                .chat
                .snapshot_hash
                .as_ref()
                .map(|_| "Repository Engineer".into()),
            branch: Some(if draft { "main" } else { "codex/auth-refresh" }.into()),
            phase: phase(self.chat.state).into(),
            locked_workflow: self.chat.snapshot_hash.is_some(),
            queued_inputs: self
                .chat
                .queued_inputs
                .iter()
                .map(ToString::to_string)
                .collect(),
            expected_version: version,
        }
    }
}

fn apply_action(
    chat: &mut ChatAggregate,
    timeline: &mut Vec<TimelineItemDto>,
    input: &UiCommandInput,
) -> Result<(), String> {
    match input.action.as_str() {
        "new_chat" => {
            *chat = ChatAggregate::new(
                StableId::parse(input.command_id.clone()).map_err(|error| error.to_string())?,
            );
            timeline.clear();
        }
        "start" => {
            let workflow_id = string_field(&input.payload, "workflowId")?;
            StableId::parse(workflow_id.clone()).map_err(|error| error.to_string())?;
            validate_attachment_references(&input.payload)?;
            chat.apply(ChatCommand::Start {
                snapshot_hash: workflow_id,
            })
            .map_err(|error| error.to_string())?;
            push_user_message(timeline, input)?;
        }
        "enqueue" => {
            let input_id = id(&format!("input.{}", timeline.len() + 1));
            chat.apply(ChatCommand::QueueInput { input_id })
                .map_err(|error| error.to_string())?;
            push_user_message(timeline, input)?;
        }
        "pause" => chat
            .apply(ChatCommand::Pause)
            .map(|_| ())
            .map_err(|error| error.to_string())?,
        "resume" => chat
            .apply(ChatCommand::Resume)
            .map(|_| ())
            .map_err(|error| error.to_string())?,
        "cancel" => {
            chat.apply(ChatCommand::Cancel)
                .map_err(|error| error.to_string())?;
            chat.apply(ChatCommand::Cancelled)
                .map_err(|error| error.to_string())?;
        }
        "approval" => {
            if bool_field(&input.payload, "approved")? {
                chat.apply(ChatCommand::Approve)
                    .map_err(|error| error.to_string())?;
            } else {
                chat.apply(ChatCommand::Cancel)
                    .map_err(|error| error.to_string())?;
                chat.apply(ChatCommand::Cancelled)
                    .map_err(|error| error.to_string())?;
            }
        }
        "retry" => chat
            .apply(ChatCommand::Retry)
            .map(|_| ())
            .map_err(|error| error.to_string())?,
        "fork" | "continue" => {
            let child_chat_id = id(&format!("chat.child.{}", timeline.len() + 1));
            chat.apply(ChatCommand::Fork { child_chat_id })
                .map_err(|error| error.to_string())?;
            timeline.push(TimelineItemDto {
                id: format!("lineage.{}", timeline.len() + 1),
                kind: "route".into(),
                title: if input.action == "fork" {
                    "Fork requested"
                } else {
                    "Continue requested"
                }
                .into(),
                body: "The trusted core accepted a new child-Chat lineage operation.".into(),
                created_at: "now".into(),
                status: Some("completed".into()),
                action: None,
                metadata: json!({"targetId":input.target_id}),
            });
        }
        other => return Err(format!("unsupported desktop action {other}")),
    }
    Ok(())
}

fn push_user_message(
    timeline: &mut Vec<TimelineItemDto>,
    input: &UiCommandInput,
) -> Result<(), String> {
    let body = string_field(&input.payload, "input")?;
    if body.len() > 256 * 1024 || body.chars().any(|character| character == '\0') {
        return Err("command input is empty, oversized, or contains NUL".into());
    }
    timeline.push(TimelineItemDto {
        id: format!("message.{}", timeline.len() + 1),
        kind: "message".into(),
        title: "You".into(),
        body,
        created_at: "now".into(),
        status: Some("queued".into()),
        action: None,
        metadata: json!({"commandId":input.command_id}),
    });
    Ok(())
}

fn validate_attachment_references(payload: &Value) -> Result<(), String> {
    let Some(value) = payload.get("attachments") else {
        return Ok(());
    };
    let references = value
        .as_array()
        .ok_or_else(|| "attachments must be an array of references".to_owned())?;
    if references.len() > 32
        || references.iter().any(|reference| {
            reference.as_str().is_none_or(|value| {
                value.trim().is_empty()
                    || value.len() > 4_096
                    || value.chars().any(char::is_control)
            })
        })
    {
        return Err("attachment references are empty, oversized, or malformed".into());
    }
    Ok(())
}

fn receipt_after_commit(receipt: DesktopReceipt, version: u64) -> UiCommandReceipt {
    UiCommandReceipt {
        command_id: receipt.command_id.to_string(),
        accepted: receipt.accepted_for_processing,
        current_version: version,
        reason: None,
    }
}

fn command_fingerprint(command: &impl Serialize) -> Result<String, String> {
    serde_json::to_string(command).map_err(|error| format!("invalid desktop command: {error}"))
}

fn replay_processed(
    processed: &ProcessedCommand,
    fingerprint: &str,
) -> Result<UiCommandReceipt, String> {
    if processed.fingerprint == fingerprint {
        Ok(processed.receipt.clone())
    } else {
        Err("desktop command ID was reused with different content".into())
    }
}

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("internal Aworkit IDs are valid")
}

fn string_field(payload: &Value, name: &str) -> Result<String, String> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("command payload requires non-empty {name}"))
}

fn bool_field(payload: &Value, name: &str) -> Result<bool, String> {
    payload
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("command payload requires boolean {name}"))
}

fn phase(state: ChatState) -> &'static str {
    match state {
        ChatState::Draft => "draft",
        ChatState::Active => "running",
        ChatState::WaitingInput => "waiting_input",
        ChatState::WaitingApproval => "awaiting_approval",
        ChatState::Paused => "paused",
        ChatState::Cancelling => "cancelling",
        ChatState::Cancelled => "cancelled",
        ChatState::Completed => "completed",
        ChatState::Failed => "failed",
    }
}

fn demo_timeline() -> Vec<TimelineItemDto> {
    vec![
        TimelineItemDto {
            id: "message.user.1".into(),
            kind: "message".into(),
            title: "You".into(),
            body: "Check whether the auth refresh branch is ready to merge.".into(),
            created_at: "12:41".into(),
            status: None,
            action: None,
            metadata: Value::Null,
        },
        TimelineItemDto {
            id: "plan.1".into(),
            kind: "plan".into(),
            title: "Release readiness plan".into(),
            body: "Inspect changes\nRun workspace tests\nReview migration risk\nSummarize readiness".into(),
            created_at: "12:41".into(),
            status: Some("3 of 4 complete".into()),
            action: None,
            metadata: json!({"completed":3,"total":4}),
        },
        TimelineItemDto {
            id: "tool.1".into(),
            kind: "tool".into(),
            title: "Shell".into(),
            body: "cargo test --workspace --all-targets".into(),
            created_at: "12:42".into(),
            status: Some("completed".into()),
            action: None,
            metadata: json!({"exitCode":0,"tests":428}),
        },
        TimelineItemDto {
            id: "message.assistant.1".into(),
            kind: "message".into(),
            title: "Aworkit".into(),
            body: "The branch is ready for review. All tests passed; the remaining item is a manual migration sign-off.".into(),
            created_at: "12:43".into(),
            status: None,
            action: None,
            metadata: Value::Null,
        },
    ]
}

fn default_evidence() -> Vec<EvidenceRecordDto> {
    vec![
        EvidenceRecordDto {
            id: "evidence.tool.1".into(),
            category: "provenance".into(),
            label: "Shell invocation".into(),
            state: "available".into(),
            value: json!({"command":"cargo test --workspace --all-targets","workingDirectory":"/workspace/project-atlas","exitCode":0}),
        },
        EvidenceRecordDto {
            id: "evidence.usage.1".into(),
            category: "usage".into(),
            label: "Usage and cost".into(),
            state: "available".into(),
            value: json!({"inputTokens":1284,"outputTokens":326,"cost":"local / unpriced"}),
        },
        EvidenceRecordDto {
            id: "evidence.debug.1".into(),
            category: "debug".into(),
            label: "Detailed protocol capture".into(),
            state: "redacted".into(),
            value: Value::Null,
        },
        EvidenceRecordDto {
            id: "evidence.routing.1".into(),
            category: "routing".into(),
            label: "Frozen route decision".into(),
            state: "available".into(),
            value: json!({"route":"quality","source":"workflow.transition.deep-review"}),
        },
        EvidenceRecordDto {
            id: "evidence.approval.1".into(),
            category: "approval".into(),
            label: "Approval decision".into(),
            state: "expired".into(),
            value: Value::Null,
        },
        EvidenceRecordDto {
            id: "evidence.artifact.1".into(),
            category: "artifact".into(),
            label: "Test report artifact".into(),
            state: "available".into(),
            value: json!({"contentId":"sha256:demo","mediaType":"text/plain"}),
        },
        EvidenceRecordDto {
            id: "evidence.retry.1".into(),
            category: "retry".into(),
            label: "Attempt policy".into(),
            state: "available".into(),
            value: json!({"attempt":1,"retrySafe":true}),
        },
        EvidenceRecordDto {
            id: "evidence.opacity.1".into(),
            category: "opacity".into(),
            label: "Provider-private reasoning".into(),
            state: "opaque".into(),
            value: Value::Null,
        },
        EvidenceRecordDto {
            id: "evidence.retention.1".into(),
            category: "retention".into(),
            label: "Detailed capture retention".into(),
            state: "unsupported".into(),
            value: Value::Null,
        },
    ]
}

fn default_workflow() -> Value {
    json!({
        "schemaVersion": 1,
        "name": "Repository Engineer",
        "nodes": [
            {"id":"input.1","label":"Request","type":"input","position":{"x":36,"y":205}},
            {"id":"model.fast","label":"Fast model","type":"model","position":{"x":245,"y":112},"requirement":"model.fast","capabilityStatus":"ready"},
            {"id":"model.quality","label":"Quality model","type":"model","position":{"x":245,"y":296},"requirement":"model.quality","capabilityStatus":"ready"},
            {"id":"plugin.review","label":"acme.code-review@2.x","type":"plugin","position":{"x":470,"y":112},"requirement":"plugin.code-review@2.x","capabilityStatus":"missing"},
            {"id":"gate.1","label":"Review approval","type":"gate","position":{"x":470,"y":296},"required":true},
            {"id":"output.1","label":"Response","type":"output","position":{"x":710,"y":205}}
        ],
        "edges": [
            {"id":"request-fast","source":"input.1","target":"model.fast"},
            {"id":"fast-review","source":"model.fast","target":"plugin.review"},
            {"id":"request-quality","source":"input.1","target":"model.quality","label":"deep review"},
            {"id":"review-gate","source":"plugin.review","target":"gate.1"},
            {"id":"quality-gate","source":"model.quality","target":"gate.1"},
            {"id":"gate-response","source":"gate.1","target":"output.1"}
        ],
        "unknownExtension": {"retain": true},
        "comments": "Unknown fields stay lossless."
    })
}

fn validate_workflow_document(document: &Value) -> Result<(), String> {
    let object = document
        .as_object()
        .ok_or_else(|| "workflow document must be an object".to_owned())?;
    if object
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .filter(|version| *version > 0)
        .is_none()
        || !object.get("nodes").is_some_and(Value::is_array)
        || !object.get("edges").is_some_and(Value::is_array)
    {
        return Err(
            "workflow document requires a positive schemaVersion plus nodes and edges arrays"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_core_first_send_locks_workflow_and_deduplicates_command_ids() {
        let mut runtime = DesktopRuntime::draft();
        let command = UiCommandInput {
            schema_version: 1,
            command_id: "command.first".into(),
            expected_version: 0,
            action: "start".into(),
            target_id: Some("chat.local".into()),
            payload: json!({"workflowId":"workflow.review","input":"hello","attachments":[]}),
        };
        let first = runtime.command(command.clone()).expect("first send");
        let duplicate = runtime.command(command).expect("idempotent retry");
        assert_eq!(first.current_version, duplicate.current_version);
        let snapshot = runtime.snapshot(0).expect("snapshot");
        assert!(snapshot.chat.locked_workflow);
        assert_eq!(snapshot.chat.phase, "running");
        assert_eq!(snapshot.timeline.len(), 1);
    }

    #[test]
    fn stale_expected_version_and_illegal_controls_are_rejected_without_events() {
        let mut runtime = DesktopRuntime::draft();
        let error = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "command.pause".into(),
                expected_version: 1,
                action: "pause".into(),
                target_id: None,
                payload: Value::Null,
            })
            .expect_err("stale command");
        assert!(error.contains("version conflict"));
        assert_eq!(runtime.snapshot(0).expect("snapshot").last_sequence, 0);
    }

    #[test]
    fn rejected_domain_command_consumes_neither_version_nor_idempotency_key() {
        let mut runtime = DesktopRuntime::draft();
        let command_id = "command.reusable";
        let error = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: command_id.into(),
                expected_version: 0,
                action: "pause".into(),
                target_id: None,
                payload: json!({}),
            })
            .expect_err("pause is illegal in draft");
        assert!(error.contains("domain command was rejected"));
        assert_eq!(runtime.snapshot(0).expect("snapshot").version, 0);
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: command_id.into(),
                expected_version: 0,
                action: "start".into(),
                target_id: None,
                payload: json!({"workflowId":"workflow.review","input":"hello","attachments":[]}),
            })
            .expect("the rejected ID remains available");
        assert_eq!(runtime.snapshot(0).expect("snapshot").version, 1);
    }

    #[test]
    fn native_controls_follow_core_lifecycle_legality() {
        let mut runtime = DesktopRuntime::draft();
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "command.start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: None,
                payload: json!({"workflowId":"workflow.review","input":"hello","attachments":[]}),
            })
            .expect("start");
        for (ordinal, action) in ["pause", "resume", "cancel", "fork", "continue"]
            .into_iter()
            .enumerate()
        {
            runtime
                .command(UiCommandInput {
                    schema_version: 1,
                    command_id: format!("command.{action}"),
                    expected_version: ordinal as u64 + 1,
                    action: action.into(),
                    target_id: None,
                    payload: json!({}),
                })
                .unwrap_or_else(|error| panic!("{action}: {error}"));
        }
        let snapshot = runtime.snapshot(0).expect("snapshot");
        assert_eq!(snapshot.version, 6);
        assert_eq!(snapshot.chat.phase, "cancelled");
        assert_eq!(
            snapshot
                .timeline
                .iter()
                .filter(|item| item.kind == "route")
                .count(),
            2
        );
    }

    #[test]
    fn new_chat_command_replaces_the_selected_projection_with_an_unlocked_draft() {
        let mut runtime = DesktopRuntime::default();
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "command.new-chat".into(),
                expected_version: 2,
                action: "new_chat".into(),
                target_id: None,
                payload: json!({}),
            })
            .expect("new chat");
        let snapshot = runtime.snapshot(0).expect("snapshot");
        assert_eq!(snapshot.chat.title, "New Chat");
        assert_eq!(snapshot.chat.phase, "draft");
        assert!(!snapshot.chat.locked_workflow);
        assert!(snapshot.timeline.is_empty());
    }

    #[test]
    fn approval_and_retry_commands_are_projected_from_core_state() {
        let mut approval = DesktopRuntime::draft();
        approval
            .chat
            .apply(ChatCommand::Start {
                snapshot_hash: "snapshot".into(),
            })
            .expect("start");
        approval
            .chat
            .apply(ChatCommand::Wait {
                reason: aworkit_trusted_core::WaitReason::Approval,
            })
            .expect("approval wait");
        approval
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "command.approve".into(),
                expected_version: 0,
                action: "approval".into(),
                target_id: Some("approval.1".into()),
                payload: json!({"approved":true}),
            })
            .expect("approve");
        assert_eq!(
            approval.snapshot(0).expect("snapshot").chat.phase,
            "running"
        );

        let mut retry = DesktopRuntime::draft();
        retry
            .chat
            .apply(ChatCommand::Start {
                snapshot_hash: "snapshot".into(),
            })
            .expect("start");
        retry
            .chat
            .apply(ChatCommand::Fail { retryable: true })
            .expect("fail");
        retry
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "command.retry".into(),
                expected_version: 0,
                action: "retry".into(),
                target_id: None,
                payload: json!({}),
            })
            .expect("retry");
        assert_eq!(retry.snapshot(0).expect("snapshot").chat.phase, "running");
    }

    #[test]
    fn settings_commits_are_versioned_validated_and_idempotent() {
        let mut runtime = DesktopRuntime::draft();
        let command = SettingsCommitInput {
            command_id: "desktop.settings.1".into(),
            expected_version: 3,
            appearance: "dark".into(),
            configured_capabilities: vec!["model.local".into(), "tool.files".into()],
            portable_history_enabled: true,
        };
        let first = runtime
            .settings_commit(command.clone())
            .expect("settings commit");
        let duplicate = runtime.settings_commit(command).expect("idempotent retry");
        assert_eq!(first.current_version, 4);
        assert_eq!(duplicate.current_version, 4);
        assert_eq!(runtime.settings_snapshot().appearance, "dark");
        assert!(runtime.settings_snapshot().portable_history_enabled);
        let collision = runtime
            .settings_commit(SettingsCommitInput {
                command_id: "desktop.settings.1".into(),
                expected_version: 3,
                appearance: "light".into(),
                configured_capabilities: vec!["model.local".into()],
                portable_history_enabled: false,
            })
            .expect_err("changed idempotency content");
        assert!(collision.contains("reused with different content"));
    }

    #[test]
    fn workflow_commit_preserves_unknown_json_and_rejects_stale_overwrite() {
        let mut runtime = DesktopRuntime::draft();
        let mut document = default_workflow();
        document["futureRoot"] = json!({"retained": true});
        runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "desktop.workflow.1".into(),
                expected_version: 1,
                document,
            })
            .expect("workflow commit");
        assert_eq!(
            runtime.workflow_snapshot().document["futureRoot"],
            json!({"retained": true})
        );
        let error = runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "desktop.workflow.2".into(),
                expected_version: 1,
                document: default_workflow(),
            })
            .expect_err("stale overwrite");
        assert!(error.contains("version conflict"));
        assert_eq!(runtime.workflow_snapshot().version, 2);
    }
}
