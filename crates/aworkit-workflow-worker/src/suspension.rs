//! Exact suspension state plus core-fenced logical checkpoint recovery.

use aworkit_protocol::{ProcessGeneration, StableId};
use thiserror::Error;

use crate::{plan::ExecutionPlan, scheduler::Token};

/// A non-effecting point at which a worker may stop scheduling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Suspension {
    Input,
    Approval,
    Paused,
    Cancelling,
    Quiescent,
}
/// A worker-only state machine which is advanced only by explicitly named controls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuspensionController {
    state: Option<Suspension>,
    token: Option<Token>,
}
impl SuspensionController {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: None,
            token: None,
        }
    }
    pub fn suspend(&mut self, state: Suspension, token: Token) {
        self.state = Some(state);
        self.token = Some(token);
    }
    pub fn resume(&mut self) -> Option<Token> {
        self.state.take()?;
        self.token.take()
    }
    #[must_use]
    pub fn state(&self) -> Option<&Suspension> {
        self.state.as_ref()
    }
}
impl Default for SuspensionController {
    fn default() -> Self {
        Self::new()
    }
}

/// A proposed logical checkpoint; its cursor is valid only after a core acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub snapshot_hash: String,
    pub plan_fingerprint: String,
    pub generation: ProcessGeneration,
    pub committed_cursor: u64,
    pub tokens: Vec<Token>,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum RecoveryError {
    #[error("checkpoint snapshot does not match frozen plan")]
    SnapshotMismatch,
    #[error("checkpoint plan fingerprint does not match")]
    PlanMismatch,
    #[error("checkpoint generation is stale")]
    GenerationMismatch,
}
/// Rehydrates logical tokens only; it has no API to replay capability work.
pub struct Rehydrator;
impl Rehydrator {
    pub fn restore(
        plan: &ExecutionPlan,
        checkpoint: &Checkpoint,
        generation: ProcessGeneration,
    ) -> Result<Vec<Token>, RecoveryError> {
        if checkpoint.snapshot_hash != plan.snapshot().snapshot_hash {
            return Err(RecoveryError::SnapshotMismatch);
        }
        if checkpoint.plan_fingerprint != plan.fingerprint() {
            return Err(RecoveryError::PlanMismatch);
        }
        if checkpoint.generation != generation {
            return Err(RecoveryError::GenerationMismatch);
        }
        Ok(checkpoint.tokens.clone())
    }
    #[must_use]
    pub fn checkpoint(
        plan: &ExecutionPlan,
        generation: ProcessGeneration,
        cursor: u64,
        tokens: Vec<Token>,
    ) -> Checkpoint {
        Checkpoint {
            snapshot_hash: plan.snapshot().snapshot_hash.clone(),
            plan_fingerprint: plan.fingerprint().to_owned(),
            generation,
            committed_cursor: cursor,
            tokens,
        }
    }
}

/// A stable lifecycle event reference used by later IPC adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointProposal {
    pub checkpoint_id: StableId,
    pub checkpoint: Checkpoint,
}
