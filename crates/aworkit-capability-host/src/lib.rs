//! Core-approved, generation-fenced capability execution.
mod external_agent;
mod files;
mod gateway;
mod isolation;
mod materialize;
mod mcp;
mod model;
mod normalize;
mod plugin;
mod process;
mod registry;
mod tools;
pub use external_agent::*;
pub use files::{
    FileAuthority, FileEditRequestV1, FileEditResultV1, FileEffectDescriptorV1, FileEffectKindV1,
    FileReadRequestV1, FileReadResultV1, FileSearchRequestV1, FileSearchResultV1, FileToolError,
    ProjectFiles,
};
pub use gateway::{
    AdmissionDispositionV1, AdmissionReceipt, AdmittedInvocationDispatcherV1,
    ApprovedInvocationEnvelopeV1, CapabilityHost, DispatchLifecycleV1, HostControlEnvelopeV1,
    HostControlKindV1, HostDispatchReceiptV1, HostError,
};
pub use isolation::*;
pub use materialize::{
    InjectionTargetV1, RedeemLeaseRequestV1, SecretDeliveryV1, SecretFieldPlanV1,
    SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializationV1, SecretMaterializer,
};
pub use mcp::*;
pub use model::{
    FrozenModelGateway, ModelCandidateV1, ModelDispatchEvidenceV1, ModelEventV1, ModelGateway,
    ModelProvider, ModelRequest, ModelRequestV1, ModelResolutionPlanV1, ModelResponse,
    ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
};
pub use normalize::{
    CapabilityOutcome, CapabilityOutcomeV1, DispatchEvidenceV1, EffectEvidenceV1,
    HostInvocationEventV1, InvocationNormalizer, NormalizeError, NormalizedContentV1,
    OutcomeDispositionV1, OutcomeKind, Redactor, RetrySafetyV1, StreamNormalizer,
    StreamingRedactor, TerminalEvidenceV1, classify_outcome,
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
