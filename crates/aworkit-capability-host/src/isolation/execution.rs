//! Isolated command, bounded transfer, terminal, and cleanup evidence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::contract::{
    BackendExecutionLocationV1, EnforcementReportV1, content_hash_v1, is_bounded_evidence,
    is_bounded_identity, is_content_hash,
};

const MAX_ARGUMENTS: usize = 4_096;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 1_024;
const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;

/// Upper bounds for every channel crossing the isolation boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferLimitsV1 {
    pub maximum_input_bytes: usize,
    pub maximum_event_count: usize,
    pub maximum_event_bytes: usize,
    pub maximum_stream_bytes: usize,
    pub maximum_result_bytes: usize,
    pub maximum_artifact_count: usize,
    pub maximum_artifact_bytes: usize,
    pub maximum_total_artifact_bytes: usize,
}

impl TransferLimitsV1 {
    pub(crate) fn validate(&self, backend_maximum: usize) -> bool {
        self.maximum_input_bytes > 0
            && self.maximum_event_count > 0
            && self.maximum_event_bytes > 0
            && self.maximum_stream_bytes > 0
            && self.maximum_result_bytes > 0
            && self.maximum_artifact_count > 0
            && self.maximum_artifact_bytes > 0
            && self.maximum_total_artifact_bytes > 0
            && self.maximum_input_bytes <= backend_maximum
            && self.maximum_event_bytes <= backend_maximum
            && self.maximum_stream_bytes <= backend_maximum
            && self.maximum_result_bytes <= backend_maximum
            && self.maximum_artifact_bytes <= backend_maximum
            && self.maximum_total_artifact_bytes <= backend_maximum
            && self.maximum_artifact_bytes <= self.maximum_total_artifact_bytes
    }
}

/// Argv-only command passed to an already verified environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedCommandV1 {
    pub program: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
}

impl IsolatedCommandV1 {
    pub(crate) fn validate(&self) -> bool {
        is_bounded_identity(&self.program)
            && is_bounded_identity(&self.working_directory)
            && self.arguments.len() <= MAX_ARGUMENTS
            && self.arguments.iter().map(String::len).sum::<usize>() <= MAX_ARGUMENT_BYTES
            && self.arguments.iter().all(|value| !value.contains('\0'))
            && self.environment.len() <= MAX_ENVIRONMENT_ENTRIES
            && self
                .environment
                .iter()
                .map(|(key, value)| key.len().saturating_add(value.len()))
                .sum::<usize>()
                <= MAX_ENVIRONMENT_BYTES
            && self.environment.iter().all(|(key, value)| {
                !key.is_empty()
                    && !key.contains(['=', '\0'])
                    && !value.contains('\0')
                    && key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
    }
}

/// One invocation pinned to one profile, workspace, deadline, and input hash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedExecutionV1 {
    pub invocation_id: String,
    pub profile_id: String,
    pub profile_hash: String,
    pub workspace_id: String,
    pub deadline_epoch_millis: u64,
    pub command: IsolatedCommandV1,
    pub input: Vec<u8>,
    pub input_hash: String,
    pub transfer_limits: TransferLimitsV1,
}

impl IsolatedExecutionV1 {
    pub(crate) fn validate(&self, backend_maximum: usize) -> bool {
        is_bounded_identity(&self.invocation_id)
            && is_bounded_identity(&self.profile_id)
            && is_content_hash(&self.profile_hash)
            && is_bounded_identity(&self.workspace_id)
            && self.deadline_epoch_millis > 0
            && self.command.validate()
            && self.transfer_limits.validate(backend_maximum)
            && self.input.len() <= self.transfer_limits.maximum_input_bytes
            && is_content_hash(&self.input_hash)
            && content_hash_v1(&self.input) == self.input_hash
    }
}

/// One content-addressed artifact returned from the environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactTransferV1 {
    pub correlation_id: String,
    pub relative_path: String,
    pub media_type: String,
    pub content_hash: String,
    pub content: Vec<u8>,
}

impl ArtifactTransferV1 {
    pub(crate) fn has_valid_identity_and_hash(&self) -> bool {
        is_bounded_identity(&self.correlation_id)
            && is_safe_relative_path(&self.relative_path)
            && is_bounded_identity(&self.media_type)
            && is_content_hash(&self.content_hash)
            && content_hash_v1(&self.content) == self.content_hash
    }
}

/// Single bounded terminal result payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoundedResultTransferV1 {
    pub media_type: String,
    pub content_hash: String,
    pub content: Vec<u8>,
}

impl BoundedResultTransferV1 {
    pub(crate) fn has_valid_identity_and_hash(&self) -> bool {
        is_bounded_identity(&self.media_type)
            && is_content_hash(&self.content_hash)
            && content_hash_v1(&self.content) == self.content_hash
    }
}

/// Raw backend events remain bounded and source-labelled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum IsolationRawEventV1 {
    DispatchAccepted { receipt: String },
    Progress(String),
    StandardOutput(Vec<u8>),
    StandardError(Vec<u8>),
    Artifact(ArtifactTransferV1),
    Result(BoundedResultTransferV1),
}

impl IsolationRawEventV1 {
    pub(crate) fn payload_bytes(&self) -> usize {
        match self {
            Self::DispatchAccepted { receipt } | Self::Progress(receipt) => receipt.len(),
            Self::StandardOutput(bytes) | Self::StandardError(bytes) => bytes.len(),
            Self::Artifact(artifact) => artifact.content.len(),
            Self::Result(result) => result.content.len(),
        }
    }
}

/// Whether a backend accepted work before a terminal or transport failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendDispatchV1 {
    DefinitelyNotDispatched,
    Accepted,
    Unknown,
}

/// Cancellation evidence never implies that already performed effects vanished.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancellationEvidenceV1 {
    pub requested: bool,
    pub backend_acknowledged: bool,
    pub terminal_confirmed: bool,
    pub evidence: String,
}

impl CancellationEvidenceV1 {
    pub(crate) fn validate(&self) -> bool {
        self.requested
            && (!self.terminal_confirmed || self.backend_acknowledged)
            && is_bounded_evidence(&self.evidence)
    }
}

/// Exact terminal fact returned by a backend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BackendTerminalV1 {
    Exited {
        exit_code: i32,
    },
    Rejected {
        reason: String,
    },
    Cancelled {
        dispatch: BackendDispatchV1,
        cancellation: CancellationEvidenceV1,
    },
    DeadlineExceeded {
        dispatch: BackendDispatchV1,
        terminal_confirmed: bool,
        evidence: String,
    },
    RemoteLost {
        dispatch: BackendDispatchV1,
        detail: String,
    },
    BackendFailed {
        dispatch: BackendDispatchV1,
        detail: String,
    },
}

impl BackendTerminalV1 {
    pub(crate) fn validate(&self) -> bool {
        match self {
            Self::Exited { .. } => true,
            Self::Rejected { reason }
            | Self::RemoteLost { detail: reason, .. }
            | Self::BackendFailed { detail: reason, .. } => is_bounded_evidence(reason),
            Self::Cancelled { cancellation, .. } => cancellation.validate(),
            Self::DeadlineExceeded { evidence, .. } => is_bounded_evidence(evidence),
        }
    }

    #[must_use]
    pub fn execution_outcome(&self) -> IsolationOutcomeV1 {
        match self {
            Self::Exited { exit_code: 0 } => IsolationOutcomeV1::Completed,
            Self::Exited { .. } => IsolationOutcomeV1::Failed,
            Self::Rejected { .. } => IsolationOutcomeV1::DefinitelyNotStarted,
            Self::Cancelled {
                cancellation,
                dispatch,
            } if cancellation.terminal_confirmed && *dispatch != BackendDispatchV1::Unknown => {
                IsolationOutcomeV1::Cancelled
            }
            Self::DeadlineExceeded {
                dispatch,
                terminal_confirmed: true,
                ..
            } if *dispatch != BackendDispatchV1::Unknown => IsolationOutcomeV1::TimedOut,
            Self::RemoteLost {
                dispatch: BackendDispatchV1::DefinitelyNotDispatched,
                ..
            }
            | Self::BackendFailed {
                dispatch: BackendDispatchV1::DefinitelyNotDispatched,
                ..
            } => IsolationOutcomeV1::DefinitelyNotStarted,
            Self::Cancelled { .. }
            | Self::DeadlineExceeded { .. }
            | Self::RemoteLost { .. }
            | Self::BackendFailed { .. } => IsolationOutcomeV1::OutcomeUncertain,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStageV1 {
    Verification,
    Dispatch,
    Execution,
}

/// Backend failures carry dispatch certainty so fallback/retry stays safe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendExecutionFailureV1 {
    pub stage: BackendStageV1,
    pub dispatch: BackendDispatchV1,
    pub detail: String,
}

/// Cleanup status for each independently meaningful lifecycle fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupVerificationV1 {
    Verified,
    Unverified,
    Failed,
    NotApplicable,
}

/// Post-session cleanup evidence. It is preserved even when execution fails.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolationCleanupV1 {
    pub session_id: String,
    pub process_tree_terminated: CleanupVerificationV1,
    pub environment_state_removed: CleanupVerificationV1,
    pub remote_session_closed: CleanupVerificationV1,
    pub evidence: String,
}

impl IsolationCleanupV1 {
    #[must_use]
    pub fn is_verified_for(&self, session_id: &str, location: BackendExecutionLocationV1) -> bool {
        self.session_id == session_id
            && self.process_tree_terminated == CleanupVerificationV1::Verified
            && self.environment_state_removed == CleanupVerificationV1::Verified
            && match location {
                BackendExecutionLocationV1::Local => matches!(
                    self.remote_session_closed,
                    CleanupVerificationV1::Verified | CleanupVerificationV1::NotApplicable
                ),
                BackendExecutionLocationV1::Remote => {
                    self.remote_session_closed == CleanupVerificationV1::Verified
                }
            }
            && is_bounded_evidence(&self.evidence)
    }
}

/// Outcome of execution or its conservative lifecycle combination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationOutcomeV1 {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    DefinitelyNotStarted,
    OutcomeUncertain,
}

/// This value is constructed only after exact enforcement verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationStrengthV1 {
    VerifiedSecurityBoundary,
}

/// Complete noncanonical run evidence returned to the invocation pipeline.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolationRunReportV1 {
    pub invocation_id: String,
    pub strength: IsolationStrengthV1,
    pub enforcement: EnforcementReportV1,
    pub events: Vec<IsolationRawEventV1>,
    pub terminal: BackendTerminalV1,
    pub execution_outcome: IsolationOutcomeV1,
    pub overall_outcome: IsolationOutcomeV1,
    pub cleanup: IsolationCleanupV1,
    pub contract_violation: Option<IsolationEventErrorV1>,
}

/// A bounded stream or content-integrity violation after dispatch.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IsolationEventErrorV1 {
    #[error("isolation event count exceeded its bound")]
    EventCountExceeded,
    #[error("one isolation event exceeded its byte bound")]
    EventBytesExceeded,
    #[error("isolation stream exceeded its aggregate byte bound")]
    StreamBytesExceeded,
    #[error("more than one result payload was returned")]
    DuplicateResult,
    #[error("result transfer exceeded its byte bound")]
    ResultBytesExceeded,
    #[error("result transfer hash or identity is invalid")]
    ResultIntegrity,
    #[error("artifact count exceeded its bound")]
    ArtifactCountExceeded,
    #[error("one artifact exceeded its byte bound")]
    ArtifactBytesExceeded,
    #[error("aggregate artifact transfer exceeded its byte bound")]
    AggregateArtifactBytesExceeded,
    #[error("artifact path, identity, or content hash is invalid")]
    ArtifactIntegrity,
    #[error("backend event metadata is malformed")]
    MalformedMetadata,
}

fn is_safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && !value.starts_with(['/', '\\'])
        && !value.contains('\0')
        && value
            .split(['/', '\\'])
            .all(|component| !component.is_empty() && component != "." && component != "..")
}
