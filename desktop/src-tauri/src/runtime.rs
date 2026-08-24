//! Durable native runtime for the first supported Aworkit vertical slice.
//!
//! The desktop no longer boots a sample projection. It opens canonical local
//! configuration, the editable Simple Chat workflow, and semantic Chat history.

mod credential_journal;
mod credentials;
mod documents;
mod dto;
mod extension_inspection;
mod extension_registration;
mod external_agent;
mod history;
mod mcp;
mod model_tool_loop;
mod pipeline;
mod project_scope;
mod provider;
mod provider_health;
mod service;
mod settings_diagnostics;
mod settings_v2;
mod tool_loop;

/// Canonical persistence-safe built-in project-tool limits. Settings, runtime
/// freezing, renderer defaults, and native QA must expose these exact values.
pub(crate) const PROJECT_FILE_READ_MAXIMUM_BYTES_V1: u64 = 64 * 1024;
pub(crate) const PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1: u64 = 512;

pub use dto::*;
pub use external_agent::{ExternalAgentProbeRequestV2, ExternalAgentProbeResultV2};
pub use pipeline::{
    SimpleChatExecutionPipeline, SimpleChatExecutionRequestV1, SimpleChatExecutionResultV1,
    SimpleChatExecutionStatusV1, SimpleChatMessageV1, SimpleChatPipelineError,
    SimpleChatProviderBindingV1,
};
pub use service::DesktopRuntime;
pub use settings_diagnostics::{
    ProjectProbeRequestV2, ProjectProbeResultV2, ToolProbeRequestV2, ToolProbeResultV2,
};
pub use settings_v2::*;
pub use tool_loop::{SimpleChatToolActivityV1, SimpleChatToolBindingV1};
