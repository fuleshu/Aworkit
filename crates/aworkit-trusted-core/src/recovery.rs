//! Committed-fact recovery and replacement-worker rehydration construction.

use std::{collections::BTreeSet, sync::Arc};

use aworkit_protocol::{
    CapabilityOutcomeV1, ProcessGeneration, RehydrationEnvelopeV1, StableId, WorkerCheckpointV1,
    WorkerFrozenRunSnapshotV1,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::lifecycle::{CommittedRunEventV1, RunAggregateV1};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    pub head_sequence: u64,
    pub terminal: bool,
    pub last_checkpoint_hash: Option<String>,
    pub pending_delivery_count: usize,
    pub effect_replay_required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryEventV1 {
    pub sequence: u64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryFactsV1 {
    pub snapshot: WorkerFrozenRunSnapshotV1,
    pub checkpoint: Option<WorkerCheckpointV1>,
    /// Canonical lifecycle stream used to rebuild the one-Chat/one-Run
    /// aggregate. Recovery never trusts a serialized mutable aggregate.
    pub lifecycle_events: Vec<CommittedRunEventV1>,
    pub events: Vec<RecoveryEventV1>,
    pub committed_deltas: Vec<Value>,
    pub reconciled_outcomes: Vec<CapabilityOutcomeV1>,
    pub uncertain_invocation_ids: Vec<StableId>,
    pub pending_delivery_count: usize,
    pub prior_generation: ProcessGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryPortErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub inspectable_read_only: bool,
}

/// Read-only process-neutral history boundary. It cannot commit, dispatch, or
/// acknowledge an invocation, which keeps recovery incapable of effect replay.
pub trait RecoveryHistoryPort: Send + Sync {
    fn load_recovery_facts(
        &self,
        chat_id: &StableId,
        branch_id: &StableId,
    ) -> Result<RecoveryFactsV1, RecoveryPortErrorV1>;
}

#[derive(Clone)]
pub struct LocalRecovery {
    port: Arc<dyn RecoveryHistoryPort>,
}

impl LocalRecovery {
    #[must_use]
    pub fn new(port: impl RecoveryHistoryPort + 'static) -> Self {
        Self {
            port: Arc::new(port),
        }
    }

    /// Compatibility report over the same strict committed-fact fold.
    pub fn recover(&self, chat_id: &str, branch_id: &str) -> Result<RecoveryReport, RecoveryError> {
        let chat_id = StableId::parse(chat_id.to_owned()).map_err(|_| RecoveryError::InvalidId)?;
        let branch_id =
            StableId::parse(branch_id.to_owned()).map_err(|_| RecoveryError::InvalidId)?;
        let facts = self.port.load_recovery_facts(&chat_id, &branch_id)?;
        validate_event_history(&facts.events)?;
        let aggregate = RunAggregateV1::fold(
            facts.snapshot.chat_id.clone(),
            facts.snapshot.run_id.clone(),
            &facts.lifecycle_events,
        )
        .map_err(|_| RecoveryError::CorruptAggregate)?;
        if facts.snapshot.chat_id != chat_id
            || aggregate.snapshot_hash.as_deref() != Some(facts.snapshot.snapshot_hash.as_str())
        {
            return Err(RecoveryError::IdentityMismatch);
        }
        Ok(RecoveryReport {
            head_sequence: facts.events.last().map_or(0, |event| event.sequence),
            terminal: aggregate.state.is_terminal(),
            last_checkpoint_hash: facts
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.checkpoint_hash.clone()),
            pending_delivery_count: facts.pending_delivery_count,
            effect_replay_required: false,
        })
    }

    pub fn recover_v1(
        &self,
        chat_id: &StableId,
        branch_id: &StableId,
    ) -> Result<RecoveryDecisionV1, RecoveryError> {
        let facts = self.port.load_recovery_facts(chat_id, branch_id)?;
        if facts.snapshot.chat_id != *chat_id {
            return Err(RecoveryError::IdentityMismatch);
        }
        validate_event_history(&facts.events)?;
        let aggregate = RunAggregateV1::fold(
            facts.snapshot.chat_id.clone(),
            facts.snapshot.run_id.clone(),
            &facts.lifecycle_events,
        )
        .map_err(|_| RecoveryError::CorruptAggregate)?;
        if aggregate.snapshot_hash.as_deref() != Some(facts.snapshot.snapshot_hash.as_str()) {
            return Err(RecoveryError::IdentityMismatch);
        }
        if aggregate.state.is_terminal() {
            return Ok(RecoveryDecisionV1::Terminal {
                aggregate,
                report: RecoveryReport {
                    head_sequence: facts.events.last().map_or(0, |event| event.sequence),
                    terminal: true,
                    last_checkpoint_hash: facts
                        .checkpoint
                        .as_ref()
                        .map(|checkpoint| checkpoint.checkpoint_hash.clone()),
                    pending_delivery_count: facts.pending_delivery_count,
                    effect_replay_required: false,
                },
            });
        }
        let uncertain: BTreeSet<_> = facts
            .uncertain_invocation_ids
            .iter()
            .map(StableId::as_str)
            .collect();
        if uncertain.len() != facts.uncertain_invocation_ids.len() {
            return Err(RecoveryError::ConflictingOutcome);
        }
        let aggregate_uncertain: BTreeSet<_> = aggregate
            .uncertain_invocations
            .iter()
            .map(String::as_str)
            .collect();
        if uncertain != aggregate_uncertain {
            return Err(RecoveryError::ConflictingOutcome);
        }
        if !facts.uncertain_invocation_ids.is_empty() {
            return Ok(RecoveryDecisionV1::Blocked {
                aggregate,
                uncertain_invocation_ids: facts.uncertain_invocation_ids,
                pending_delivery_count: facts.pending_delivery_count,
            });
        }
        let checkpoint = facts.checkpoint.ok_or(RecoveryError::MissingCheckpoint)?;
        validate_checkpoint(&facts.snapshot, &checkpoint, facts.prior_generation)?;
        validate_deltas(checkpoint.committed_cursor, &facts.committed_deltas)?;
        let mut outcome_invocations = BTreeSet::new();
        let mut outcome_ids = BTreeSet::new();
        for outcome in &facts.reconciled_outcomes {
            if !outcome_invocations.insert(outcome.invocation_id.as_str())
                || !outcome_ids.insert(outcome.outcome_id.as_str())
            {
                return Err(RecoveryError::ConflictingOutcome);
            }
        }
        let replacement_generation = ProcessGeneration(
            facts
                .prior_generation
                .0
                .checked_add(1)
                .ok_or(RecoveryError::GenerationExhausted)?,
        );
        Ok(RecoveryDecisionV1::SpawnReplacement {
            aggregate,
            envelope: RehydrationEnvelopeV1 {
                snapshot: facts.snapshot,
                checkpoint,
                replacement_generation,
                committed_deltas: facts.committed_deltas,
                reconciled_outcomes: facts.reconciled_outcomes,
            },
            pending_delivery_count: facts.pending_delivery_count,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryDecisionV1 {
    Terminal {
        aggregate: RunAggregateV1,
        report: RecoveryReport,
    },
    Blocked {
        aggregate: RunAggregateV1,
        uncertain_invocation_ids: Vec<StableId>,
        pending_delivery_count: usize,
    },
    SpawnReplacement {
        aggregate: RunAggregateV1,
        envelope: RehydrationEnvelopeV1,
        pending_delivery_count: usize,
    },
}

fn validate_event_history(events: &[RecoveryEventV1]) -> Result<(), RecoveryError> {
    if events.len() > 10_000_000 {
        return Err(RecoveryError::HistoryTooLong);
    }
    for (index, event) in events.iter().enumerate() {
        let expected = u64::try_from(index)
            .map_err(|_| RecoveryError::HistoryTooLong)?
            .saturating_add(1);
        if event.sequence != expected || event.kind.trim().is_empty() || event.kind.len() > 256 {
            return Err(RecoveryError::HistoryGap);
        }
    }
    Ok(())
}

fn validate_checkpoint(
    snapshot: &WorkerFrozenRunSnapshotV1,
    checkpoint: &WorkerCheckpointV1,
    prior_generation: ProcessGeneration,
) -> Result<(), RecoveryError> {
    let calculated_snapshot =
        crate::authority::snapshot_hash_v1(snapshot).map_err(|_| RecoveryError::Encoding)?;
    if snapshot.snapshot_hash != calculated_snapshot
        || checkpoint.snapshot_hash != snapshot.snapshot_hash
        || checkpoint.prior_generation != prior_generation
        || !is_sha256(&checkpoint.plan_hash)
        || !is_sha256(&checkpoint.checkpoint_hash)
    {
        return Err(RecoveryError::CheckpointMismatch);
    }
    let mut canonical = checkpoint.clone();
    canonical.checkpoint_hash.clear();
    let bytes = serde_jcs::to_vec(&canonical).map_err(|_| RecoveryError::Encoding)?;
    let calculated = format!("{:x}", Sha256::digest(bytes));
    if calculated != checkpoint.checkpoint_hash {
        return Err(RecoveryError::CheckpointMismatch);
    }
    Ok(())
}

fn validate_deltas(start: u64, deltas: &[Value]) -> Result<(), RecoveryError> {
    if deltas.len() > 100_000 {
        return Err(RecoveryError::HistoryTooLong);
    }
    let mut expected = start.saturating_add(1);
    for delta in deltas {
        if delta.get("cursor").and_then(Value::as_u64) != Some(expected) {
            return Err(RecoveryError::HistoryGap);
        }
        expected = expected.saturating_add(1);
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("recovery history port failed: {code}: {message}")]
    Port {
        code: String,
        message: String,
        retryable: bool,
        inspectable_read_only: bool,
    },
    #[error("recovery identity is invalid")]
    InvalidId,
    #[error("recovery facts do not belong to the requested Chat")]
    IdentityMismatch,
    #[error("committed lifecycle history cannot rebuild the Run aggregate")]
    CorruptAggregate,
    #[error("committed history has a sequence gap or malformed event")]
    HistoryGap,
    #[error("the local history has too many events to represent its sequence")]
    HistoryTooLong,
    #[error("a nonterminal Run has no durable logical checkpoint")]
    MissingCheckpoint,
    #[error("checkpoint identity, generation, or content hash is invalid")]
    CheckpointMismatch,
    #[error("reconciled capability outcomes conflict")]
    ConflictingOutcome,
    #[error("replacement worker generation is exhausted")]
    GenerationExhausted,
    #[error("recovery facts could not be canonically encoded")]
    Encoding,
}

impl From<RecoveryPortErrorV1> for RecoveryError {
    fn from(error: RecoveryPortErrorV1) -> Self {
        Self::Port {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
            inspectable_read_only: error.inspectable_read_only,
        }
    }
}
