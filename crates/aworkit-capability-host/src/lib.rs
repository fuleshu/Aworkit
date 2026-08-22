//! Core-approved, generation-fenced capability execution.
mod files;
mod gateway;
mod materialize;
mod model;
mod normalize;
mod process;
mod registry;
mod tools;
pub use files::{
    FileAuthority, FileEditRequestV1, FileEditResultV1, FileEffectDescriptorV1, FileEffectKindV1,
    FileReadRequestV1, FileReadResultV1, FileSearchRequestV1, FileSearchResultV1, FileToolError,
    ProjectFiles,
};
pub use gateway::{
    AdmissionDispositionV1, AdmissionReceipt, ApprovedInvocation, ApprovedInvocationEnvelopeV1,
    CapabilityHost, HostError, InvocationResult,
};
pub use materialize::{
    InjectionTargetV1, RedeemLeaseRequestV1, SecretDeliveryV1, SecretFieldPlanV1,
    SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializationV1, SecretMaterializer,
};
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
pub use process::{
    CancellationToken, ControlledProcessResult, HermeticProcessPort, HermeticProcessStep,
    NativeProcessPort, PlatformProcessHealthV1, PlatformProcessPort, ProcessError, ProcessRequest,
    ProcessResult, ProcessRunner, ProcessSpecV1, ProcessTermination,
};
pub use registry::{
    AdapterDescriptor, AdapterRegistry, CapabilityDescriptor, CapabilityKind,
    FrozenAdapterRegistry, RegistryError, SideEffectClass,
};
pub use tools::{
    ArgumentVectorInvocationV1, BuiltInProcessTools, HostToolLimitsV1, PythonInvocationV1,
    ShellInvocationV1, ToolAdapterError, ToolAuthorityModeV1,
};
