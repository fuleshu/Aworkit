//! First-class ComfyUI integration.
//!
//! The module owns four things: the persisted Settings section, the native
//! workflow tools the Agent calls in process, the model-assisted parameter
//! authoring contract, and the connection lifecycle. Everything it exposes is
//! data-in/data-out so the Settings commands stay thin.

/// Directory, relative to the runtime data root, that owns ComfyUI files.
pub const COMFYUI_DIRECTORY: &str = "comfyui";

pub mod authoring;
pub mod dto;
pub mod server;
pub mod settings;
pub mod tools;

pub use tools::{
    COMFYUI_ADAPTER_ID, COMFYUI_ADAPTER_VERSION, COMFYUI_AUTHORING_SCOPE, COMFYUI_CAPABILITY_PREFIX,
    COMFYUI_GET_WORKFLOW_CAPABILITY_ID, COMFYUI_GET_WORKFLOW_PROVIDER_NAME,
    COMFYUI_NODE_TYPES_CAPABILITY_ID, COMFYUI_NODE_TYPES_PROVIDER_NAME, COMFYUI_RESERVED_TOOL_IDS,
    COMFYUI_RUN_SCOPE, ComfyUiNativeTool, comfyui_authoring_configuration,
    comfyui_authoring_description, comfyui_authoring_provider_name, comfyui_authoring_schema,
    comfyui_capability_id, comfyui_frozen_configuration, comfyui_input_schema, comfyui_native_tool,
    comfyui_native_tools, comfyui_tool_id, is_comfyui_authoring_capability, is_comfyui_capability,
    is_comfyui_workflow_capability,
};
pub use dto::*;
pub use settings::{
    ComfyUiConfigurationV2, ComfyUiParameterKindV2, ComfyUiToolParameterV2, ComfyUiWorkflowToolV2,
};
