//! Wire contracts for the ComfyUI Settings commands.
//!
//! Every request carries the exact draft the user is looking at and every result
//! echoes the fields the UI needs to reconcile a stale answer. Nothing here
//! mutates Settings; the commands are probes and compilations, and the user
//! saves the draft through the ordinary Settings commit.

use serde::{Deserialize, Serialize};

use aworkit_capability_host::ComfyUiWorkflowInspectionV1;

use super::settings::ComfyUiWorkflowToolV2;

/// Reachability probe request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiProbeRequestV2 {
    /// Endpoint to probe.
    pub endpoint: String,
    /// Client-side draft fingerprint this answer belongs to.
    pub draft_fingerprint: String,
}

/// Reachability probe result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiProbeResultV2 {
    /// Whether the server answered.
    pub ok: bool,
    /// Human-readable outcome.
    pub message: String,
    /// Normalized endpoint that was probed.
    pub endpoint: String,
    /// Reported ComfyUI version, when the server returns one.
    pub version: Option<String>,
    /// Round-trip latency in milliseconds.
    pub latency_millis: u64,
    /// Echoed client-side draft fingerprint.
    pub draft_fingerprint: String,
}

/// Explicit local start request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiStartRequestV2 {
    /// Endpoint to wait for.
    pub endpoint: String,
    /// Local ComfyUI installation folder.
    pub install_path: String,
    /// Launch command and arguments inside that folder.
    pub launch_arguments: Vec<String>,
}

/// Explicit local start result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiStartResultV2 {
    /// Whether the server became reachable.
    pub ok: bool,
    /// Human-readable outcome.
    pub message: String,
    /// Normalized endpoint.
    pub endpoint: String,
    /// Process id of the launched server, when one was spawned.
    pub process_id: Option<u32>,
    /// Path of the captured startup log.
    pub log_path: String,
    /// Bounded tail of the captured startup output.
    pub output_tail: String,
}

/// Workflow inspection request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiInspectRequestV2 {
    /// Absolute path of the API workflow JSON.
    pub workflow_path: String,
    /// Optional endpoint used to attach live `/object_info` choices.
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// Workflow inspection result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiInspectResultV2 {
    /// Whether inspection succeeded.
    pub ok: bool,
    /// Human-readable outcome.
    pub message: String,
    /// Inspected workflow path.
    pub workflow_path: String,
    /// Every editable input found in the workflow.
    pub inspection: ComfyUiWorkflowInspectionV1,
}

/// Parameter authoring request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiAutocreateRequestV2 {
    /// ComfyUI endpoint, used to attach live `/object_info` choices.
    pub endpoint: String,
    /// Absolute path of the API workflow JSON.
    pub workflow_path: String,
    /// Client-side draft fingerprint this answer belongs to.
    pub draft_fingerprint: String,
}

/// Parameter authoring result: a proposal, never a saved change.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiAutocreateResultV2 {
    /// Whether a usable proposal was produced.
    pub ok: bool,
    /// Human-readable outcome.
    pub message: String,
    /// Proposed workflow tool, validated against the workflow.
    pub tool: ComfyUiWorkflowToolV2,
    /// Model-authored review summary.
    pub summary: String,
    /// Concrete provider that answered.
    pub provider_id: String,
    /// Concrete model that answered.
    pub model_id: String,
    /// Input tokens the authoring call used.
    pub input_tokens: u64,
    /// Output tokens the authoring call used.
    pub output_tokens: u64,
}