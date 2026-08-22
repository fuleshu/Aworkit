//! Portable, immutable Aworkit session history.
//!
//! This crate is deliberately a data repository: it accepts only already
//! scrubbed semantic facts, never executes imported data, and never selects
//! authority or capabilities.  Every published object is content addressed.

mod artifact;
mod codec;
mod commit;
mod export;
mod integrity;
mod manifest;
mod port;
mod projection;
mod rebind;
mod repository;
mod workspace;

pub use artifact::{ArtifactDescriptor, ArtifactError, ArtifactStore, MAX_PORTABLE_ARTIFACT_BYTES};
pub use codec::{
    CanonicalCodec, CodecError, PortableCapabilityRequirementV1, PortableCheckpoint,
    PortableCommitContextV1, PortableEvent, PortableFrozenSnapshotV1, PortableGitFactsV1,
    PortableProvenanceV1, PortableSegment, PortableTransitionRecordV1, canonical_json, digest,
    portable_snapshot_hash, validate_checkpoint_record, validate_context,
};
pub use commit::{
    CommitError, CommitFaultPoint, CommitReceipt, PortableCommit, PortableRepository,
    PreparedCommit,
};
pub use export::{ExportError, ExportPolicy, OmissionFact, PortableRecordClass, ScrubbedValue};
pub use integrity::{
    IntegrityError, IntegrityIssueV1, IntegrityReportV1, NonDestructiveRepairProposalV1,
    PortableIntegrityEngine,
};
pub use manifest::{
    BranchManifest, BranchRef, ChildContinuationManifestReceiptV1,
    ChildContinuationManifestRequestV1, ManifestCatalog, ManifestEnvelopeV1, ManifestError,
    RepositoryCompatibility, RepositoryManifest, SessionManifest,
};
pub use port::protocol_value_hash;
pub use projection::{ImportReport, PortablePage, PortableProjectionEvidenceV1, ProjectionFeed};
pub use rebind::{
    CapabilityRequirement, ChildContinuationPlanV1, ContinuationRebindPlanV1, LocalCapabilityV1,
    ReachabilityScanV1, RebindPlan, RebindResolutionV1, RetentionError, RetentionPlan,
    plan_child_continuation, plan_continuation_rebind, plan_rebind, retention_plan,
    retention_plan_two_phase,
};
pub use repository::{PortableError, PortablePaths};
pub use workspace::{
    GitFactAvailabilityV1, GitWorktreeFactsV1, ProjectReference, WorkspaceError, WorkspaceRoot,
};
