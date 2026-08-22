//! Optional verified-isolation contracts and orchestration.
//!
//! An isolation backend is never an implicit fallback target. Callers pass a
//! core-pinned backend and profile, and the runtime either verifies that exact
//! realization or returns evidence that isolated execution did not proceed.

mod contract;
mod execution;
mod gateway;
mod hermetic;
mod runtime;

pub use contract::{
    BackendAvailabilityV1, BackendExecutionLocationV1, BackendUnavailableReasonV1,
    EnforcementCategoryV1, EnforcementClaimV1, EnforcementRealizationV1, EnforcementReportV1,
    EnforcementVerificationV1, IsolationBackendManifestV1, IsolationProfileV1,
    IsolationRequirementV1, MountAccessV1, MountRealizationV1, NetworkPolicyV1,
    PinnedBackendIdentityV1, ProcessLimitsV1, ResidualStatePolicyV1, ResourceLimitsV1,
    UserPolicyV1, content_hash_v1,
};
pub use execution::{
    ArtifactTransferV1, BackendDispatchV1, BackendExecutionFailureV1, BackendStageV1,
    BackendTerminalV1, BoundedResultTransferV1, CancellationEvidenceV1, CleanupVerificationV1,
    IsolatedCommandV1, IsolatedExecutionV1, IsolationCleanupV1, IsolationEventErrorV1,
    IsolationOutcomeV1, IsolationRawEventV1, IsolationRunReportV1, IsolationStrengthV1,
    TransferLimitsV1,
};
pub use gateway::{
    IsolationGatewayDispatchErrorV1, IsolationGatewayDispatcherV1, IsolationGatewayRequestV1,
};
pub use hermetic::{
    HermeticCleanupV1, HermeticIsolationBackend, HermeticIsolationError,
    HermeticIsolationObservationV1, HermeticIsolationRunV1, HermeticVerificationV1,
};
pub use runtime::{IsolationBackendPortV1, IsolationRuntime, IsolationRuntimeError};
