//! Trusted-core domain services.
//!
//! The core owns durable lifecycle and authority decisions.  Its public types
//! are deliberately provider-neutral so the desktop, worker, and capability
//! host can only exchange Aworkit contracts.

mod authority;
mod broker;
mod committer;
mod desktop;
mod extensions;
mod lifecycle;
mod portable;
mod project;
mod recovery;
pub mod repair;
mod secrets;
mod supervisor;

pub use authority::{
    ApprovalDecisionV1, ApprovalEngineV1, ApprovalGrantV1, ApprovalRequirement, AuthorityManifest,
    AuthorityManifestV1, CapabilityBinding, CapabilityBindingV1, FrozenRunSnapshot, SnapshotError,
    SnapshotErrorV1, SnapshotFreezer, SnapshotFreezerV1, SnapshotRequest, SnapshotRequestV1,
    snapshot_hash_v1, workflow_graph_hash_v1,
};
pub use broker::{
    ApprovalChallengeV1, ApprovalResponseV1, ApprovedDispatchV1, ApprovedHostDispatchPortV1,
    BrokerDecisionV1, BrokerError, CommittedWorkerResultPortV1, DeliveryAcceptanceV1,
    DispatchOutboxV1, DurableInvocationBroker, InvocationBroker, InvocationDecision,
    InvocationLeasePortV1, InvocationLedgerEventV1, InvocationLedgerPortV1, MemoryInvocationLedger,
    WorkerInvocationProposalV1, WorkerProposal, WorkerResultOutboxV1,
};
pub use committer::{
    CanonicalCommitOutcomeV1, CanonicalCommitRequestV1, CanonicalCommitter, CommitRequest,
    CoreCommitError, HistoryBinding,
};
pub use desktop::{
    CoreServiceRequestKindV1, CoreServiceRequestV1, CoreServiceResponseKindV1,
    CoreServiceResponseV1, DesktopApi, DesktopApiError, DesktopCommand, DesktopEvent,
    DesktopReceipt, DesktopSnapshot, DesktopTransactionV1, serve_core_stdio,
};
pub use extensions::{ExtensionRegistry, ExtensionRegistryError};
pub use lifecycle::{
    AttemptStateV1, ChatAggregate, ChatCommand, ChatEvent, ChatState, CommittedRunEventV1,
    LifecycleError, LifecycleErrorV1, RunAggregateV1, RunCommandKindV1, RunCommandOutcomeV1,
    RunCommandV1, RunEventKindV1, RunStateV1, WaitReason,
};
pub use portable::{PortableCommitGate, PortableGateError, PortableRecoveryFacts};
pub use project::{
    DocumentWatchResultV1, ProjectCoordinator, ProjectDocumentKindV1, ProjectDocumentPort,
    ProjectDocumentV1, ProjectError, ProjectPortErrorV1, ProjectRecordV1, StoredProjectDocumentV1,
    WorkspaceBinding, WorkspaceBindingV1, WorkspaceIdentity, WorkspaceIdentityV1,
};
pub use recovery::{
    LocalRecovery, RecoveryDecisionV1, RecoveryError, RecoveryEventV1, RecoveryFactsV1,
    RecoveryHistoryPort, RecoveryPortErrorV1, RecoveryReport,
};
pub use repair::*;
pub use secrets::{
    CredentialMetadataV1, CredentialRef, CredentialSecretV1, MemoryCredentialStore,
    PlatformCredentialStorePort, RedeemLeaseRequestV1, ScopedLeaseRequestV1, SecretBroker,
    SecretDeliveryV1, SecretError, SecretLease, SecretLeaseAuditKindV1, SecretLeaseAuditV1,
};
pub use supervisor::{
    ProcessWorkerSupervisorV1, WorkerControl, WorkerHandshake, WorkerSupervisor,
    WorkerSupervisorError,
};
