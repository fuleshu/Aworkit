//! Frozen attempt policy and conservative side-effect retry decisions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Compatibility outcome classification supplied by a core-mediated result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectOutcome {
    Success,
    DefiniteNotStarted,
    /// A failure after dispatch. It is not automatically retry-safe.
    Failed,
    OutcomeUncertain,
}

/// Compatibility scheduler action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttemptDecision {
    Continue,
    Retry,
    Fallback(String),
    WaitForApproval,
    TerminalFailure,
}

/// Compact compatibility policy declared by old plan fixtures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptPolicy {
    pub max_retries: u32,
    pub fallback: Option<String>,
    pub requires_approval: bool,
}

impl AttemptPolicy {
    /// Never repeats an uncertain or known-started effect. The richer V1 policy
    /// below can admit a same-invocation reconciliation retry only with proof.
    #[must_use]
    pub fn decide(&self, attempt: u32, outcome: EffectOutcome) -> AttemptDecision {
        match outcome {
            EffectOutcome::Success => AttemptDecision::Continue,
            EffectOutcome::OutcomeUncertain => AttemptDecision::WaitForApproval,
            EffectOutcome::DefiniteNotStarted if attempt < self.max_retries => {
                AttemptDecision::Retry
            }
            EffectOutcome::DefiniteNotStarted | EffectOutcome::Failed => self
                .fallback
                .clone()
                .map_or(AttemptDecision::TerminalFailure, AttemptDecision::Fallback),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    Succeeded,
    DefiniteNotStarted,
    ContractFailure,
    FailedKnownStarted,
    CancelledWithEvidence,
    OutcomeUncertain,
    Denied,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryProof {
    pub invocation_id: String,
    pub descriptor_idempotent: bool,
    pub same_id_deduplicated: bool,
    pub effect_absence_proven: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptPolicyV1 {
    pub policy_id: String,
    pub max_attempts: u32,
    pub fallback_node_id: Option<String>,
    pub feedback_transition_id: Option<String>,
    pub evaluator_transition_id: Option<String>,
    pub approval_gate_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptInputV1 {
    pub operation_id: String,
    pub attempt_id: String,
    pub attempt_ordinal: u32,
    pub outcome: OutcomeClass,
    pub retry_proof: Option<RetryProof>,
    pub evaluator_passed: Option<bool>,
    pub gate_passed: Option<bool>,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AttemptDecisionV1 {
    Complete,
    RetryWithNewAttempt { next_ordinal: u32 },
    ReconcileSameInvocation { invocation_id: String },
    FollowExistingEdge { transition_id: String },
    SelectExistingFallback { node_id: String },
    AwaitApproval { gate_id: String },
    RequireUserDecision,
    Fail { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptRecordV1 {
    pub operation_id: String,
    pub attempt_id: String,
    pub attempt_ordinal: u32,
    pub outcome: OutcomeClass,
    pub decision: AttemptDecisionV1,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PolicyError {
    #[error("attempt ID was reused with different content")]
    AttemptIdentityConflict,
    #[error("attempt ordinal is not contiguous")]
    AttemptOrdinalGap,
    #[error("malformed or contradictory outcome metadata")]
    MalformedOutcome,
    #[error("attempt-policy checkpoint is inconsistent")]
    InvalidCheckpoint,
}

/// Pure policy ledger retaining decisions and no-resend invocation facts across
/// checkpoints. Re-applying an identical attempt returns the original decision.
#[derive(Clone, Debug, Default)]
pub struct AttemptLedger {
    records: BTreeMap<String, AttemptRecordV1>,
    last_ordinal: BTreeMap<String, u32>,
    no_resend_invocations: BTreeSet<String>,
}

impl AttemptLedger {
    pub fn decide(
        &mut self,
        policy: &AttemptPolicyV1,
        input: AttemptInputV1,
    ) -> Result<AttemptDecisionV1, PolicyError> {
        if let Some(existing) = self.records.get(&input.attempt_id) {
            if existing.operation_id == input.operation_id
                && existing.attempt_ordinal == input.attempt_ordinal
                && existing.outcome == input.outcome
            {
                return Ok(existing.decision.clone());
            }
            return Err(PolicyError::AttemptIdentityConflict);
        }
        let previous = self
            .last_ordinal
            .get(&input.operation_id)
            .copied()
            .unwrap_or(0);
        if input.attempt_ordinal != previous.saturating_add(1) {
            return Err(PolicyError::AttemptOrdinalGap);
        }
        let decision = Self::evaluate(policy, &input)?;
        if matches!(
            input.outcome,
            OutcomeClass::FailedKnownStarted
                | OutcomeClass::CancelledWithEvidence
                | OutcomeClass::OutcomeUncertain
        ) && !matches!(decision, AttemptDecisionV1::ReconcileSameInvocation { .. })
            && let Some(proof) = &input.retry_proof
        {
            self.no_resend_invocations
                .insert(proof.invocation_id.clone());
        }
        self.last_ordinal
            .insert(input.operation_id.clone(), input.attempt_ordinal);
        self.records.insert(
            input.attempt_id.clone(),
            AttemptRecordV1 {
                operation_id: input.operation_id,
                attempt_id: input.attempt_id,
                attempt_ordinal: input.attempt_ordinal,
                outcome: input.outcome,
                decision: decision.clone(),
            },
        );
        Ok(decision)
    }

    fn evaluate(
        policy: &AttemptPolicyV1,
        input: &AttemptInputV1,
    ) -> Result<AttemptDecisionV1, PolicyError> {
        if input.cancelled {
            return Ok(AttemptDecisionV1::Fail {
                reason: "cancelled".to_owned(),
            });
        }
        if input.evaluator_passed == Some(false) {
            if let Some(transition_id) = &policy.evaluator_transition_id {
                return Ok(AttemptDecisionV1::FollowExistingEdge {
                    transition_id: transition_id.clone(),
                });
            }
        }
        if input.gate_passed == Some(false) {
            if let Some(gate_id) = &policy.approval_gate_id {
                return Ok(AttemptDecisionV1::AwaitApproval {
                    gate_id: gate_id.clone(),
                });
            }
        }
        match input.outcome {
            OutcomeClass::Succeeded => Ok(AttemptDecisionV1::Complete),
            OutcomeClass::DefiniteNotStarted | OutcomeClass::ContractFailure
                if input.attempt_ordinal < policy.max_attempts =>
            {
                Ok(AttemptDecisionV1::RetryWithNewAttempt {
                    next_ordinal: input.attempt_ordinal + 1,
                })
            }
            OutcomeClass::FailedKnownStarted | OutcomeClass::CancelledWithEvidence => {
                if let Some(proof) = &input.retry_proof
                    && proof.descriptor_idempotent
                    && proof.same_id_deduplicated
                {
                    return Ok(AttemptDecisionV1::ReconcileSameInvocation {
                        invocation_id: proof.invocation_id.clone(),
                    });
                }
                Self::non_repeating_exit(policy, "known_started")
            }
            OutcomeClass::OutcomeUncertain => Ok(AttemptDecisionV1::RequireUserDecision),
            OutcomeClass::Denied => Self::non_repeating_exit(policy, "denied"),
            OutcomeClass::DefiniteNotStarted | OutcomeClass::ContractFailure => {
                Self::non_repeating_exit(policy, "attempts_exhausted")
            }
        }
    }

    fn non_repeating_exit(
        policy: &AttemptPolicyV1,
        reason: &str,
    ) -> Result<AttemptDecisionV1, PolicyError> {
        if let Some(transition_id) = &policy.feedback_transition_id {
            return Ok(AttemptDecisionV1::FollowExistingEdge {
                transition_id: transition_id.clone(),
            });
        }
        if let Some(node_id) = &policy.fallback_node_id {
            return Ok(AttemptDecisionV1::SelectExistingFallback {
                node_id: node_id.clone(),
            });
        }
        Ok(AttemptDecisionV1::Fail {
            reason: reason.to_owned(),
        })
    }

    #[must_use]
    pub fn may_send_invocation(&self, invocation_id: &str) -> bool {
        !self.no_resend_invocations.contains(invocation_id)
    }

    #[must_use]
    pub fn checkpoint(&self) -> (Vec<AttemptRecordV1>, Vec<String>) {
        (
            self.records.values().cloned().collect(),
            self.no_resend_invocations.iter().cloned().collect(),
        )
    }

    pub fn restore(checkpoint: (Vec<AttemptRecordV1>, Vec<String>)) -> Result<Self, PolicyError> {
        let (records, no_resend) = checkpoint;
        let mut ledger = Self::default();
        let mut ordinals = BTreeMap::<String, BTreeSet<u32>>::new();
        for record in records {
            if record.operation_id.trim().is_empty()
                || record.attempt_id.trim().is_empty()
                || record.operation_id.len() > 512
                || record.attempt_id.len() > 512
                || record.attempt_ordinal == 0
                || !decision_matches_outcome(&record)
                || ledger
                    .records
                    .insert(record.attempt_id.clone(), record.clone())
                    .is_some()
            {
                return Err(PolicyError::InvalidCheckpoint);
            }
            ordinals
                .entry(record.operation_id.clone())
                .or_default()
                .insert(record.attempt_ordinal);
        }
        for (operation_id, values) in ordinals {
            let maximum = values.iter().next_back().copied().unwrap_or(0);
            if values.len()
                != usize::try_from(maximum).map_err(|_| PolicyError::InvalidCheckpoint)?
                || values.iter().copied().ne(1..=maximum)
            {
                return Err(PolicyError::InvalidCheckpoint);
            }
            ledger.last_ordinal.insert(operation_id, maximum);
        }
        for invocation in no_resend {
            if invocation.trim().is_empty()
                || invocation.len() > 512
                || !ledger.no_resend_invocations.insert(invocation)
            {
                return Err(PolicyError::InvalidCheckpoint);
            }
        }
        if ledger.records.values().any(|record| {
            matches!(
                &record.decision,
                AttemptDecisionV1::ReconcileSameInvocation { invocation_id }
                    if ledger.no_resend_invocations.contains(invocation_id)
            )
        }) {
            return Err(PolicyError::InvalidCheckpoint);
        }
        Ok(ledger)
    }
}

fn decision_matches_outcome(record: &AttemptRecordV1) -> bool {
    match (&record.outcome, &record.decision) {
        (OutcomeClass::Succeeded, AttemptDecisionV1::Complete)
        | (OutcomeClass::OutcomeUncertain, AttemptDecisionV1::RequireUserDecision) => true,
        (
            OutcomeClass::DefiniteNotStarted | OutcomeClass::ContractFailure,
            AttemptDecisionV1::RetryWithNewAttempt { next_ordinal },
        ) => *next_ordinal == record.attempt_ordinal.saturating_add(1),
        (
            OutcomeClass::FailedKnownStarted | OutcomeClass::CancelledWithEvidence,
            AttemptDecisionV1::ReconcileSameInvocation { .. },
        ) => true,
        (_, AttemptDecisionV1::FollowExistingEdge { .. })
        | (_, AttemptDecisionV1::SelectExistingFallback { .. })
        | (_, AttemptDecisionV1::AwaitApproval { .. })
        | (_, AttemptDecisionV1::Fail { .. }) => true,
        _ => false,
    }
}
