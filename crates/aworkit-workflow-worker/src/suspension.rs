//! Exact suspension state plus core-fenced logical checkpoint recovery.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{
    ApprovalOutcomeV1, ProcessGeneration, RehydrationEnvelopeV1, StableId, WorkerCheckpointV1,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum SuspensionKindV1 {
    Input { input_id: StableId },
    Approval { approval_id: StableId },
    Paused { scope: String },
    Cancelling { scope: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuspensionFrameV1 {
    pub suspension_id: StableId,
    pub token_id: StableId,
    pub kind: SuspensionKindV1,
    pub resolved: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuspensionCheckpointV1 {
    pub frames: Vec<SuspensionFrameV1>,
    pub consumed_inputs: Vec<StableId>,
    pub consumed_approvals: Vec<StableId>,
    pub applied_controls: Vec<StableId>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SuspensionErrorV1 {
    #[error("suspension {0} already exists")]
    DuplicateSuspension(String),
    #[error("unknown suspension {0}")]
    UnknownSuspension(String),
    #[error("suspension has already been resolved")]
    AlreadyResolved,
    #[error("control payload does not match the suspension")]
    ControlMismatch,
    #[error("suspension checkpoint is invalid")]
    InvalidCheckpoint,
}

#[derive(Debug, Default)]
pub struct SuspensionControllerV1 {
    frames: BTreeMap<String, SuspensionFrameV1>,
    consumed_inputs: BTreeSet<String>,
    consumed_approvals: BTreeSet<String>,
    applied_controls: BTreeSet<String>,
}

impl SuspensionControllerV1 {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn suspend(&mut self, frame: SuspensionFrameV1) -> Result<(), SuspensionErrorV1> {
        if frame.resolved
            || self
                .frames
                .values()
                .any(|existing| !existing.resolved && existing.token_id == frame.token_id)
            || self.frames.contains_key(frame.suspension_id.as_str())
        {
            return Err(SuspensionErrorV1::DuplicateSuspension(
                frame.suspension_id.to_string(),
            ));
        }
        self.frames
            .insert(frame.suspension_id.as_str().to_owned(), frame);
        Ok(())
    }

    pub fn resolve_input(
        &mut self,
        suspension_id: &StableId,
        input_id: &StableId,
    ) -> Result<bool, SuspensionErrorV1> {
        if self.consumed_inputs.contains(input_id.as_str()) {
            return Ok(false);
        }
        let frame = self.frame_unresolved(suspension_id)?;
        if !matches!(&frame.kind, SuspensionKindV1::Input { input_id: expected } if expected == input_id)
        {
            return Err(SuspensionErrorV1::ControlMismatch);
        }
        frame.resolved = true;
        self.consumed_inputs.insert(input_id.as_str().to_owned());
        Ok(true)
    }

    pub fn resolve_approval(
        &mut self,
        suspension_id: &StableId,
        approval_id: &StableId,
        _outcome: ApprovalOutcomeV1,
    ) -> Result<bool, SuspensionErrorV1> {
        if self.consumed_approvals.contains(approval_id.as_str()) {
            return Ok(false);
        }
        let frame = self.frame_unresolved(suspension_id)?;
        if !matches!(&frame.kind, SuspensionKindV1::Approval { approval_id: expected } if expected == approval_id)
        {
            return Err(SuspensionErrorV1::ControlMismatch);
        }
        frame.resolved = true;
        self.consumed_approvals
            .insert(approval_id.as_str().to_owned());
        Ok(true)
    }

    pub fn apply_control(
        &mut self,
        control_id: &StableId,
        scope: &str,
        cancelling: bool,
    ) -> Result<bool, SuspensionErrorV1> {
        if scope.is_empty() || scope.len() > 512 {
            return Err(SuspensionErrorV1::ControlMismatch);
        }
        if !self.applied_controls.insert(control_id.as_str().to_owned()) {
            return Ok(false);
        }
        for frame in self.frames.values_mut().filter(|frame| !frame.resolved) {
            frame.kind = if cancelling {
                SuspensionKindV1::Cancelling {
                    scope: scope.to_owned(),
                }
            } else {
                SuspensionKindV1::Paused {
                    scope: scope.to_owned(),
                }
            };
        }
        Ok(true)
    }

    pub fn resume_pause(
        &mut self,
        control_id: &StableId,
        scope: &str,
    ) -> Result<Vec<StableId>, SuspensionErrorV1> {
        if scope.is_empty() || scope.len() > 512 {
            return Err(SuspensionErrorV1::ControlMismatch);
        }
        if !self.applied_controls.insert(control_id.as_str().to_owned()) {
            return Ok(Vec::new());
        }
        let mut resumed = Vec::new();
        for frame in self.frames.values_mut() {
            if matches!(&frame.kind, SuspensionKindV1::Paused { scope: current } if current == scope)
                && !frame.resolved
            {
                frame.resolved = true;
                resumed.push(frame.token_id.clone());
            }
        }
        Ok(resumed)
    }

    #[must_use]
    pub fn unresolved(&self) -> Vec<&SuspensionFrameV1> {
        self.frames
            .values()
            .filter(|frame| !frame.resolved)
            .collect()
    }

    #[must_use]
    pub fn checkpoint(&self) -> SuspensionCheckpointV1 {
        SuspensionCheckpointV1 {
            frames: self.frames.values().cloned().collect(),
            consumed_inputs: self
                .consumed_inputs
                .iter()
                .filter_map(|id| StableId::parse(id.clone()).ok())
                .collect(),
            consumed_approvals: self
                .consumed_approvals
                .iter()
                .filter_map(|id| StableId::parse(id.clone()).ok())
                .collect(),
            applied_controls: self
                .applied_controls
                .iter()
                .filter_map(|id| StableId::parse(id.clone()).ok())
                .collect(),
        }
    }

    pub fn restore(checkpoint: SuspensionCheckpointV1) -> Result<Self, SuspensionErrorV1> {
        if checkpoint.frames.len() > 10_000 {
            return Err(SuspensionErrorV1::InvalidCheckpoint);
        }
        let mut controller = Self::new();
        let mut unresolved_tokens = BTreeSet::new();
        for frame in checkpoint.frames {
            if (!frame.resolved && !unresolved_tokens.insert(frame.token_id.as_str().to_owned()))
                || controller.frames.contains_key(frame.suspension_id.as_str())
            {
                return Err(SuspensionErrorV1::InvalidCheckpoint);
            }
            if controller
                .frames
                .insert(frame.suspension_id.as_str().to_owned(), frame)
                .is_some()
            {
                return Err(SuspensionErrorV1::InvalidCheckpoint);
            }
        }
        controller.consumed_inputs = unique_ids(checkpoint.consumed_inputs)?;
        controller.consumed_approvals = unique_ids(checkpoint.consumed_approvals)?;
        controller.applied_controls = unique_ids(checkpoint.applied_controls)?;
        let resolved_inputs: BTreeSet<_> = controller
            .frames
            .values()
            .filter_map(|frame| match &frame.kind {
                SuspensionKindV1::Input { input_id } if frame.resolved => {
                    Some(input_id.as_str().to_owned())
                }
                _ => None,
            })
            .collect();
        let resolved_approvals: BTreeSet<_> = controller
            .frames
            .values()
            .filter_map(|frame| match &frame.kind {
                SuspensionKindV1::Approval { approval_id } if frame.resolved => {
                    Some(approval_id.as_str().to_owned())
                }
                _ => None,
            })
            .collect();
        if resolved_inputs != controller.consumed_inputs
            || resolved_approvals != controller.consumed_approvals
        {
            return Err(SuspensionErrorV1::InvalidCheckpoint);
        }
        Ok(controller)
    }

    fn frame_unresolved(
        &mut self,
        suspension_id: &StableId,
    ) -> Result<&mut SuspensionFrameV1, SuspensionErrorV1> {
        let frame = self
            .frames
            .get_mut(suspension_id.as_str())
            .ok_or_else(|| SuspensionErrorV1::UnknownSuspension(suspension_id.to_string()))?;
        if frame.resolved {
            return Err(SuspensionErrorV1::AlreadyResolved);
        }
        Ok(frame)
    }
}

fn unique_ids(ids: Vec<StableId>) -> Result<BTreeSet<String>, SuspensionErrorV1> {
    let expected = ids.len();
    let ids: BTreeSet<_> = ids.into_iter().map(|id| id.as_str().to_owned()).collect();
    if ids.len() != expected {
        return Err(SuspensionErrorV1::InvalidCheckpoint);
    }
    Ok(ids)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RehydratedStateV1 {
    pub checkpoint: WorkerCheckpointV1,
    pub committed_deltas: Vec<Value>,
    pub reconciled_outcome_ids: Vec<StableId>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RecoveryErrorV1 {
    #[error("rehydration snapshot hash is invalid")]
    SnapshotMismatch,
    #[error("rehydration plan hash is invalid")]
    PlanMismatch,
    #[error("checkpoint content hash is invalid")]
    CheckpointHashMismatch,
    #[error("replacement process generation must be newer")]
    GenerationFence,
    #[error("committed recovery deltas are not contiguous")]
    DeltaGap,
    #[error("reconciled capability outcomes conflict")]
    OutcomeConflict,
    #[error("rehydration payload exceeds its bound")]
    PayloadTooLarge,
    #[error("rehydration payload cannot be encoded")]
    Encoding,
}

/// Restores only durable logical state and already-reconciled outcomes. This
/// type intentionally exposes no invocation/proposal method, preventing crash
/// recovery from resending an uncertain external effect.
pub struct RehydratorV1;

impl RehydratorV1 {
    pub fn restore(envelope: RehydrationEnvelopeV1) -> Result<RehydratedStateV1, RecoveryErrorV1> {
        if envelope.committed_deltas.len() > 10_000 || envelope.reconciled_outcomes.len() > 10_000 {
            return Err(RecoveryErrorV1::PayloadTooLarge);
        }
        let calculated_snapshot = crate::plan::snapshot_content_hash(&envelope.snapshot)
            .map_err(|_| RecoveryErrorV1::Encoding)?;
        if envelope.snapshot.snapshot_hash != calculated_snapshot
            || envelope.checkpoint.snapshot_hash != calculated_snapshot
        {
            return Err(RecoveryErrorV1::SnapshotMismatch);
        }
        let plan =
            crate::plan::ExecutionPlanV1::compile(envelope.snapshot.clone(), &calculated_snapshot)
                .map_err(|_| RecoveryErrorV1::PlanMismatch)?;
        if envelope.checkpoint.plan_hash != plan.fingerprint() {
            return Err(RecoveryErrorV1::PlanMismatch);
        }
        if envelope.replacement_generation.0 <= envelope.checkpoint.prior_generation.0 {
            return Err(RecoveryErrorV1::GenerationFence);
        }
        if checkpoint_hash(&envelope.checkpoint)? != envelope.checkpoint.checkpoint_hash {
            return Err(RecoveryErrorV1::CheckpointHashMismatch);
        }
        validate_delta_cursors(
            envelope.checkpoint.committed_cursor,
            &envelope.committed_deltas,
        )?;
        let mut invocation_outcome = BTreeMap::<String, (String, Value)>::new();
        let mut outcome_ids = Vec::with_capacity(envelope.reconciled_outcomes.len());
        for outcome in &envelope.reconciled_outcomes {
            let current = (
                outcome.outcome_id.as_str().to_owned(),
                serde_json::to_value(outcome).map_err(|_| RecoveryErrorV1::Encoding)?,
            );
            if let Some(previous) = invocation_outcome
                .insert(outcome.invocation_id.as_str().to_owned(), current.clone())
                && previous != current
            {
                return Err(RecoveryErrorV1::OutcomeConflict);
            }
            outcome_ids.push(outcome.outcome_id.clone());
        }
        Ok(RehydratedStateV1 {
            checkpoint: envelope.checkpoint,
            committed_deltas: envelope.committed_deltas,
            reconciled_outcome_ids: outcome_ids,
        })
    }
}

pub fn checkpoint_hash(checkpoint: &WorkerCheckpointV1) -> Result<String, RecoveryErrorV1> {
    let mut canonical = checkpoint.clone();
    canonical.checkpoint_hash.clear();
    let bytes = serde_jcs::to_vec(&canonical).map_err(|_| RecoveryErrorV1::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_delta_cursors(start: u64, deltas: &[Value]) -> Result<(), RecoveryErrorV1> {
    let mut expected = start.saturating_add(1);
    for delta in deltas {
        let cursor = delta
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or(RecoveryErrorV1::DeltaGap)?;
        if cursor != expected {
            return Err(RecoveryErrorV1::DeltaGap);
        }
        expected = expected.saturating_add(1);
    }
    Ok(())
}
