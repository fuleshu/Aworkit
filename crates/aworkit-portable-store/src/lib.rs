//! Portable, immutable Aworkit session history.
//!
//! This crate is deliberately a data repository: it accepts only already
//! scrubbed semantic facts, never executes imported data, and never selects
//! authority or capabilities.  Every published object is content addressed.

mod artifact;
mod codec;
mod commit;
mod export;
mod manifest;
mod projection;
mod rebind;
mod repository;
mod workspace;

pub use artifact::{ArtifactDescriptor, ArtifactStore};
pub use codec::{
    CanonicalCodec, CodecError, PortableCheckpoint, PortableEvent, PortableSegment, canonical_json,
    digest,
};
pub use commit::{CommitError, CommitReceipt, PortableCommit, PortableRepository};
pub use export::{ExportPolicy, OmissionFact, ScrubbedValue};
pub use manifest::{BranchManifest, BranchRef, RepositoryManifest, SessionManifest};
pub use projection::{ImportReport, PortablePage, ProjectionFeed};
pub use rebind::{CapabilityRequirement, RebindPlan, RetentionPlan, plan_rebind, retention_plan};
pub use repository::{PortableError, PortablePaths};
pub use workspace::{ProjectReference, WorkspaceRoot};
