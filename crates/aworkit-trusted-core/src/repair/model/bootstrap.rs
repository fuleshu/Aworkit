//! Managed-local eligibility, activation baton, and helper receipt records.
//!
//! The wire contracts are owned by `aworkit-protocol` so the independently
//! surviving helper never depends on the trusted-core implementation crate.

pub use aworkit_protocol::{
    ActivationEligibilityV1, AuthenticatedBootstrapResultV1, BootstrapAcceptedAdmissionV1,
    BootstrapAdmissionV1, BootstrapDeadlinesV1, BootstrapPeerProofV1, BootstrapResultKindV1,
    BootstrapResultV1, BuildOriginV1, CoreQuiescenceFactsV1, EnrollmentPreparedV1,
    IntegrityStrengthV1, ManagedLocalEnrollmentRequestV1, ManagementCheckpointRefV1,
    ManualRecoveryNoticeV1, PlatformCapabilityReportV1, PlatformReasonV1, ReasonCodeV1,
    RepairActivationBatonV1, RepairActivationDecisionV1, RepairCandidateDecisionV1,
    RepairCandidateDispositionV1,
};
