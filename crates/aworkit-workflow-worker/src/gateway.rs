//! The worker's sole Trusted Core boundary and generation fence.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{
    ProcessGeneration, StableId, WorkerControlEnvelopeV1, WorkerControlKindV1,
    WorkerProposalEnvelopeV1, WorkerProposalKindV1,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DEFAULT_MAX_PENDING_PROPOSALS: usize = 1_024;
const DEFAULT_MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESERVED_CONTROL_PROPOSALS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionV1 {
    New,
    Duplicate,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum GatewayError {
    #[error("worker gateway has not accepted a start or restore envelope")]
    NotInitialized,
    #[error("worker gateway is already initialized")]
    AlreadyInitialized,
    #[error("control message ID was reused with different content")]
    MessageIdentityConflict,
    #[error("control envelope does not match the active Chat/Run identity")]
    IdentityMismatch,
    #[error("stale or unexpected process generation")]
    GenerationMismatch,
    #[error("frozen snapshot hash does not match the active Run")]
    SnapshotMismatch,
    #[error("committed cursor regressed or skipped required acknowledgement state")]
    CursorMismatch,
    #[error("proposal buffer is full; scheduling must stop until an ack arrives")]
    Backpressure,
    #[error("unknown proposal acknowledgement")]
    UnknownAcknowledgement,
    #[error("proposal sequence exhausted")]
    SequenceExhausted,
    #[error("proposal cannot be encoded")]
    Encoding,
    #[error("stable identifier construction failed")]
    InvalidIdentifier,
}

#[derive(Clone, Debug)]
struct ActiveIdentity {
    chat_id: StableId,
    run_id: StableId,
    generation: ProcessGeneration,
    snapshot_hash: String,
}

/// Stateful core gateway with a reserved inbound control lane. Proposal
/// backpressure never prevents pause/cancel/shutdown controls from being read.
#[derive(Debug)]
pub struct CoreGatewayV1 {
    identity: Option<ActiveIdentity>,
    worker_sequence: u64,
    committed_cursor: u64,
    seen_messages: BTreeMap<String, String>,
    pending: BTreeMap<String, WorkerProposalEnvelopeV1>,
    pending_bytes: usize,
    max_pending_proposals: usize,
    max_pending_bytes: usize,
    acknowledged: BTreeSet<String>,
}

impl Default for CoreGatewayV1 {
    fn default() -> Self {
        Self::with_bounds(DEFAULT_MAX_PENDING_PROPOSALS, DEFAULT_MAX_PENDING_BYTES)
    }
}

impl CoreGatewayV1 {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_bounds(max_pending_proposals: usize, max_pending_bytes: usize) -> Self {
        Self {
            identity: None,
            worker_sequence: 0,
            committed_cursor: 0,
            seen_messages: BTreeMap::new(),
            pending: BTreeMap::new(),
            pending_bytes: 0,
            max_pending_proposals,
            max_pending_bytes,
            acknowledged: BTreeSet::new(),
        }
    }

    pub fn admit_control(
        &mut self,
        envelope: &WorkerControlEnvelopeV1,
    ) -> Result<AdmissionV1, GatewayError> {
        let bytes = serde_jcs::to_vec(envelope).map_err(|_| GatewayError::Encoding)?;
        let message_hash = format!("{:x}", Sha256::digest(bytes));
        if let Some(existing) = self.seen_messages.get(envelope.message_id.as_str()) {
            if existing != &message_hash {
                return Err(GatewayError::MessageIdentityConflict);
            }
            self.validate_identity(envelope)?;
            return Ok(AdmissionV1::Duplicate);
        }
        match &envelope.control {
            WorkerControlKindV1::Start(snapshot) => {
                if self.identity.is_some() {
                    return Err(GatewayError::AlreadyInitialized);
                } else {
                    if snapshot.chat_id != envelope.chat_id
                        || snapshot.run_id != envelope.run_id
                        || snapshot.snapshot_hash != envelope.snapshot_hash
                    {
                        return Err(GatewayError::IdentityMismatch);
                    }
                    self.identity = Some(ActiveIdentity {
                        chat_id: envelope.chat_id.clone(),
                        run_id: envelope.run_id.clone(),
                        generation: envelope.generation,
                        snapshot_hash: envelope.snapshot_hash.clone(),
                    });
                    self.committed_cursor = envelope.committed_cursor;
                }
            }
            WorkerControlKindV1::Restore(rehydration) => {
                if self.identity.is_some() {
                    return Err(GatewayError::AlreadyInitialized);
                }
                if rehydration.snapshot.chat_id != envelope.chat_id
                    || rehydration.snapshot.run_id != envelope.run_id
                    || rehydration.snapshot.snapshot_hash != envelope.snapshot_hash
                    || rehydration.replacement_generation != envelope.generation
                {
                    return Err(GatewayError::IdentityMismatch);
                }
                self.identity = Some(ActiveIdentity {
                    chat_id: envelope.chat_id.clone(),
                    run_id: envelope.run_id.clone(),
                    generation: envelope.generation,
                    snapshot_hash: envelope.snapshot_hash.clone(),
                });
                self.committed_cursor = envelope.committed_cursor;
            }
            _ => self.validate_identity(envelope)?,
        }

        match &envelope.control {
            WorkerControlKindV1::CommittedAck {
                proposal_id,
                committed_cursor,
            } => self.acknowledge(proposal_id, *committed_cursor)?,
            _ if envelope.committed_cursor != self.committed_cursor => {
                return Err(GatewayError::CursorMismatch);
            }
            _ => {}
        }
        self.seen_messages
            .insert(envelope.message_id.as_str().to_owned(), message_hash);
        Ok(AdmissionV1::New)
    }

    pub fn emit(
        &mut self,
        proposal: WorkerProposalKindV1,
    ) -> Result<WorkerProposalEnvelopeV1, GatewayError> {
        self.emit_inner(proposal, false)
    }

    /// Emits a bounded response to pause/cancel/shutdown over the reserved
    /// control lane even when ordinary proposal scheduling is backpressured.
    pub fn emit_reserved(
        &mut self,
        proposal: WorkerProposalKindV1,
    ) -> Result<WorkerProposalEnvelopeV1, GatewayError> {
        self.emit_inner(proposal, true)
    }

    fn emit_inner(
        &mut self,
        proposal: WorkerProposalKindV1,
        reserved: bool,
    ) -> Result<WorkerProposalEnvelopeV1, GatewayError> {
        let identity = self.identity.as_ref().ok_or(GatewayError::NotInitialized)?;
        let hard_proposal_limit = self
            .max_pending_proposals
            .saturating_add(MAX_RESERVED_CONTROL_PROPOSALS);
        if (!reserved && self.pending.len() >= self.max_pending_proposals)
            || self.pending.len() >= hard_proposal_limit
        {
            return Err(GatewayError::Backpressure);
        }
        let next_sequence = self
            .worker_sequence
            .checked_add(1)
            .ok_or(GatewayError::SequenceExhausted)?;
        let proposal_id = stable_id(&format!(
            "proposal:{}:{}:{}:{}",
            identity.chat_id, identity.run_id, identity.generation.0, next_sequence
        ))?;
        let envelope = WorkerProposalEnvelopeV1 {
            proposal_id: proposal_id.clone(),
            chat_id: identity.chat_id.clone(),
            run_id: identity.run_id.clone(),
            generation: identity.generation,
            snapshot_hash: identity.snapshot_hash.clone(),
            worker_sequence: next_sequence,
            base_committed_cursor: self.committed_cursor,
            proposal,
        };
        let encoded = serde_json::to_vec(&envelope).map_err(|_| GatewayError::Encoding)?;
        let new_bytes = self
            .pending_bytes
            .checked_add(encoded.len())
            .ok_or(GatewayError::Backpressure)?;
        let reserved_byte_limit = self.max_pending_bytes.saturating_add(256 * 1024);
        if (!reserved && new_bytes > self.max_pending_bytes) || new_bytes > reserved_byte_limit {
            return Err(GatewayError::Backpressure);
        }
        self.pending_bytes = new_bytes;
        self.worker_sequence = next_sequence;
        self.pending
            .insert(proposal_id.as_str().to_owned(), envelope.clone());
        Ok(envelope)
    }

    /// Returns byte-for-byte equivalent logical proposals with their original
    /// IDs and sequences. Retransmission never creates a second invocation.
    #[must_use]
    pub fn retransmit_pending(&self) -> Vec<WorkerProposalEnvelopeV1> {
        let mut proposals = self.pending.values().cloned().collect::<Vec<_>>();
        proposals.sort_by_key(|proposal| proposal.worker_sequence);
        proposals
    }

    #[must_use]
    pub fn committed_cursor(&self) -> u64 {
        self.committed_cursor
    }

    #[must_use]
    pub fn generation(&self) -> Option<ProcessGeneration> {
        self.identity.as_ref().map(|identity| identity.generation)
    }

    #[must_use]
    pub fn identity(&self) -> Option<(&StableId, &StableId, &str)> {
        self.identity.as_ref().map(|identity| {
            (
                &identity.chat_id,
                &identity.run_id,
                identity.snapshot_hash.as_str(),
            )
        })
    }

    fn acknowledge(
        &mut self,
        proposal_id: &StableId,
        committed_cursor: u64,
    ) -> Result<(), GatewayError> {
        if self.acknowledged.contains(proposal_id.as_str()) {
            if committed_cursor < self.committed_cursor {
                return Err(GatewayError::CursorMismatch);
            }
            self.committed_cursor = committed_cursor;
            return Ok(());
        }
        if committed_cursor <= self.committed_cursor {
            return Err(GatewayError::CursorMismatch);
        }
        let removed = self
            .pending
            .remove(proposal_id.as_str())
            .ok_or(GatewayError::UnknownAcknowledgement)?;
        let bytes = serde_json::to_vec(&removed).map_err(|_| GatewayError::Encoding)?;
        self.pending_bytes = self.pending_bytes.saturating_sub(bytes.len());
        self.acknowledged.insert(proposal_id.as_str().to_owned());
        self.committed_cursor = committed_cursor;
        Ok(())
    }

    fn validate_identity(&self, envelope: &WorkerControlEnvelopeV1) -> Result<(), GatewayError> {
        let identity = self.identity.as_ref().ok_or(GatewayError::NotInitialized)?;
        if identity.chat_id != envelope.chat_id || identity.run_id != envelope.run_id {
            return Err(GatewayError::IdentityMismatch);
        }
        if identity.generation != envelope.generation {
            return Err(GatewayError::GenerationMismatch);
        }
        if identity.snapshot_hash != envelope.snapshot_hash {
            return Err(GatewayError::SnapshotMismatch);
        }
        Ok(())
    }
}

fn stable_id(material: &str) -> Result<StableId, GatewayError> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    StableId::parse(format!("gateway.{}", &digest[..48]))
        .map_err(|_| GatewayError::InvalidIdentifier)
}
