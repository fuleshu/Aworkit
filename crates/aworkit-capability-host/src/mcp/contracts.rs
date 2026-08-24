//! Aworkit-owned contracts for configured Model Context Protocol sessions.

use std::collections::BTreeMap;

use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The configured transport class. It does not imply any isolation strength.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKindV1 {
    Stdio,
    StreamableHttp,
}

/// Exact core-attested server identity admitted to one host generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerManifestV1 {
    pub server_id: StableId,
    pub adapter_version: String,
    pub binding_hash: String,
    pub host_generation: ProcessGeneration,
    pub configured: bool,
    pub enabled: bool,
    pub core_attested: bool,
    pub transport: McpTransportKindV1,
    pub minimum_protocol_version: u16,
    pub maximum_protocol_version: u16,
    pub maximum_in_flight: usize,
    pub maximum_progress_events: usize,
    pub secret_slots: Vec<String>,
    pub workspace_roots: Vec<String>,
}

/// Features actually negotiated with the server, never inferred from config.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpFeatureSetV1 {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
    pub progress: bool,
    pub cancellation: bool,
}

/// A discovered callable entry with the schema identity frozen at initialization.
/// The schema and description are retained so an owning core can build exact
/// model-facing tool definitions without a second discovery round trip.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolDescriptorV1 {
    pub name: String,
    pub input_schema_hash: String,
    pub side_effect_known_read_only: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input_schema: Value,
}

/// Runtime discovery evidence. It is not canonical configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogV1 {
    pub tools: Vec<McpToolDescriptorV1>,
    pub resources: Vec<String>,
    pub prompts: Vec<String>,
}

/// Bounded initialize request sent after exact attestation checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpInitializeRequestV1 {
    pub server_id: StableId,
    pub host_generation: ProcessGeneration,
    pub minimum_protocol_version: u16,
    pub maximum_protocol_version: u16,
}

/// Server-negotiated protocol and catalog snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpInitializeResponseV1 {
    pub server_id: StableId,
    pub protocol_version: u16,
    pub features: McpFeatureSetV1,
    pub catalog: McpCatalogV1,
}

/// Exact discovery snapshot retained for the lifetime of one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpCapabilitySnapshotV1 {
    pub server_id: StableId,
    pub host_generation: ProcessGeneration,
    pub binding_hash: String,
    pub protocol_version: u16,
    pub features: McpFeatureSetV1,
    pub catalog: McpCatalogV1,
    pub catalog_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpCallKindV1 {
    Tool,
    Resource,
    Prompt,
}

/// An approved MCP operation pinned to the discovery schema seen at initialize.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCallV1 {
    pub invocation_id: StableId,
    pub kind: McpCallKindV1,
    pub name: String,
    pub expected_schema_hash: Option<String>,
    pub arguments: Value,
}

/// Source-provided progress retained in exact sequence order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProgressV1 {
    pub sequence: u64,
    pub message: String,
}

/// A successful or known terminal response from the peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPeerCallResultV1 {
    pub result: Value,
    pub progress: Vec<McpProgressV1>,
}

/// Dispatch evidence attached to transport failures and disconnects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDispatchMilestoneV1 {
    DefinitelyNotStarted,
    Started,
    Unknown,
}

/// Normalized transport failure without transport-native error objects.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[error("{code}: {message}")]
pub struct McpPeerErrorV1 {
    pub code: String,
    pub message: String,
    pub dispatch: McpDispatchMilestoneV1,
    pub transport_lost: bool,
}

/// Evidence returned for a cancellation control request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpCancellationEvidenceV1 {
    ConfirmedBeforeEffect,
    ConfirmedAfterStart,
    Unsupported,
    Unknown,
}

/// Non-terminal acknowledgement from the reserved cancellation control path.
/// The invocation itself remains the sole producer of a terminal outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpCancellationReceiptV1 {
    pub invocation_id: StableId,
    pub evidence: McpCancellationEvidenceV1,
    pub protocol: McpProtocolEvidenceV1,
}

/// Runtime protocol evidence suitable for redacted diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpProtocolEvidenceV1 {
    pub server_id: StableId,
    pub protocol_version: u16,
    pub catalog_hash: String,
    pub reconnect_count: u32,
    pub transport_lost: bool,
    pub definitely_not_started: bool,
}

/// Current local lifecycle facts without transport-native handles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpSessionHealthV1 {
    pub server_id: StableId,
    pub host_generation: ProcessGeneration,
    pub degraded: bool,
    pub closing: bool,
    pub retired: bool,
    pub reconnecting: bool,
    pub in_flight: usize,
    pub maximum_in_flight: usize,
    pub settled_invocations: usize,
    pub maximum_settled_invocations: usize,
    pub reconnect_count: u32,
    pub reconnect_budget: u32,
}

/// Result plus conservative effect classification from the host normalizer.
#[derive(Clone, Debug, PartialEq)]
pub struct McpCallOutcomeV1 {
    pub result: Option<Value>,
    pub progress: Vec<McpProgressV1>,
    pub outcome: crate::CapabilityOutcomeV1,
    pub evidence: McpProtocolEvidenceV1,
}

/// Forwarding metadata exposed to external-agent adapters only when negotiated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardableMcpSetV1 {
    pub servers: BTreeMap<String, McpCapabilitySnapshotV1>,
}

/// Replaceable stdio/HTTP peer boundary. Implementations own transport objects;
/// the session manager owns all identity, replay, and effect policy.
pub trait McpPeerPort: Send + Sync {
    fn initialize(
        &self,
        manifest: &McpServerManifestV1,
        request: &McpInitializeRequestV1,
    ) -> Result<McpInitializeResponseV1, McpPeerErrorV1>;

    fn invoke(
        &self,
        manifest: &McpServerManifestV1,
        call: &McpCallV1,
    ) -> Result<McpPeerCallResultV1, McpPeerErrorV1>;

    fn cancel(
        &self,
        manifest: &McpServerManifestV1,
        invocation_id: &StableId,
    ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1>;

    fn close(&self, manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1>;
}
