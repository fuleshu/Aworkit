//! Generation-fenced worker process supervision and core-only IPC admission.

use std::collections::BTreeMap;

use aworkit_protocol::{ProcessGeneration, StableId};
use thiserror::Error;

use crate::FrozenRunSnapshot;

/// A worker command that is safe to deliver only after its semantic commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerControl { Resume, Pause, Cancel, Input { input_id: StableId }, ApprovalGranted { approval_id: StableId } }

/// Authenticated handshake facts emitted by a freshly launched worker generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerHandshake { pub chat_id: StableId, pub generation: ProcessGeneration, pub snapshot_hash: String }

#[derive(Clone, Debug)]
struct WorkerRecord { generation: ProcessGeneration, snapshot_hash: String, healthy: bool, restarts: u32 }

/// Tracks worker generations; platform spawning remains behind its narrow process adapter.
#[derive(Default)]
pub struct WorkerSupervisor { workers: BTreeMap<String, WorkerRecord>, max_restarts: u32 }

impl WorkerSupervisor {
    #[must_use]
    pub fn with_restart_budget(max_restarts: u32) -> Self { Self { workers: BTreeMap::new(), max_restarts } }

    /// Allocates a new core-owned generation for the immutable snapshot.
    pub fn start(&mut self, snapshot: &FrozenRunSnapshot) -> Result<WorkerHandshake, WorkerSupervisorError> {
        let generation = match self.workers.get(snapshot.chat_id.as_str()) { Some(record) => ProcessGeneration(record.generation.0.checked_add(1).ok_or(WorkerSupervisorError::GenerationExhausted)?), None => ProcessGeneration(1) };
        let restarts = self.workers.get(snapshot.chat_id.as_str()).map_or(0, |record| record.restarts);
        if restarts > self.max_restarts { return Err(WorkerSupervisorError::RestartBudgetExhausted); }
        self.workers.insert(snapshot.chat_id.as_str().to_owned(), WorkerRecord { generation, snapshot_hash: snapshot.snapshot_hash.clone(), healthy: false, restarts });
        Ok(WorkerHandshake { chat_id: snapshot.chat_id.clone(), generation, snapshot_hash: snapshot.snapshot_hash.clone() })
    }

    /// Admits the worker's handshake only when it proves the exact frozen identity.
    pub fn acknowledge_handshake(&mut self, handshake: &WorkerHandshake) -> Result<(), WorkerSupervisorError> {
        let record = self.workers.get_mut(handshake.chat_id.as_str()).ok_or(WorkerSupervisorError::UnknownWorker)?;
        if record.generation != handshake.generation || record.snapshot_hash != handshake.snapshot_hash { return Err(WorkerSupervisorError::StaleHandshake); }
        record.healthy = true;
        Ok(())
    }

    /// Validates that a committed control cannot reach a stale/unhealthy worker.
    pub fn deliver(&self, chat_id: &StableId, generation: ProcessGeneration, _control: &WorkerControl) -> Result<(), WorkerSupervisorError> {
        let record = self.workers.get(chat_id.as_str()).ok_or(WorkerSupervisorError::UnknownWorker)?;
        if record.generation != generation || !record.healthy { return Err(WorkerSupervisorError::StaleHandshake); }
        Ok(())
    }

    /// Marks a dead worker and consumes one bounded replacement opportunity.
    pub fn crashed(&mut self, chat_id: &StableId, generation: ProcessGeneration) -> Result<(), WorkerSupervisorError> {
        let record = self.workers.get_mut(chat_id.as_str()).ok_or(WorkerSupervisorError::UnknownWorker)?;
        if record.generation != generation { return Err(WorkerSupervisorError::StaleHandshake); }
        record.healthy = false;
        record.restarts = record.restarts.checked_add(1).ok_or(WorkerSupervisorError::RestartBudgetExhausted)?;
        if record.restarts > self.max_restarts { return Err(WorkerSupervisorError::RestartBudgetExhausted); }
        Ok(())
    }
}

/// A supervisor rejects stale generations instead of risking cross-Run effects.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum WorkerSupervisorError {
    #[error("unknown worker Chat")]
    UnknownWorker,
    #[error("stale or unauthenticated worker generation")]
    StaleHandshake,
    #[error("worker generation counter is exhausted")]
    GenerationExhausted,
    #[error("worker restart budget is exhausted")]
    RestartBudgetExhausted,
}
