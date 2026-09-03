//! Durable native runtime for the first supported Aworkit vertical slice.
//!
//! The desktop no longer boots a sample projection. It opens canonical local
//! configuration, editable JSON workflows, and semantic Chat history.

mod credential_journal;
mod credentials;
mod documents;
mod dto;
mod extension_inspection;
mod extension_registration;
mod external_agent;
mod graph_pass;
mod history;
mod mcp;
mod mcp_tools;
mod model_tool_loop;
mod pipeline;
mod plan_contract;
mod project_scope;
mod provider;
mod provider_health;
mod repeat_tool_reminder;
mod run_events;
mod semantic_events;
mod service;
mod settings_diagnostics;
mod settings_v2;
mod tool_loop;

/// Canonical persistence-safe built-in project-tool limits. Settings, runtime
/// freezing, renderer defaults, and native QA must expose these exact values.
pub(crate) const PROJECT_FILE_READ_MAXIMUM_BYTES_V1: u64 = 64 * 1024;
pub(crate) const PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1: u64 = 512;
pub(crate) const PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1: u64 = 1000;
pub(crate) const PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1: u64 = 512;
pub(crate) const PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1: u64 = 1024 * 1024;
pub(crate) const WEB_SEARCH_MAXIMUM_RESULTS_V1: u64 = 8;
pub(crate) const WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1: u64 = 1024 * 1024;
pub(crate) const WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1: u64 = 32 * 1024;

pub use dto::*;
pub use external_agent::{ExternalAgentProbeRequestV2, ExternalAgentProbeResultV2};
pub use graph_pass::{GraphApprovalRequestV1, GraphNodeActivityV1};
pub use pipeline::{
    WorkflowExecutionPipeline, WorkflowExecutionRequestV1, WorkflowExecutionResultV1,
    WorkflowExecutionStatusV1, WorkflowMessageV1, WorkflowPipelineError, WorkflowProviderBindingV1,
    WorkflowReasoningActivityV1,
};
pub use semantic_events::{CommittedChatEventPort, CoreEventEnvelope};
pub use service::DesktopRuntime;
pub use settings_diagnostics::{
    ProjectProbeRequestV2, ProjectProbeResultV2, ToolProbeRequestV2, ToolProbeResultV2,
};
pub use settings_v2::*;
pub use tool_loop::{WorkflowToolActivityV1, WorkflowToolBindingV1};
