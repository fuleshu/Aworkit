//! Versioned, receipt-oriented desktop command and ordered-event surface.

use std::sync::{Arc, Mutex};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Every desktop command carries one opaque client idempotency key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopCommand {
    pub command_id: StableId,
    pub expected_version: u64,
    pub name: String,
    pub payload: Value,
}

/// A transport acknowledgement is intentionally not a domain-completion claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopReceipt {
    pub command_id: StableId,
    pub accepted_for_processing: bool,
    pub current_version: u64,
}

/// A committed, ordered event observable by the desktop projection layer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopEvent {
    pub sequence: u64,
    pub event_id: StableId,
    pub name: String,
    pub payload: Value,
}

/// A bounded snapshot followed by events strictly after `last_sequence`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopSnapshot {
    pub version: u64,
    pub last_sequence: u64,
    pub events: Vec<DesktopEvent>,
}

/// In-memory event fan-out facade; durable events remain owned by the committer.
#[derive(Clone, Default)]
pub struct DesktopApi {
    state: Arc<Mutex<DesktopState>>,
}

#[derive(Default)]
struct DesktopState {
    version: u64,
    events: Vec<DesktopEvent>,
}

impl DesktopApi {
    /// Accepts a command only at the caller-observed optimistic API version.
    pub fn accept(&self, command: &DesktopCommand) -> Result<DesktopReceipt, DesktopApiError> {
        let state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        if state.version != command.expected_version {
            return Err(DesktopApiError::VersionConflict { expected: command.expected_version, actual: state.version });
        }
        Ok(DesktopReceipt { command_id: command.command_id.clone(), accepted_for_processing: true, current_version: state.version })
    }

    /// Publishes only a committed domain event and advances the observed version.
    pub fn publish_committed(&self, event_id: StableId, name: impl Into<String>, payload: Value) -> Result<DesktopEvent, DesktopApiError> {
        let mut state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        state.version = state.version.checked_add(1).ok_or(DesktopApiError::VersionExhausted)?;
        let event = DesktopEvent { sequence: state.version, event_id, name: name.into(), payload };
        state.events.push(event.clone());
        Ok(event)
    }

    /// Returns a snapshot of events committed after the supplied cursor.
    pub fn snapshot_after(&self, last_sequence: u64) -> Result<DesktopSnapshot, DesktopApiError> {
        let state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        Ok(DesktopSnapshot { version: state.version, last_sequence: state.version, events: state.events.iter().filter(|event| event.sequence > last_sequence).cloned().collect() })
    }
}

/// API-level failures do not claim domain execution completed.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum DesktopApiError {
    #[error("desktop command version conflict: expected {expected}, actual {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("desktop event version is exhausted")]
    VersionExhausted,
    #[error("desktop API state is unavailable")]
    Poisoned,
}
