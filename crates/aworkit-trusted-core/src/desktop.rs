//! Versioned, receipt-oriented desktop command and ordered-event surface.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{Arc, Mutex},
};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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
    accepted_commands: BTreeMap<String, AcceptedCommand>,
    event_digests: BTreeMap<String, String>,
}

#[derive(Clone)]
struct AcceptedCommand {
    command_digest: String,
    transaction_digest: String,
    receipt: DesktopReceipt,
    event: DesktopEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopTransactionV1 {
    pub receipt: DesktopReceipt,
    pub event: DesktopEvent,
    pub duplicate: bool,
}

impl DesktopApi {
    /// Validation-only compatibility admission. It deliberately does not
    /// consume the idempotency key; use `transact_committed` to finalize it.
    pub fn accept(&self, command: &DesktopCommand) -> Result<DesktopReceipt, DesktopApiError> {
        validate_command(command)?;
        let digest = content_hash(command)?;
        let state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        if let Some(previous) = state.accepted_commands.get(command.command_id.as_str()) {
            return if previous.command_digest == digest {
                Ok(previous.receipt.clone())
            } else {
                Err(DesktopApiError::CommandIdentityConflict)
            };
        }
        if state.version != command.expected_version {
            return Err(DesktopApiError::VersionConflict {
                expected: command.expected_version,
                actual: state.version,
            });
        }
        Ok(DesktopReceipt {
            command_id: command.command_id.clone(),
            accepted_for_processing: true,
            current_version: state.version,
        })
    }

    /// Runs one already-validated domain mutation and atomically records both
    /// its command identity and committed projection publication. A rejected
    /// mutation consumes neither version nor idempotency key.
    pub fn transact_committed<F>(
        &self,
        command: &DesktopCommand,
        event_id: StableId,
        event_name: impl Into<String>,
        event_payload: Value,
        apply_domain: F,
    ) -> Result<DesktopTransactionV1, DesktopApiError>
    where
        F: FnOnce() -> Result<(), String>,
    {
        validate_command(command)?;
        let event_name = event_name.into();
        validate_event_publication(&event_name, &event_payload)?;
        let command_digest = content_hash(command)?;
        let transaction_digest = content_hash(&(
            command,
            event_id.as_str(),
            event_name.as_str(),
            &event_payload,
        ))?;
        let mut state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        if let Some(previous) = state.accepted_commands.get(command.command_id.as_str()) {
            if previous.command_digest != command_digest
                || previous.transaction_digest != transaction_digest
            {
                return Err(DesktopApiError::CommandIdentityConflict);
            }
            return Ok(DesktopTransactionV1 {
                receipt: previous.receipt.clone(),
                event: previous.event.clone(),
                duplicate: true,
            });
        }
        if state.version != command.expected_version {
            return Err(DesktopApiError::VersionConflict {
                expected: command.expected_version,
                actual: state.version,
            });
        }
        if state.event_digests.contains_key(event_id.as_str()) {
            return Err(DesktopApiError::EventIdentityConflict);
        }
        if state.events.len() >= 100_000 {
            return Err(DesktopApiError::ProjectionBackpressure);
        }
        apply_domain().map_err(DesktopApiError::DomainRejected)?;
        state.version = state
            .version
            .checked_add(1)
            .ok_or(DesktopApiError::VersionExhausted)?;
        let event = DesktopEvent {
            sequence: state.version,
            event_id: event_id.clone(),
            name: event_name,
            payload: event_payload,
        };
        let event_digest = content_hash(&(event.name.as_str(), &event.payload))?;
        let receipt = DesktopReceipt {
            command_id: command.command_id.clone(),
            accepted_for_processing: true,
            current_version: state.version,
        };
        state.events.push(event.clone());
        state
            .event_digests
            .insert(event_id.as_str().to_owned(), event_digest);
        state.accepted_commands.insert(
            command.command_id.as_str().to_owned(),
            AcceptedCommand {
                command_digest,
                transaction_digest,
                receipt: receipt.clone(),
                event: event.clone(),
            },
        );
        Ok(DesktopTransactionV1 {
            receipt,
            event,
            duplicate: false,
        })
    }

    /// Publishes only a committed domain event and advances the observed version.
    pub fn publish_committed(
        &self,
        event_id: StableId,
        name: impl Into<String>,
        payload: Value,
    ) -> Result<DesktopEvent, DesktopApiError> {
        let name = name.into();
        validate_event_publication(&name, &payload)?;
        let mut state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        let digest = content_hash(&(name.as_str(), &payload))?;
        if let Some(previous) = state.event_digests.get(event_id.as_str()) {
            if previous != &digest {
                return Err(DesktopApiError::EventIdentityConflict);
            }
            return state
                .events
                .iter()
                .find(|event| event.event_id == event_id)
                .cloned()
                .ok_or(DesktopApiError::EventIdentityConflict);
        }
        if state.events.len() >= 100_000 {
            return Err(DesktopApiError::ProjectionBackpressure);
        }
        state.version = state
            .version
            .checked_add(1)
            .ok_or(DesktopApiError::VersionExhausted)?;
        let event = DesktopEvent {
            sequence: state.version,
            event_id: event_id.clone(),
            name,
            payload,
        };
        state.events.push(event.clone());
        state
            .event_digests
            .insert(event_id.as_str().to_owned(), digest);
        Ok(event)
    }

    /// Returns a snapshot of events committed after the supplied cursor.
    pub fn snapshot_after(&self, last_sequence: u64) -> Result<DesktopSnapshot, DesktopApiError> {
        self.snapshot_page_after(last_sequence, 10_000)
    }

    /// Bounded ordered projection page. A cursor ahead of the committed head is
    /// rejected instead of silently returning a misleading empty projection.
    pub fn snapshot_page_after(
        &self,
        last_sequence: u64,
        limit: u32,
    ) -> Result<DesktopSnapshot, DesktopApiError> {
        if limit == 0 || limit > 10_000 {
            return Err(DesktopApiError::InvalidPageLimit);
        }
        let state = self.state.lock().map_err(|_| DesktopApiError::Poisoned)?;
        if last_sequence > state.version {
            return Err(DesktopApiError::CursorAhead {
                cursor: last_sequence,
                head: state.version,
            });
        }
        let events = state
            .events
            .iter()
            .filter(|event| event.sequence > last_sequence)
            .take(limit as usize)
            .cloned()
            .collect::<Vec<_>>();
        let page_cursor = events.last().map_or(last_sequence, |event| event.sequence);
        Ok(DesktopSnapshot {
            version: state.version,
            last_sequence: page_cursor,
            events,
        })
    }
}

fn validate_command(command: &DesktopCommand) -> Result<(), DesktopApiError> {
    validate_text(&command.name, 256)?;
    if serde_json::to_vec(&command.payload)
        .map_err(|_| DesktopApiError::InvalidPayload)?
        .len()
        > 1024 * 1024
    {
        return Err(DesktopApiError::InvalidPayload);
    }
    Ok(())
}

fn validate_event_publication(name: &str, payload: &Value) -> Result<(), DesktopApiError> {
    validate_text(name, 256)?;
    if serde_json::to_vec(payload)
        .map_err(|_| DesktopApiError::InvalidPayload)?
        .len()
        > 1024 * 1024
    {
        return Err(DesktopApiError::InvalidPayload);
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize) -> Result<(), DesktopApiError> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(DesktopApiError::InvalidText)
    } else {
        Ok(())
    }
}

fn content_hash<T: Serialize>(value: &T) -> Result<String, DesktopApiError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| DesktopApiError::InvalidPayload)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
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
    #[error("desktop command ID was reused with different content")]
    CommandIdentityConflict,
    #[error("desktop event ID was reused with different content")]
    EventIdentityConflict,
    #[error("desktop text field is empty, oversized, or contains controls")]
    InvalidText,
    #[error("desktop payload is malformed or exceeds one MiB")]
    InvalidPayload,
    #[error("desktop projection subscriber exceeded its bounded retained queue")]
    ProjectionBackpressure,
    #[error("desktop projection page limit is outside 1..=10000")]
    InvalidPageLimit,
    #[error("desktop cursor {cursor} is ahead of committed head {head}")]
    CursorAhead { cursor: u64, head: u64 },
    #[error("domain command was rejected: {0}")]
    DomainRejected(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum CoreServiceRequestKindV1 {
    Accept(DesktopCommand),
    PublishCommitted {
        event_id: StableId,
        name: String,
        payload: Value,
    },
    SnapshotAfter {
        cursor: u64,
        limit: u32,
    },
    Ping,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoreServiceRequestV1 {
    pub message_id: StableId,
    pub generation: aworkit_protocol::ProcessGeneration,
    pub request: CoreServiceRequestKindV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum CoreServiceResponseKindV1 {
    Receipt(DesktopReceipt),
    Event(DesktopEvent),
    Snapshot(DesktopSnapshot),
    Pong,
    ShutdownAck,
    Error { code: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoreServiceResponseV1 {
    pub message_id: StableId,
    pub generation: aworkit_protocol::ProcessGeneration,
    pub response: CoreServiceResponseKindV1,
}

/// Runs the trusted-core's bounded diagnostic/service process surface. Domain
/// completion still comes only from committed events; command receipts remain
/// admission receipts.
pub fn serve_core_stdio<R: Read, W: Write>(mut input: R, mut output: W) -> Result<(), String> {
    let api = DesktopApi::default();
    loop {
        let Some(frame) = read_frame(&mut input)? else {
            return Ok(());
        };
        let request: CoreServiceRequestV1 =
            aworkit_protocol::decode_frame(&frame).map_err(|error| error.to_string())?;
        let (response, shutdown) = match request.request {
            CoreServiceRequestKindV1::Accept(command) => match api.accept(&command) {
                Ok(receipt) => (CoreServiceResponseKindV1::Receipt(receipt), false),
                Err(error) => service_error(error),
            },
            CoreServiceRequestKindV1::PublishCommitted {
                event_id,
                name,
                payload,
            } => match api.publish_committed(event_id, name, payload) {
                Ok(event) => (CoreServiceResponseKindV1::Event(event), false),
                Err(error) => service_error(error),
            },
            CoreServiceRequestKindV1::SnapshotAfter { cursor, limit } => {
                match api.snapshot_page_after(cursor, limit) {
                    Ok(snapshot) => (CoreServiceResponseKindV1::Snapshot(snapshot), false),
                    Err(error) => service_error(error),
                }
            }
            CoreServiceRequestKindV1::Ping => (CoreServiceResponseKindV1::Pong, false),
            CoreServiceRequestKindV1::Shutdown => (CoreServiceResponseKindV1::ShutdownAck, true),
        };
        let response = CoreServiceResponseV1 {
            message_id: request.message_id,
            generation: request.generation,
            response,
        };
        output
            .write_all(
                &aworkit_protocol::encode_frame(&response).map_err(|error| error.to_string())?,
            )
            .and_then(|()| output.flush())
            .map_err(|error| error.to_string())?;
        if shutdown {
            return Ok(());
        }
    }
}

fn service_error(error: DesktopApiError) -> (CoreServiceResponseKindV1, bool) {
    (
        CoreServiceResponseKindV1::Error {
            code: "desktop_api_rejected".to_owned(),
            message: error.to_string(),
        },
        false,
    )
}

fn read_frame<R: Read>(input: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut prefix = [0_u8; 4];
    let mut read = 0;
    while read < prefix.len() {
        let count = input
            .read(&mut prefix[read..])
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return if read == 0 {
                Ok(None)
            } else {
                Err("truncated core frame prefix".to_owned())
            };
        }
        read += count;
    }
    let body_length = u32::from_be_bytes(prefix) as usize;
    if body_length > aworkit_protocol::MAX_FRAME_BYTES {
        return Err("core frame exceeds one MiB".to_owned());
    }
    let mut frame = Vec::with_capacity(4 + body_length);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + body_length, 0);
    input
        .read_exact(&mut frame[4..])
        .map_err(|error| error.to_string())?;
    Ok(Some(frame))
}
