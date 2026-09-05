#![allow(
    clippy::assigning_clones,
    clippy::cast_possible_truncation,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::duration_suboptimal_units,
    clippy::format_collect,
    clippy::if_not_else,
    clippy::ignored_unit_patterns,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_pass_by_value,
    clippy::needless_question_mark,
    clippy::nonminimal_bool,
    clippy::op_ref,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::redundant_else,
    clippy::should_implement_trait,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::uninlined_format_args,
    clippy::unnecessary_wraps,
    clippy::unnested_or_patterns,
    clippy::unused_self,
    clippy::wildcard_imports,
    clippy::zero_sized_map_values
)]
//! Core-approved, generation-fenced capability execution.
mod anthropic_messages;
mod codex_app_server;
mod external_agent;
mod files;
mod gateway;
mod google_gemini;
mod isolation;
mod materialize;
mod mcp;
mod model;
pub mod model_images;
mod model_result;
mod model_tools;
mod normalize;
mod openai_compatible;
mod plugin;
mod process;
mod provider_tools;
mod provider_transport;
mod registry;
mod tools;
mod web;
pub use anthropic_messages::{
    AnthropicConnectionTestV1, AnthropicMessagesLimitsV1, AnthropicMessagesProvider,
    AnthropicMessagesProviderConfig, AnthropicMessagesProviderError, AnthropicModelV1,
};
pub use codex_app_server::{
    CodexAppServerAccountV1, CodexAppServerCapabilitiesV1, CodexAppServerEnvironmentV1,
    CodexAppServerProbeConfigV1, CodexAppServerProbeError, CodexAppServerProbeLimitsV1,
    CodexAppServerProbeResultV1, probe_codex_app_server_v1,
};
pub use external_agent::*;
pub use files::{
    FileAuthority, FileEditRequestV1, FileEditResultV1, FileEffectDescriptorV1, FileEffectKindV1,
    FileGrepMatchV1, FileGrepRequestV1, FileGrepResultV1, FileListEntryV1, FileListRequestV1,
    FileListResultV1, FileReadRequestV1, FileReadResultV1, FileSearchRequestV1, FileSearchResultV1,
    FileToolError, FileWriteRequestV1, FileWriteResultV1, ProjectFiles,
};
pub use gateway::{
    AdmissionDispositionV1, AdmissionReceipt, AdmittedInvocationDispatcherV1,
    ApprovedInvocationEnvelopeV1, CapabilityHost, DispatchLifecycleV1, HostControlEnvelopeV1,
    HostControlKindV1, HostDispatchReceiptV1, HostError,
};
pub use google_gemini::{
    GoogleGeminiConnectionTestV1, GoogleGeminiLimitsV1, GoogleGeminiModelV1, GoogleGeminiProvider,
    GoogleGeminiProviderConfig, GoogleGeminiProviderError,
};
pub use isolation::*;
pub use materialize::{
    InjectionTargetV1, RedeemLeaseRequestV1, SecretDeliveryV1, SecretFieldPlanV1,
    SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializationV1, SecretMaterializer,
};
pub use mcp::*;
pub use model::{
    FrozenModelGateway, ModelCandidateV1, ModelDispatchEvidenceV1, ModelEventObserverV1,
    ModelEventV1, ModelGateway, ModelProvider, ModelRequest, ModelRequestV1, ModelResolutionPlanV1,
    ModelResponse, ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
};
pub use model_result::{
    ModelResultEventV1, ModelTurnProjectionV1, project_model_events, project_model_tool_events,
};
pub use model_tools::{
    ModelAssistantContentV1, ModelProviderContextV1, ModelToolCallV1, ModelToolDefinitionV1,
    ModelToolDispatchEvidenceV1, ModelToolEventV1, ModelToolExchangeV1, ModelToolRequestV1,
    ModelToolResultV1,
};
pub use normalize::{
    CapabilityOutcome, CapabilityOutcomeV1, DispatchEvidenceV1, EffectEvidenceV1,
    HostInvocationEventV1, InvocationNormalizer, NormalizeError, NormalizedContentV1,
    OutcomeDispositionV1, OutcomeKind, Redactor, RetrySafetyV1, StreamNormalizer,
    StreamingRedactor, TerminalEvidenceV1, classify_outcome,
};
pub use openai_compatible::{
    OpenAiCompatibleLimitsV1, OpenAiCompatibleProvider, OpenAiCompatibleProviderConfig,
    OpenAiCompatibleProviderError, OpenAiConnectionTestV1, OpenAiDiscoveredModelV1,
};
pub use plugin::{
    AttestedPluginPinV1, ExtensionManifestV1, NativePluginProcessV1, PinnedPluginManifestV1,
    PluginCancelRequestV1, PluginCancelResultV1, PluginContributionKindV1, PluginContributionV1,
    PluginDependencyV1, PluginDispatchPhaseV1, PluginEffectStatusV1, PluginEntryPointV1,
    PluginFrameCodecV1, PluginFrameDecoderV1, PluginFrameError, PluginHandshakeIdentityV1,
    PluginHandshakeRequestV1, PluginHandshakeResultV1, PluginHealthRequestV1, PluginHealthResultV1,
    PluginHealthStatusV1, PluginInvocationAcceptedV1, PluginInvocationEventKindV1,
    PluginInvocationEventV1, PluginInvocationRequestV1, PluginInvocationResultV1,
    PluginInvocationSettlementV1, PluginLifecycleError, PluginLifecycleLimitsV1,
    PluginLifecycleStateV1, PluginManifestError, PluginManifestLimitsV1, PluginPinError,
    PluginProcessDiagnosticsV1, PluginProcessError, PluginProcessExitV1, PluginProcessLimitsV1,
    PluginProtocolErrorV1, PluginProtocolFrameV1, PluginProtocolLimitsV1, PluginProtocolMessageV1,
    PluginReplayDispositionV1, PluginRestartPolicyV1, PluginShutdownRequestV1,
    PluginShutdownResultV1, PluginTerminalStatusV1, TRUSTED_PLUGIN_SECURITY_DISCLOSURE,
    TrustedPluginLifecycleV1, parse_extension_manifest_v1,
};
pub use process::{
    CancellationToken, ControlledProcessResult, HermeticProcessPort, HermeticProcessStep,
    NativeProcessPort, PlatformProcessHealthV1, PlatformProcessPort, ProcessError, ProcessRequest,
    ProcessResult, ProcessRunner, ProcessSpecV1, ProcessTermination,
};
pub use registry::{
    AdapterDescriptor, AdapterRegistry, CapabilityDescriptor, CapabilityKind,
    FrozenAdapterRegistry, RegistryError, SideEffectClass, build_extension_handshake_v1,
};
pub use tools::{
    ArgumentVectorInvocationV1, BuiltInProcessTools, HostToolLimitsV1, PythonInvocationV1,
    ShellInvocationV1, ToolAdapterError, ToolAuthorityModeV1,
};
pub use web::{
    MAXIMUM_WEB_DOCUMENT_BYTES, WebDocumentMetadataV1, WebDocumentV1, WebExtractPageV1,
    WebExtractionQualityV1, WebFetchResultV1, WebRenderSnapshotV1, WebRendererPort,
    WebSearchAttemptV1, WebSearchBackendV1, WebSearchConfigurationV1, WebSearchFreshnessModeV1,
    WebSearchFreshnessV1, WebSearchOutcomeV1, WebSearchProviderTierV1, WebSearchProviderUsageV1,
    WebSearchResultV1, WebSourceV1, WebToolError, WebTools, WebTransportPort,
};
