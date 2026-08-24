use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use super::settings_v2::{
    McpServerConfigurationV2, ProviderConfigurationV2, SettingsConfigurationV2, WorkspaceKindV2,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCommandReceipt {
    pub command_id: String,
    pub accepted: bool,
    pub current_version: u64,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_mutation: Option<CredentialMutationOutcomeV2>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMutationOperationV2 {
    Create,
    Replace,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialMutationOutcomeV2 {
    pub operation: CredentialMutationOperationV2,
    pub previous_credential_ref: Option<String>,
    pub fresh_credential_ref: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProjectionDto {
    pub chat_id: String,
    pub run_id: String,
    pub title: String,
    pub scope: String,
    pub workflow_name: Option<String>,
    pub branch: Option<String>,
    pub project_id: Option<String>,
    pub phase: String,
    pub locked_workflow: bool,
    pub queued_inputs: Vec<String>,
    pub expected_version: u64,
    pub disabled_reason: Option<String>,
    pub recovery_pending: bool,
}

/// One saved project that the supported native Simple Chat slice can select
/// before its first input. Paths stay behind the trusted native boundary.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChoiceDto {
    pub project_id: String,
    pub name: String,
    pub workspace_kind: WorkspaceKindV2,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRecordDto {
    pub id: String,
    pub category: String,
    pub label: String,
    pub state: String,
    pub value: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub version: u64,
    pub last_sequence: u64,
    pub chat: ChatProjectionDto,
    pub projects: Vec<ProjectChoiceDto>,
    pub timeline: Vec<TimelineItemDto>,
    pub evidence: Vec<EvidenceRecordDto>,
    pub events: Vec<Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCommitInput {
    pub base_url: String,
    pub model: String,
    pub credential_action: String,
    pub api_key: Option<Zeroizing<String>>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsCommitInput {
    pub command_id: String,
    pub expected_version: u64,
    pub appearance: String,
    pub portable_history_enabled: bool,
    pub provider: ProviderCommitInput,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettingsSnapshot {
    pub base_url: String,
    pub model: String,
    pub credential_configured: bool,
    pub state: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    pub version: u64,
    pub appearance: String,
    pub portable_history_enabled: bool,
    pub project_roots: Vec<String>,
    pub provider: ProviderSettingsSnapshot,
}

/// Secret-free projection of one provider's latest runtime health.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderHealthSnapshotV2 {
    pub provider_id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Full canonical Settings v2 projection with its optimistic-lock version.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsV2Snapshot {
    pub version: u64,
    pub schema_version: u16,
    pub settings: SettingsConfigurationV2,
    pub provider_health: Vec<ProviderHealthSnapshotV2>,
}

/// Version-checked full-document Settings v2 save command.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsV2CommitInput {
    pub command_id: String,
    pub expected_version: u64,
    pub settings: SettingsConfigurationV2,
}

/// Dedicated version-checked registration of one saved inert discovery.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRegisterInputV2 {
    pub command_id: String,
    pub expected_version: u64,
    pub extension_id: String,
}

/// Write-only credential create/replace command. Secret field values are
/// zeroized and never appear in a response or canonical JSON document.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialStoreInputV2 {
    pub command_id: String,
    pub expected_version: u64,
    #[serde(default)]
    pub replace_credential_ref: Option<String>,
    pub label: String,
    pub kind: String,
    #[serde(default)]
    pub bound_provider_id: Option<String>,
    #[serde(default)]
    pub bound_endpoint: Option<String>,
    pub fields: BTreeMap<String, Zeroizing<String>>,
}

/// Version-checked deletion of one unreferenced credential record.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialDeleteInputV2 {
    pub command_id: String,
    pub expected_version: u64,
    pub credential_ref: String,
}

/// Secret-safe provider connection test against the current unsaved draft.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderProbeRequestV2 {
    pub provider: ProviderConfigurationV2,
    pub model_id: String,
    #[serde(default)]
    pub replacement_credential: Option<Zeroizing<String>>,
    pub use_stored_credential: bool,
    pub draft_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderProbeResultV2 {
    pub ok: bool,
    pub message: String,
    pub provider_id: String,
    pub model_id: Option<String>,
    pub remote_model_id: Option<String>,
    pub latency_millis: u64,
    pub draft_fingerprint: String,
}

/// Model discovery request against an unsaved provider draft.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDiscoveryRequestV2 {
    pub provider: ProviderConfigurationV2,
    #[serde(default)]
    pub replacement_credential: Option<Zeroizing<String>>,
    pub use_stored_credential: bool,
    pub draft_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveredModelV2 {
    pub remote_id: String,
    pub name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDiscoveryResultV2 {
    pub provider_id: String,
    pub draft_fingerprint: String,
    pub models: Vec<DiscoveredModelV2>,
    pub message: String,
}

/// Real MCP discovery request against one unsaved server draft. Credential
/// values remain in the operating-system store and are referenced opaquely by
/// the transport bindings contained in `server`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProbeRequestV2 {
    pub server: McpServerConfigurationV2,
    pub draft_fingerprint: String,
}

/// Features actually negotiated with the MCP server during initialization.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProbeFeaturesV2 {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
    pub progress: bool,
    pub cancellation: bool,
}

/// Secret-free evidence from one completed real MCP connection and discovery.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProbeResultV2 {
    pub server_id: String,
    pub protocol_version: String,
    pub features: McpProbeFeaturesV2,
    pub tool_names: Vec<String>,
    pub resource_names: Vec<String>,
    pub prompt_names: Vec<String>,
    pub binding_hash: String,
    pub catalog_hash: String,
    pub latency_millis: u64,
    pub draft_fingerprint: String,
    pub message: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTestInput {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<Zeroizing<String>>,
    pub use_stored_credential: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResult {
    pub ok: bool,
    pub message: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowCommitInput {
    pub command_id: String,
    pub expected_version: u64,
    pub document: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSnapshot {
    pub version: u64,
    pub document: Value,
    pub editable: bool,
}
