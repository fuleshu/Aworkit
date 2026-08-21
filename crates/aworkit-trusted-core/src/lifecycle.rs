//! Event-sourced one-Chat/one-Run lifecycle aggregate.

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The externally inspectable, mutually exclusive lifecycle state of one Chat.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatState { Draft, Active, WaitingInput, WaitingApproval, Paused, Cancelling, Cancelled, Completed, Failed }

impl ChatState {
    #[must_use]
    pub const fn is_terminal(self) -> bool { matches!(self, Self::Cancelled | Self::Completed | Self::Failed) }
}

/// The reason a worker has stopped at a committed, exact position.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason { Input, Approval }

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
    Started { snapshot_hash: String }, InputQueued { input_id: StableId }, AttemptBegan { attempt_id: StableId },
    Waited { reason: WaitReason }, Approved, Resumed, Paused, CancellationRequested, Cancelled,
    Completed, Failed { retryable: bool }, Retried, Forked { child_chat_id: StableId }, Continued { parent_chat_id: StableId },
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
    pub fn new(chat_id: StableId) -> Self { Self { chat_id, state: ChatState::Draft, snapshot_hash: None, queued_inputs: Vec::new(), events: Vec::new(), retryable_failure: false } }

    /// Validates and applies one transition, returning the durable event to commit.
    pub fn apply(&mut self, command: ChatCommand) -> Result<ChatEvent, LifecycleError> {
        let event = match command {
            ChatCommand::Start { snapshot_hash } if self.state == ChatState::Draft && !snapshot_hash.is_empty() => { self.state = ChatState::Active; self.snapshot_hash = Some(snapshot_hash.clone()); ChatEvent::Started { snapshot_hash } }
            ChatCommand::QueueInput { input_id } if !self.state.is_terminal() => { self.queued_inputs.push(input_id.clone()); ChatEvent::InputQueued { input_id } }
            ChatCommand::BeginAttempt { attempt_id } if self.state == ChatState::Active => ChatEvent::AttemptBegan { attempt_id },
            ChatCommand::Wait { reason: WaitReason::Input } if self.state == ChatState::Active => { self.state = ChatState::WaitingInput; ChatEvent::Waited { reason: WaitReason::Input } }
            ChatCommand::Wait { reason: WaitReason::Approval } if self.state == ChatState::Active => { self.state = ChatState::WaitingApproval; ChatEvent::Waited { reason: WaitReason::Approval } }
            ChatCommand::Approve if self.state == ChatState::WaitingApproval => { self.state = ChatState::Active; ChatEvent::Approved }
            ChatCommand::Resume if matches!(self.state, ChatState::Paused | ChatState::WaitingInput) => { self.state = ChatState::Active; ChatEvent::Resumed }
            ChatCommand::Pause if matches!(self.state, ChatState::Active | ChatState::WaitingInput | ChatState::WaitingApproval) => { self.state = ChatState::Paused; ChatEvent::Paused }
            ChatCommand::Cancel if !self.state.is_terminal() => { self.state = ChatState::Cancelling; ChatEvent::CancellationRequested }
            ChatCommand::Cancelled if self.state == ChatState::Cancelling => { self.state = ChatState::Cancelled; ChatEvent::Cancelled }
            ChatCommand::Complete if self.state == ChatState::Active => { self.state = ChatState::Completed; ChatEvent::Completed }
            ChatCommand::Fail { retryable } if matches!(self.state, ChatState::Active | ChatState::Cancelling) => { self.state = ChatState::Failed; self.retryable_failure = retryable; ChatEvent::Failed { retryable } }
            ChatCommand::Retry if self.state == ChatState::Failed && self.retryable_failure => { self.state = ChatState::Active; ChatEvent::Retried }
            ChatCommand::Fork { child_chat_id } if self.state.is_terminal() => ChatEvent::Forked { child_chat_id },
            ChatCommand::ContinueFrom { parent_chat_id } if self.state == ChatState::Draft => ChatEvent::Continued { parent_chat_id },
            _ => return Err(LifecycleError::IllegalTransition(self.state)),
        };
        self.events.push(event.clone());
        Ok(event)
    }
}

/// A rejected command leaves the aggregate entirely unchanged.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum LifecycleError { #[error("command is illegal in lifecycle state {0:?}")] IllegalTransition(ChatState) }

#[cfg(test)]
mod tests {
    use super::*;
    fn id(value: &str) -> StableId { StableId::parse(value).expect("id") }
    #[test]
    fn snapshot_is_set_exactly_once_and_waits_require_explicit_resume() {
        let mut chat = ChatAggregate::new(id("chat.1"));
        chat.apply(ChatCommand::Start { snapshot_hash: "frozen".into() }).expect("start");
        assert!(chat.apply(ChatCommand::Start { snapshot_hash: "new".into() }).is_err());
        chat.apply(ChatCommand::Wait { reason: WaitReason::Approval }).expect("wait");
        assert!(chat.apply(ChatCommand::Resume).is_err());
        chat.apply(ChatCommand::Approve).expect("approve");
        assert_eq!(chat.snapshot_hash.as_deref(), Some("frozen"));
    }
}
