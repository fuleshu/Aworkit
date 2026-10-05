//! Durable native runtime for the first supported Aworkit vertical slice.
//!
//! The desktop no longer boots a sample projection. It opens canonical local
//! configuration, editable JSON workflows, and semantic Chat history.

mod approvals;
mod cancellation;
mod chat_workspace;
pub mod comfyui;
mod compaction;
mod concurrency;
mod context_inspection;
mod credential_journal;
mod credentials;
mod digest;
mod documents;
mod dto;
mod extension_inspection;
mod extension_registration;
mod external_agent;
mod graph_pass;
mod history;
mod history_retention;
mod path_actions;
pub use history_retention::{
    HistoryPayloadKindV1, HistoryStoreStatusV1, ReclaimOutcomeV1, ReclaimProgressV1,
    ReclaimReportV1,
};
mod history_index;
mod image_files;
mod images;
mod mcp;
mod mcp_tools;
mod model_tool_loop;
mod pipeline;
mod plan_contract;
mod project_scope;
mod provider;
mod provider_health;
mod record_cache;
mod repeat_tool_reminder;
mod run_events;
mod screen_capture;
mod semantic_events;
mod service;
mod workflow_capabilities;
pub use concurrency::dispatch_chat_command;
mod settings_diagnostics;
mod settings_v2;
mod tool_loop;
pub mod tool_registry;
mod tool_result_preview;
mod web_documents;

/// Records the installed application resource directory so the bundled standard
/// skills resolve in a packaged build as well as from the source tree.
pub use tool_loop::bundled_skills::register_installed_root as register_bundled_skills_root;

/// Canonical persistence-safe built-in project-tool limits. Settings, runtime
/// freezing, renderer defaults, and native QA must expose these exact values.
pub(crate) const PROJECT_FILE_READ_MAXIMUM_BYTES_V1: u64 = 256 * 1024;
pub(crate) const PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1: u64 = 512;
pub(crate) const PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1: u64 = 1000;
pub(crate) const PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1: u64 = 512;
pub(crate) const PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1: u64 = 1024 * 1024;
pub(crate) const WEB_SEARCH_MAXIMUM_RESULTS_V1: u64 = 100;
pub(crate) const WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1: u64 = 8 * 1024 * 1024;
pub(crate) const WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1: u64 = 32 * 1024;

pub use approvals::FilesystemGrant;
pub use approvals::{ApprovalMode, ApprovalResolution, ApprovalSettings, ProjectApprovalGrant};
pub use cancellation::WorkflowCancellationController;
pub use comfyui::{
    ComfyUiAutocreateRequestV2, ComfyUiAutocreateResultV2, ComfyUiConfigurationV2,
    ComfyUiInspectRequestV2, ComfyUiInspectResultV2, ComfyUiParameterKindV2, ComfyUiProbeRequestV2,
    ComfyUiProbeResultV2, ComfyUiStartRequestV2, ComfyUiStartResultV2, ComfyUiToolParameterV2,
    ComfyUiWorkflowToolV2,
};
pub use dto::*;
pub use external_agent::{ExternalAgentProbeRequestV2, ExternalAgentProbeResultV2};
pub use graph_pass::{GraphApprovalRequestV1, GraphNodeActivityV1};
pub use images::ChatImageStore;
pub use path_actions::{PathActionOutcomeV1, PathActionRequestV1};
pub use pipeline::{
    WorkflowExecutionPipeline, WorkflowExecutionRequestV1, WorkflowExecutionResultV1,
    WorkflowExecutionStatusV1, WorkflowMessageV1, WorkflowPipelineError, WorkflowProviderBindingV1,
    WorkflowReasoningActivityV1,
};
pub use semantic_events::{CommittedChatEventPort, CoreEventEnvelope};
pub use service::ChatFeedReader;
pub use service::DesktopRuntime;
pub use settings_diagnostics::{
    ProjectProbeRequestV2, ProjectProbeResultV2, ToolProbeRequestV2, ToolProbeResultV2,
};
pub use settings_v2::*;
pub use tool_loop::{WorkflowToolActivityV1, WorkflowToolBindingV1};
