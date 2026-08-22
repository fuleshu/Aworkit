//! Event-sourced one-Chat/one-Run lifecycle aggregate.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The externally inspectable, mutually exclusive lifecycle state of one Chat.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatState {
    Draft,
    Active,
    WaitingInput,
    WaitingApproval,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

impl ChatState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed | Self::Failed)
    }
}

/// The reason a worker has stopped at a committed, exact position.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Input,
    Approval,
}

/// Legal user/core commands against the aggregate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatCommand {
    Start { snapshot_hash: String },
    QueueInput { input_id: StableId },
    BeginAttempt { attempt_id: StableId },
    Wait { reason: WaitReason },
    Approve,
    Resume,
    Pause,
    Cancel,
    Cancelled,
    Complete,
    Fail { retryable: bool },
    Retry,
    Fork { child_chat_id: StableId },
    ContinueFrom { parent_chat_id: StableId },
}

/// A reducer event; the persistent committer stores the semantic form.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ChatEvent {
    Started { snapshot_hash: String },
    InputQueued { input_id: StableId },
    AttemptBegan { attempt_id: StableId },
    Waited { reason: WaitReason },
    Approved,
    Resumed,
    Paused,
    CancellationRequested,
    Cancelled,
    Completed,
    Failed { retryable: bool },
    Retried,
    Forked { child_chat_id: StableId },
    Continued { parent_chat_id: StableId },
}

/// Rebuildable run state; its snapshot identity never changes after `Started`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatAggregate {
    pub chat_id: StableId,
    pub state: ChatState,
    pub snapshot_hash: Option<String>,
    pub queued_inputs: Vec<StableId>,
    pub events: Vec<ChatEvent>,
    retryable_failure: bool,
}

impl ChatAggregate {
    #[must_use]
    pub fn new(chat_id: StableId) -> Self {
        Self {
            chat_id,
            state: ChatState::Draft,
            snapshot_hash: None,
            queued_inputs: Vec::new(),
            events: Vec::new(),
            retryable_failure: false,
        }
    }

    /// Validates and applies one transition, returning the durable event to commit.
    pub fn apply(&mut self, command: ChatCommand) -> Result<ChatEvent, LifecycleError> {
        let event = match command {
            ChatCommand::Start { snapshot_hash }
                if self.state == ChatState::Draft && !snapshot_hash.is_empty() =>
            {
                self.state = ChatState::Active;
                self.snapshot_hash = Some(snapshot_hash.clone());
                ChatEvent::Started { snapshot_hash }
            }
            ChatCommand::QueueInput { input_id } if !self.state.is_terminal() => {
                self.queued_inputs.push(input_id.clone());
                ChatEvent::InputQueued { input_id }
            }
            ChatCommand::BeginAttempt { attempt_id } if self.state == ChatState::Active => {
                ChatEvent::AttemptBegan { attempt_id }
            }
            ChatCommand::Wait {
                reason: WaitReason::Input,
            } if self.state == ChatState::Active => {
                self.state = ChatState::WaitingInput;
                ChatEvent::Waited {
                    reason: WaitReason::Input,
                }
            }
            ChatCommand::Wait {
                reason: WaitReason::Approval,
            } if self.state == ChatState::Active => {
                self.state = ChatState::WaitingApproval;
                ChatEvent::Waited {
                    reason: WaitReason::Approval,
                }
            }
            ChatCommand::Approve if self.state == ChatState::WaitingApproval => {
                self.state = ChatState::Active;
                ChatEvent::Approved
            }
            ChatCommand::Resume
                if matches!(self.state, ChatState::Paused | ChatState::WaitingInput) =>
            {
                self.state = ChatState::Active;
                ChatEvent::Resumed
            }
            ChatCommand::Pause
                if matches!(
                    self.state,
                    ChatState::Active | ChatState::WaitingInput | ChatState::WaitingApproval
                ) =>
            {
                self.state = ChatState::Paused;
                ChatEvent::Paused
            }
            ChatCommand::Cancel if !self.state.is_terminal() => {
                self.state = ChatState::Cancelling;
                ChatEvent::CancellationRequested
            }
            ChatCommand::Cancelled if self.state == ChatState::Cancelling => {
                self.state = ChatState::Cancelled;
                ChatEvent::Cancelled
            }
            ChatCommand::Complete if self.state == ChatState::Active => {
                self.state = ChatState::Completed;
                ChatEvent::Completed
            }
            ChatCommand::Fail { retryable }
                if matches!(self.state, ChatState::Active | ChatState::Cancelling) =>
            {
                self.state = ChatState::Failed;
                self.retryable_failure = retryable;
                ChatEvent::Failed { retryable }
            }
            ChatCommand::Retry if self.state == ChatState::Failed && self.retryable_failure => {
                self.state = ChatState::Active;
                ChatEvent::Retried
            }
            ChatCommand::Fork { child_chat_id } if self.state.is_terminal() => {
                ChatEvent::Forked { child_chat_id }
            }
            ChatCommand::ContinueFrom { parent_chat_id } if self.state == ChatState::Draft => {
                ChatEvent::Continued { parent_chat_id }
            }
            _ => return Err(LifecycleError::IllegalTransition(self.state)),
        };
        self.events.push(event.clone());
        Ok(event)
    }
}

/// A rejected command leaves the aggregate entirely unchanged.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum LifecycleError {
    #[error("command is illegal in lifecycle state {0:?}")]
    IllegalTransition(ChatState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStateV1 {
    Draft,
    BlockedDraft,
    Resolving,
    Starting,
    Active,
    WaitingInput,
    WaitingApproval,
    Pausing,
    Paused,
    Cancelling,
    Rehydrating,
    Blocked,
    Cancelled,
    Completed,
    Failed,
}

impl RunStateV1 {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum RunCommandKindV1 {
    /// Production first-input path: resolution/freezing happens before this
    /// command and the resulting event is committed in the same batch as the
    /// StartWorker outbox. No partially frozen Run becomes visible.
    StartResolved {
        input_id: StableId,
        snapshot_id: StableId,
        snapshot_hash: String,
    },
    RejectStart {
        code: String,
    },
    AcceptFirstInput {
        input_id: StableId,
    },
    FreezeSnapshot {
        snapshot_id: StableId,
        snapshot_hash: String,
    },
    WorkerReady {
        generation: ProcessGeneration,
    },
    QueueInput {
        input_id: StableId,
    },
    DeliverInput {
        input_id: StableId,
    },
    BeginAttempt {
        attempt_id: StableId,
        operation_id: StableId,
    },
    FinishAttempt {
        attempt_id: StableId,
        outcome: String,
    },
    Wait {
        reason: WaitReason,
    },
    ResolveApproval {
        approval_id: StableId,
        approved: bool,
    },
    RequestPause,
    WorkerQuiesced {
        checkpoint_hash: String,
    },
    Resume,
    RequestCancel,
    WorkerStopped,
    WorkerCrashed {
        generation: ProcessGeneration,
        checkpoint_hash: Option<String>,
        uncertain_invocations: Vec<StableId>,
    },
    RehydrationReady {
        generation: ProcessGeneration,
    },
    ResolveBlockedInvocation {
        invocation_id: StableId,
    },
    Complete,
    Fail {
        code: String,
        retryable: bool,
    },
    Retry,
    Fork {
        child_chat_id: StableId,
    },
    ContinueFrom {
        parent_chat_id: StableId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCommandV1 {
    pub command_id: StableId,
    pub expected_version: u64,
    pub command: RunCommandKindV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub enum RunEventKindV1 {
    ChatStarted {
        input_id: StableId,
        snapshot_id: StableId,
        snapshot_hash: String,
    },
    StartRejected {
        code: String,
    },
    FirstInputAccepted {
        input_id: StableId,
    },
    SnapshotFrozen {
        snapshot_id: StableId,
        snapshot_hash: String,
    },
    WorkerGenerationReady {
        generation: ProcessGeneration,
    },
    InputQueued {
        input_id: StableId,
    },
    InputDelivered {
        input_id: StableId,
    },
    AttemptBegan {
        attempt_id: StableId,
        operation_id: StableId,
    },
    AttemptFinished {
        attempt_id: StableId,
        outcome: String,
    },
    Waited {
        reason: WaitReason,
    },
    ApprovalResolved {
        approval_id: StableId,
        approved: bool,
    },
    PauseRequested,
    Paused {
        checkpoint_hash: String,
    },
    Resumed,
    CancellationRequested,
    Cancelled,
    WorkerGenerationCrashed {
        generation: ProcessGeneration,
        checkpoint_hash: Option<String>,
        uncertain_invocations: Vec<StableId>,
    },
    RehydrationReady {
        generation: ProcessGeneration,
    },
    BlockedInvocationResolved {
        invocation_id: StableId,
    },
    Completed,
    Failed {
        code: String,
        retryable: bool,
    },
    Retried,
    Forked {
        child_chat_id: StableId,
    },
    Continued {
        parent_chat_id: StableId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommittedRunEventV1 {
    pub sequence: u64,
    pub command_id: StableId,
    pub event: RunEventKindV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunCommandOutcomeV1 {
    Applied(CommittedRunEventV1),
    Duplicate(CommittedRunEventV1),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptStateV1 {
    pub operation_id: StableId,
    pub outcome: Option<String>,
}

/// Pure event-sourced lifecycle truth for exactly one Chat and one Run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunAggregateV1 {
    pub chat_id: StableId,
    pub run_id: StableId,
    pub version: u64,
    pub state: RunStateV1,
    pub snapshot_id: Option<StableId>,
    pub snapshot_hash: Option<String>,
    pub generation: Option<ProcessGeneration>,
    pub queued_inputs: Vec<StableId>,
    pub active_attempts: BTreeMap<String, AttemptStateV1>,
    pub last_checkpoint_hash: Option<String>,
    pub uncertain_invocations: BTreeSet<String>,
    pub retryable_failure: bool,
    pub parent_chat_id: Option<StableId>,
    pub child_chat_ids: Vec<StableId>,
    pub start_rejection: Option<String>,
    pub rehydration_target: Option<RunStateV1>,
    events: Vec<CommittedRunEventV1>,
    command_digests: BTreeMap<String, String>,
}

impl RunAggregateV1 {
    #[must_use]
    pub fn new(chat_id: StableId, run_id: StableId) -> Self {
        Self {
            chat_id,
            run_id,
            version: 0,
            state: RunStateV1::Draft,
            snapshot_id: None,
            snapshot_hash: None,
            generation: None,
            queued_inputs: Vec::new(),
            active_attempts: BTreeMap::new(),
            last_checkpoint_hash: None,
            uncertain_invocations: BTreeSet::new(),
            retryable_failure: false,
            parent_chat_id: None,
            child_chat_ids: Vec::new(),
            start_rejection: None,
            rehydration_target: None,
            events: Vec::new(),
            command_digests: BTreeMap::new(),
        }
    }

    pub fn handle(
        &mut self,
        command: RunCommandV1,
    ) -> Result<RunCommandOutcomeV1, LifecycleErrorV1> {
        let digest = command_digest(&command)?;
        if let Some(previous) = self.command_digests.get(command.command_id.as_str()) {
            if previous != &digest {
                return Err(LifecycleErrorV1::CommandIdentityConflict);
            }
            let event = self
                .events
                .iter()
                .find(|event| event.command_id == command.command_id)
                .cloned()
                .ok_or(LifecycleErrorV1::CorruptHistory)?;
            return Ok(RunCommandOutcomeV1::Duplicate(event));
        }
        if command.expected_version != self.version {
            return Err(LifecycleErrorV1::VersionConflict {
                expected: command.expected_version,
                actual: self.version,
            });
        }
        let event_kind = self.decide(&command.command)?;
        let event = CommittedRunEventV1 {
            sequence: self
                .version
                .checked_add(1)
                .ok_or(LifecycleErrorV1::VersionExhausted)?,
            command_id: command.command_id.clone(),
            event: event_kind,
        };
        self.apply_event(&event)?;
        self.command_digests
            .insert(command.command_id.as_str().to_owned(), digest);
        self.events.push(event.clone());
        Ok(RunCommandOutcomeV1::Applied(event))
    }

    pub fn fold(
        chat_id: StableId,
        run_id: StableId,
        events: &[CommittedRunEventV1],
    ) -> Result<Self, LifecycleErrorV1> {
        let mut aggregate = Self::new(chat_id, run_id);
        for event in events {
            if event.sequence != aggregate.version.saturating_add(1)
                || aggregate
                    .command_digests
                    .contains_key(event.command_id.as_str())
            {
                return Err(LifecycleErrorV1::CorruptHistory);
            }
            aggregate.apply_event(event)?;
            aggregate.command_digests.insert(
                event.command_id.as_str().to_owned(),
                format!("folded:{}", event.sequence),
            );
            aggregate.events.push(event.clone());
        }
        Ok(aggregate)
    }

    #[must_use]
    pub fn events(&self) -> &[CommittedRunEventV1] {
        &self.events
    }

    fn decide(&self, command: &RunCommandKindV1) -> Result<RunEventKindV1, LifecycleErrorV1> {
        let illegal = || LifecycleErrorV1::IllegalTransition(self.state);
        match command {
            RunCommandKindV1::StartResolved {
                input_id,
                snapshot_id,
                snapshot_hash,
            } if matches!(self.state, RunStateV1::Draft | RunStateV1::BlockedDraft)
                && self.snapshot_hash.is_none()
                && is_sha256(snapshot_hash) =>
            {
                Ok(RunEventKindV1::ChatStarted {
                    input_id: input_id.clone(),
                    snapshot_id: snapshot_id.clone(),
                    snapshot_hash: snapshot_hash.clone(),
                })
            }
            RunCommandKindV1::RejectStart { code }
                if matches!(
                    self.state,
                    RunStateV1::Draft | RunStateV1::BlockedDraft | RunStateV1::Resolving
                ) && self.snapshot_hash.is_none()
                    && valid_label(code) =>
            {
                Ok(RunEventKindV1::StartRejected { code: code.clone() })
            }
            RunCommandKindV1::AcceptFirstInput { input_id }
                if matches!(self.state, RunStateV1::Draft | RunStateV1::BlockedDraft) =>
            {
                Ok(RunEventKindV1::FirstInputAccepted {
                    input_id: input_id.clone(),
                })
            }
            RunCommandKindV1::FreezeSnapshot {
                snapshot_id,
                snapshot_hash,
            } if self.state == RunStateV1::Resolving
                && is_sha256(snapshot_hash)
                && self.snapshot_hash.is_none() =>
            {
                Ok(RunEventKindV1::SnapshotFrozen {
                    snapshot_id: snapshot_id.clone(),
                    snapshot_hash: snapshot_hash.clone(),
                })
            }
            RunCommandKindV1::WorkerReady { generation }
                if self.state == RunStateV1::Starting && generation.0 > 0 =>
            {
                Ok(RunEventKindV1::WorkerGenerationReady {
                    generation: *generation,
                })
            }
            RunCommandKindV1::QueueInput { input_id }
                if accepts_queued_input(self.state) && !self.queued_inputs.contains(input_id) =>
            {
                Ok(RunEventKindV1::InputQueued {
                    input_id: input_id.clone(),
                })
            }
            RunCommandKindV1::DeliverInput { input_id }
                if matches!(self.state, RunStateV1::Active | RunStateV1::WaitingInput)
                    && self.queued_inputs.first() == Some(input_id) =>
            {
                Ok(RunEventKindV1::InputDelivered {
                    input_id: input_id.clone(),
                })
            }
            RunCommandKindV1::BeginAttempt {
                attempt_id,
                operation_id,
            } if self.state == RunStateV1::Active
                && !self.active_attempts.contains_key(attempt_id.as_str()) =>
            {
                Ok(RunEventKindV1::AttemptBegan {
                    attempt_id: attempt_id.clone(),
                    operation_id: operation_id.clone(),
                })
            }
            RunCommandKindV1::FinishAttempt {
                attempt_id,
                outcome,
            } if self.state == RunStateV1::Active
                && self
                    .active_attempts
                    .get(attempt_id.as_str())
                    .is_some_and(|attempt| attempt.outcome.is_none())
                && valid_label(outcome) =>
            {
                Ok(RunEventKindV1::AttemptFinished {
                    attempt_id: attempt_id.clone(),
                    outcome: outcome.clone(),
                })
            }
            RunCommandKindV1::Wait { reason } if self.state == RunStateV1::Active => {
                Ok(RunEventKindV1::Waited { reason: *reason })
            }
            RunCommandKindV1::ResolveApproval {
                approval_id,
                approved,
            } if self.state == RunStateV1::WaitingApproval => {
                Ok(RunEventKindV1::ApprovalResolved {
                    approval_id: approval_id.clone(),
                    approved: *approved,
                })
            }
            RunCommandKindV1::RequestPause
                if matches!(
                    self.state,
                    RunStateV1::Active | RunStateV1::WaitingInput | RunStateV1::WaitingApproval
                ) =>
            {
                Ok(RunEventKindV1::PauseRequested)
            }
            RunCommandKindV1::WorkerQuiesced { checkpoint_hash }
                if self.state == RunStateV1::Pausing && is_sha256(checkpoint_hash) =>
            {
                Ok(RunEventKindV1::Paused {
                    checkpoint_hash: checkpoint_hash.clone(),
                })
            }
            RunCommandKindV1::Resume if self.state == RunStateV1::Paused => {
                Ok(RunEventKindV1::Resumed)
            }
            RunCommandKindV1::RequestCancel if !self.state.is_terminal() => {
                Ok(RunEventKindV1::CancellationRequested)
            }
            RunCommandKindV1::WorkerStopped if self.state == RunStateV1::Cancelling => {
                Ok(RunEventKindV1::Cancelled)
            }
            RunCommandKindV1::WorkerCrashed {
                generation,
                checkpoint_hash,
                uncertain_invocations,
            } if matches!(
                self.state,
                RunStateV1::Active
                    | RunStateV1::WaitingInput
                    | RunStateV1::WaitingApproval
                    | RunStateV1::Pausing
                    | RunStateV1::Paused
                    | RunStateV1::Cancelling
            ) && self.generation == Some(*generation)
                && checkpoint_hash.as_ref().is_none_or(|hash| is_sha256(hash))
                && uncertain_invocations
                    .iter()
                    .map(StableId::as_str)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == uncertain_invocations.len() =>
            {
                Ok(RunEventKindV1::WorkerGenerationCrashed {
                    generation: *generation,
                    checkpoint_hash: checkpoint_hash.clone(),
                    uncertain_invocations: uncertain_invocations.clone(),
                })
            }
            RunCommandKindV1::RehydrationReady { generation }
                if self.state == RunStateV1::Rehydrating
                    && self
                        .generation
                        .is_none_or(|current| generation.0 > current.0) =>
            {
                Ok(RunEventKindV1::RehydrationReady {
                    generation: *generation,
                })
            }
            RunCommandKindV1::ResolveBlockedInvocation { invocation_id }
                if self.state == RunStateV1::Blocked
                    && self.uncertain_invocations.contains(invocation_id.as_str()) =>
            {
                Ok(RunEventKindV1::BlockedInvocationResolved {
                    invocation_id: invocation_id.clone(),
                })
            }
            RunCommandKindV1::Complete if self.state == RunStateV1::Active => {
                Ok(RunEventKindV1::Completed)
            }
            RunCommandKindV1::Fail { code, retryable }
                if !self.state.is_terminal() && valid_label(code) =>
            {
                Ok(RunEventKindV1::Failed {
                    code: code.clone(),
                    retryable: *retryable,
                })
            }
            RunCommandKindV1::Retry
                if self.state == RunStateV1::Failed && self.retryable_failure =>
            {
                Ok(RunEventKindV1::Retried)
            }
            RunCommandKindV1::Fork { child_chat_id } if self.state.is_terminal() => {
                Ok(RunEventKindV1::Forked {
                    child_chat_id: child_chat_id.clone(),
                })
            }
            RunCommandKindV1::ContinueFrom { parent_chat_id }
                if self.state == RunStateV1::Draft && self.parent_chat_id.is_none() =>
            {
                Ok(RunEventKindV1::Continued {
                    parent_chat_id: parent_chat_id.clone(),
                })
            }
            _ => Err(illegal()),
        }
    }

    fn apply_event(&mut self, committed: &CommittedRunEventV1) -> Result<(), LifecycleErrorV1> {
        if committed.sequence != self.version.saturating_add(1) {
            return Err(LifecycleErrorV1::CorruptHistory);
        }
        if !self.can_apply(&committed.event) {
            return Err(LifecycleErrorV1::CorruptHistory);
        }
        match &committed.event {
            RunEventKindV1::ChatStarted {
                input_id,
                snapshot_id,
                snapshot_hash,
            } => {
                self.queued_inputs.push(input_id.clone());
                self.snapshot_id = Some(snapshot_id.clone());
                self.snapshot_hash = Some(snapshot_hash.clone());
                self.start_rejection = None;
                self.state = RunStateV1::Starting;
            }
            RunEventKindV1::StartRejected { code } => {
                self.start_rejection = Some(code.clone());
                self.queued_inputs.clear();
                self.state = RunStateV1::BlockedDraft;
            }
            RunEventKindV1::FirstInputAccepted { input_id } => {
                self.state = RunStateV1::Resolving;
                self.start_rejection = None;
                self.queued_inputs.push(input_id.clone());
            }
            RunEventKindV1::SnapshotFrozen {
                snapshot_id,
                snapshot_hash,
            } => {
                self.snapshot_id = Some(snapshot_id.clone());
                self.snapshot_hash = Some(snapshot_hash.clone());
                self.state = RunStateV1::Starting;
            }
            RunEventKindV1::WorkerGenerationReady { generation }
            | RunEventKindV1::RehydrationReady { generation } => {
                self.generation = Some(*generation);
                self.state = self.rehydration_target.take().unwrap_or(RunStateV1::Active);
            }
            RunEventKindV1::InputQueued { input_id } => {
                self.queued_inputs.push(input_id.clone());
            }
            RunEventKindV1::InputDelivered { input_id } => {
                if self.queued_inputs.first() != Some(input_id) {
                    return Err(LifecycleErrorV1::CorruptHistory);
                }
                self.queued_inputs.remove(0);
                self.state = RunStateV1::Active;
            }
            RunEventKindV1::AttemptBegan {
                attempt_id,
                operation_id,
            } => {
                self.active_attempts.insert(
                    attempt_id.as_str().to_owned(),
                    AttemptStateV1 {
                        operation_id: operation_id.clone(),
                        outcome: None,
                    },
                );
            }
            RunEventKindV1::AttemptFinished {
                attempt_id,
                outcome,
            } => {
                self.active_attempts
                    .get_mut(attempt_id.as_str())
                    .ok_or(LifecycleErrorV1::CorruptHistory)?
                    .outcome = Some(outcome.clone());
            }
            RunEventKindV1::Waited { reason } => {
                self.state = match reason {
                    WaitReason::Input => RunStateV1::WaitingInput,
                    WaitReason::Approval => RunStateV1::WaitingApproval,
                };
            }
            RunEventKindV1::ApprovalResolved { approved, .. } => {
                // Denial is a committed workflow outcome, not a lifecycle
                // dead-end. Frozen routing decides the next legal edge.
                let _ = approved;
                self.state = RunStateV1::Active;
            }
            RunEventKindV1::PauseRequested => self.state = RunStateV1::Pausing,
            RunEventKindV1::Paused { checkpoint_hash } => {
                self.last_checkpoint_hash = Some(checkpoint_hash.clone());
                self.state = RunStateV1::Paused;
            }
            RunEventKindV1::Resumed => self.state = RunStateV1::Active,
            RunEventKindV1::CancellationRequested => self.state = RunStateV1::Cancelling,
            RunEventKindV1::Cancelled => self.state = RunStateV1::Cancelled,
            RunEventKindV1::WorkerGenerationCrashed {
                checkpoint_hash,
                uncertain_invocations,
                ..
            } => {
                self.rehydration_target = Some(match self.state {
                    RunStateV1::WaitingInput => RunStateV1::WaitingInput,
                    RunStateV1::WaitingApproval => RunStateV1::WaitingApproval,
                    RunStateV1::Paused | RunStateV1::Pausing => RunStateV1::Paused,
                    RunStateV1::Cancelling => RunStateV1::Cancelling,
                    _ => RunStateV1::Active,
                });
                self.last_checkpoint_hash = checkpoint_hash.clone();
                self.uncertain_invocations = uncertain_invocations
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect();
                self.state = if self.uncertain_invocations.is_empty() && checkpoint_hash.is_some() {
                    RunStateV1::Rehydrating
                } else {
                    RunStateV1::Blocked
                };
            }
            RunEventKindV1::BlockedInvocationResolved { invocation_id } => {
                self.uncertain_invocations.remove(invocation_id.as_str());
                if self.uncertain_invocations.is_empty() && self.last_checkpoint_hash.is_some() {
                    self.state = RunStateV1::Rehydrating;
                }
            }
            RunEventKindV1::Completed => self.state = RunStateV1::Completed,
            RunEventKindV1::Failed { retryable, .. } => {
                self.retryable_failure = *retryable;
                self.state = RunStateV1::Failed;
            }
            RunEventKindV1::Retried => {
                self.retryable_failure = false;
                self.rehydration_target = Some(RunStateV1::Active);
                self.state = RunStateV1::Rehydrating;
            }
            RunEventKindV1::Forked { child_chat_id } => {
                self.child_chat_ids.push(child_chat_id.clone());
            }
            RunEventKindV1::Continued { parent_chat_id } => {
                self.parent_chat_id = Some(parent_chat_id.clone());
            }
        }
        self.version = committed.sequence;
        Ok(())
    }

    fn can_apply(&self, event: &RunEventKindV1) -> bool {
        match event {
            RunEventKindV1::ChatStarted { snapshot_hash, .. } => {
                matches!(self.state, RunStateV1::Draft | RunStateV1::BlockedDraft)
                    && self.snapshot_hash.is_none()
                    && is_sha256(snapshot_hash)
            }
            RunEventKindV1::StartRejected { code } => {
                matches!(
                    self.state,
                    RunStateV1::Draft | RunStateV1::BlockedDraft | RunStateV1::Resolving
                ) && self.snapshot_hash.is_none()
                    && valid_label(code)
            }
            RunEventKindV1::FirstInputAccepted { input_id } => {
                matches!(self.state, RunStateV1::Draft | RunStateV1::BlockedDraft)
                    && !self.queued_inputs.contains(input_id)
            }
            RunEventKindV1::SnapshotFrozen { snapshot_hash, .. } => {
                self.state == RunStateV1::Resolving
                    && self.snapshot_hash.is_none()
                    && is_sha256(snapshot_hash)
            }
            RunEventKindV1::WorkerGenerationReady { generation } => {
                self.state == RunStateV1::Starting && generation.0 > 0
            }
            RunEventKindV1::InputQueued { input_id } => {
                accepts_queued_input(self.state) && !self.queued_inputs.contains(input_id)
            }
            RunEventKindV1::InputDelivered { input_id } => {
                matches!(self.state, RunStateV1::Active | RunStateV1::WaitingInput)
                    && self.queued_inputs.first() == Some(input_id)
            }
            RunEventKindV1::AttemptBegan { attempt_id, .. } => {
                self.state == RunStateV1::Active
                    && !self.active_attempts.contains_key(attempt_id.as_str())
            }
            RunEventKindV1::AttemptFinished {
                attempt_id,
                outcome,
            } => {
                self.state == RunStateV1::Active
                    && valid_label(outcome)
                    && self
                        .active_attempts
                        .get(attempt_id.as_str())
                        .is_some_and(|attempt| attempt.outcome.is_none())
            }
            RunEventKindV1::Waited { .. } => self.state == RunStateV1::Active,
            RunEventKindV1::ApprovalResolved { .. } => self.state == RunStateV1::WaitingApproval,
            RunEventKindV1::PauseRequested => matches!(
                self.state,
                RunStateV1::Active | RunStateV1::WaitingInput | RunStateV1::WaitingApproval
            ),
            RunEventKindV1::Paused { checkpoint_hash } => {
                self.state == RunStateV1::Pausing && is_sha256(checkpoint_hash)
            }
            RunEventKindV1::Resumed => self.state == RunStateV1::Paused,
            RunEventKindV1::CancellationRequested => !self.state.is_terminal(),
            RunEventKindV1::Cancelled => self.state == RunStateV1::Cancelling,
            RunEventKindV1::WorkerGenerationCrashed {
                generation,
                checkpoint_hash,
                uncertain_invocations,
            } => {
                matches!(
                    self.state,
                    RunStateV1::Active
                        | RunStateV1::WaitingInput
                        | RunStateV1::WaitingApproval
                        | RunStateV1::Pausing
                        | RunStateV1::Paused
                        | RunStateV1::Cancelling
                ) && self.generation == Some(*generation)
                    && checkpoint_hash.as_ref().is_none_or(|hash| is_sha256(hash))
                    && uncertain_invocations
                        .iter()
                        .map(StableId::as_str)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == uncertain_invocations.len()
            }
            RunEventKindV1::RehydrationReady { generation } => {
                self.state == RunStateV1::Rehydrating
                    && self
                        .generation
                        .is_none_or(|current| generation.0 > current.0)
            }
            RunEventKindV1::BlockedInvocationResolved { invocation_id } => {
                self.state == RunStateV1::Blocked
                    && self.uncertain_invocations.contains(invocation_id.as_str())
            }
            RunEventKindV1::Completed => self.state == RunStateV1::Active,
            RunEventKindV1::Failed { code, .. } => !self.state.is_terminal() && valid_label(code),
            RunEventKindV1::Retried => self.state == RunStateV1::Failed && self.retryable_failure,
            RunEventKindV1::Forked { child_chat_id } => {
                self.state.is_terminal() && !self.child_chat_ids.contains(child_chat_id)
            }
            RunEventKindV1::Continued { .. } => {
                self.state == RunStateV1::Draft && self.parent_chat_id.is_none()
            }
        }
    }
}

fn command_digest(command: &RunCommandV1) -> Result<String, LifecycleErrorV1> {
    use sha2::{Digest, Sha256};
    let bytes = serde_jcs::to_vec(command).map_err(|_| LifecycleErrorV1::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn accepts_queued_input(state: RunStateV1) -> bool {
    matches!(
        state,
        RunStateV1::Starting
            | RunStateV1::Active
            | RunStateV1::WaitingInput
            | RunStateV1::WaitingApproval
            | RunStateV1::Pausing
            | RunStateV1::Paused
            | RunStateV1::Rehydrating
    )
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LifecycleErrorV1 {
    #[error("command is illegal in Run lifecycle state {0:?}")]
    IllegalTransition(RunStateV1),
    #[error("Run command version conflict: expected {expected}, actual {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("command ID was reused with different content")]
    CommandIdentityConflict,
    #[error("Run event history is non-contiguous or reducer-inconsistent")]
    CorruptHistory,
    #[error("Run aggregate version exhausted")]
    VersionExhausted,
    #[error("Run command could not be canonically encoded")]
    Encoding,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(value: &str) -> StableId {
        StableId::parse(value).expect("id")
    }
    #[test]
    fn snapshot_is_set_exactly_once_and_waits_require_explicit_resume() {
        let mut chat = ChatAggregate::new(id("chat.1"));
        chat.apply(ChatCommand::Start {
            snapshot_hash: "frozen".into(),
        })
        .expect("start");
        assert!(
            chat.apply(ChatCommand::Start {
                snapshot_hash: "new".into()
            })
            .is_err()
        );
        chat.apply(ChatCommand::Wait {
            reason: WaitReason::Approval,
        })
        .expect("wait");
        assert!(chat.apply(ChatCommand::Resume).is_err());
        chat.apply(ChatCommand::Approve).expect("approve");
        assert_eq!(chat.snapshot_hash.as_deref(), Some("frozen"));
    }
}
