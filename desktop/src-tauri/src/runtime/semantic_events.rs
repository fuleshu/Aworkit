//! Canonical semantic events shared by execution, durable history, and Chat.
//!
//! Producers submit semantic drafts. A committer assigns the authoritative
//! Chat sequence and event identity, persists the batch, and only then returns
//! the exact envelopes that may be published to subscribers.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One immutable event from the canonical Chat/Branch stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoreEventEnvelope {
    pub schema_version: u16,
    pub stream_id: String,
    pub branch_id: String,
    pub sequence: u64,
    pub event_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_event_id: Option<String>,
    pub payload: Value,
}

/// Persistence-ready semantic event before the canonical sequence is assigned.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticEventDraft {
    pub kind: String,
    pub payload: Value,
}

impl SemanticEventDraft {
    pub(crate) fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }
}

/// Sole write boundary for semantic execution events.
pub(crate) trait SemanticEventCommitter: Send + Sync {
    fn commit(&self, events: Vec<SemanticEventDraft>) -> Result<Vec<CoreEventEnvelope>, String>;

    /// Replays the exact committed envelopes used to recover open spans and
    /// causation after a suspended execution or process restart.
    fn committed_events(&self) -> Result<Vec<CoreEventEnvelope>, String>;
}

/// Transport for events that have already committed durably. Delivery failure
/// never rolls back history; the transactional outbox remains pending instead.
pub trait CommittedChatEventPort: Send + Sync {
    fn publish(&self, event: CoreEventEnvelope) -> Result<(), String>;
}

#[derive(Default)]
struct NoopCommittedChatEventPort;

impl CommittedChatEventPort for NoopCommittedChatEventPort {
    fn publish(&self, _event: CoreEventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

pub(crate) fn noop_committed_event_port() -> Arc<dyn CommittedChatEventPort> {
    Arc::new(NoopCommittedChatEventPort)
}

/// Non-persistent adapter used only when the workflow pipeline is composed in
/// isolation. The desktop composition always injects durable Chat history.
#[derive(Default)]
struct EphemeralSemanticEventState {
    next_sequence: Mutex<u64>,
    events: Mutex<Vec<CoreEventEnvelope>>,
}

impl SemanticEventCommitter for EphemeralSemanticEventState {
    fn commit(&self, events: Vec<SemanticEventDraft>) -> Result<Vec<CoreEventEnvelope>, String> {
        let mut next = self
            .next_sequence
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let committed = events
            .into_iter()
            .map(|event| {
                *next = next.saturating_add(1);
                envelope("chat.ephemeral", "main", *next, event)
            })
            .collect::<Vec<_>>();
        self.events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .extend(committed.iter().cloned());
        Ok(committed)
    }

    fn committed_events(&self) -> Result<Vec<CoreEventEnvelope>, String> {
        Ok(self
            .events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone())
    }
}

pub(crate) fn ephemeral_semantic_event_committer() -> Arc<dyn SemanticEventCommitter> {
    Arc::new(EphemeralSemanticEventState::default())
}

pub(crate) fn envelope(
    stream_id: &str,
    branch_id: &str,
    sequence: u64,
    event: SemanticEventDraft,
) -> CoreEventEnvelope {
    let span_id = event
        .payload
        .get("spanId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let causation_event_id = event
        .payload
        .get("causationEventId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    CoreEventEnvelope {
        schema_version: 1,
        stream_id: stream_id.to_owned(),
        branch_id: branch_id.to_owned(),
        sequence,
        event_id: format!("event.chat.{sequence}"),
        kind: event.kind,
        span_id,
        causation_event_id,
        payload: event.payload,
    }
}
