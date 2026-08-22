//! Repair policy and boundary errors.

use aworkit_protocol::StableId;
use thiserror::Error;

use super::super::{ActivationEligibilityV1, RepairAggregateError, RepairPortErrorV1};

/// Fail-closed repair policy or boundary failure.
#[derive(Debug, Error)]
pub enum RepairError {
    #[error(transparent)]
    CorruptLedger(#[from] RepairAggregateError),
    #[error("{boundary} failed: {source}")]
    Port {
        boundary: &'static str,
        #[source]
        source: RepairPortErrorV1,
    },
    #[error("repair ledger version mismatch: expected {expected}, observed {actual}")]
    StaleLedgerVersion { expected: u64, actual: u64 },
    #[error("recurring-error group has not been recorded")]
    GroupMissing,
    #[error("the requested candidate version is missing or no longer active")]
    CandidateMismatch,
    #[error("the requested investigation does not match the active Management investigation")]
    InvestigationMismatch,
    #[error("repair contract rejected the request: {0}")]
    InvalidContract(&'static str),
    #[error("activation is unavailable under eligibility {0:?}")]
    ActivationUnavailable(ActivationEligibilityV1),
    #[error("an enrollment or activation operation is already active")]
    OperationAlreadyActive,
    #[error("the expected bootstrap receipt is not available")]
    BootstrapResultMissing,
    #[error("the helper admitted activation, but current-generation cleanup was unsafe")]
    UnsafeQuiescence,
    #[error("repair artifact {artifact_id} is not ready: {reason}")]
    ArtifactNotReady {
        artifact_id: StableId,
        reason: &'static str,
    },
    #[error("repair operation id was previously committed with another payload")]
    OperationConflict,
}
