//! Core-owned portable commit acknowledgement gate.
//!
//! The portable repository may prepare and publish immutable objects, but this
//! gate makes their receipt usable only after the local operational journal has
//! durably linked it. No method here dispatches an effect or replays one.

use aworkit_local_store::{
    PortableJournalError, PortableJournalPhase, PortableJournalRecord, PortableRuntimeJournal,
};
use aworkit_portable_store::{CommitError, CommitReceipt, PortableCommit, PortableRepository};
use thiserror::Error;

/// Core composition root for the portable project's two-store acknowledgement.
#[derive(Clone)]
pub struct PortableCommitGate {
    repository: PortableRepository,
    journal: PortableRuntimeJournal,
    machine_instance_id: String,
    binding_generation: u64,
}
impl PortableCommitGate {
    #[must_use]
    pub fn new(
        repository: PortableRepository,
        journal: PortableRuntimeJournal,
        machine_instance_id: impl Into<String>,
        binding_generation: u64,
    ) -> Self {
        Self {
            repository,
            journal,
            machine_instance_id: machine_instance_id.into(),
            binding_generation,
        }
    }
    /// Records pending operational state, publishes, verifies, and only then links the head.
    pub fn commit(&self, commit: &PortableCommit) -> Result<CommitReceipt, PortableGateError> {
        self.journal.record_pending(&PortableJournalRecord {
            chat_id: commit
                .events
                .first()
                .map(|event| event.chat_id.clone())
                .ok_or(PortableGateError::EmptyCommit)?,
            branch_id: commit.branch_id.clone(),
            commit_id: commit.commit_id.clone(),
            machine_instance_id: self.machine_instance_id.clone(),
            binding_generation: self.binding_generation,
            expected_head_generation: commit.expected_generation,
            head_segment_hash: None,
            phase: PortableJournalPhase::PendingPortableCommit,
        })?;
        let receipt = self.repository.prepare_publish_verify(commit)?;
        self.journal
            .link_head(&commit.commit_id, &receipt.head_segment_hash)?;
        Ok(receipt)
    }
    /// Returns recoverable operational facts only for an exact same-installation fence match.
    pub fn recovery_facts(
        &self,
        commit_id: &str,
        expected_head_hash: &str,
    ) -> Result<PortableRecoveryFacts, PortableGateError> {
        let record = self
            .journal
            .get(commit_id)?
            .ok_or(PortableGateError::Quarantined)?;
        let valid = record.phase == PortableJournalPhase::HeadLinked
            && record.machine_instance_id == self.machine_instance_id
            && record.binding_generation == self.binding_generation
            && record.head_segment_hash.as_deref() == Some(expected_head_hash);
        Ok(PortableRecoveryFacts {
            resumable: valid,
            quarantined: !valid,
            branch_id: record.branch_id,
        })
    }
}
/// A generic recovery coordinator can resume only when every portable fence matches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableRecoveryFacts {
    pub resumable: bool,
    pub quarantined: bool,
    pub branch_id: String,
}
#[derive(Debug, Error)]
pub enum PortableGateError {
    #[error("portable commits require at least one event")]
    EmptyCommit,
    #[error("portable continuation is quarantined due to a missing or mismatched runtime journal")]
    Quarantined,
    #[error(transparent)]
    Journal(#[from] PortableJournalError),
    #[error(transparent)]
    Commit(#[from] CommitError),
}
