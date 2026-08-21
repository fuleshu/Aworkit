//! Trusted-core domain services.
//!
//! The core owns durable lifecycle and authority decisions.  Its public types
//! are deliberately provider-neutral so the desktop, worker, and capability
//! host can only exchange Aworkit contracts.

mod desktop;
mod authority;
mod committer;
mod broker;
mod lifecycle;
mod project;
mod portable;
mod recovery;
mod secrets;
mod supervisor;

pub use desktop::{DesktopApi, DesktopCommand, DesktopEvent, DesktopReceipt, DesktopSnapshot};
pub use authority::{ApprovalRequirement, AuthorityManifest, CapabilityBinding, FrozenRunSnapshot, SnapshotFreezer, SnapshotRequest};
pub use committer::{CanonicalCommitter, CommitRequest, CoreCommitError, HistoryBinding};
pub use broker::{InvocationBroker, InvocationDecision, WorkerProposal};
pub use lifecycle::{ChatAggregate, ChatCommand, ChatEvent, ChatState, LifecycleError, WaitReason};
pub use project::{ProjectCoordinator, ProjectError, WorkspaceBinding, WorkspaceIdentity};
pub use portable::{PortableCommitGate, PortableGateError, PortableRecoveryFacts};
pub use recovery::{LocalRecovery, RecoveryError, RecoveryReport};
pub use secrets::{CredentialRef, SecretBroker, SecretError, SecretLease};
pub use supervisor::{WorkerControl, WorkerHandshake, WorkerSupervisor, WorkerSupervisorError};
