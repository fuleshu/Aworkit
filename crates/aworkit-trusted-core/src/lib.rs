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
    CredentialMetadataV1, CredentialReadAuthorizationV1, CredentialRef, CredentialSecretV1,
    MemoryCredentialStore, NativeCredentialStore, NativeCredentialStoreStatusV1,
    PlatformCredentialStorePort, RedeemLeaseRequestV1, ScopedLeaseRequestV1, SecretBroker,
    SecretDeliveryV1, SecretError, SecretLease, SecretLeaseAuditKindV1, SecretLeaseAuditV1,
};
pub use supervisor::{
    ProcessWorkerSupervisorV1, WorkerControl, WorkerHandshake, WorkerSupervisor,
    WorkerSupervisorError,
};
