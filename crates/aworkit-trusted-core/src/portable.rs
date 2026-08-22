//! Core-owned portable commit acknowledgement and recovery gate.
//!
//! Immutable bytes may be prepared first, but a receipt is acknowledged only
//! after expected-head publication is independently verified and the exact
//! machine-local runtime fence is durably linked.

use std::sync::Arc;

use aworkit_protocol::{
    HistoryPortErrorV1, PortableCanonicalCommitPort, PortableCommitReceiptV1, PortablePortErrorV1,
    PortablePrepareV1, PortableRuntimeBeginV1, PortableRuntimeFactsV1, PortableRuntimeFinalizeV1,
    PortableRuntimeJournalPort, StableId,
};
use thiserror::Error;

#[derive(Clone)]
pub struct PortableCommitGate {
    repository: Arc<dyn PortableCanonicalCommitPort>,
    journal: Arc<dyn PortableRuntimeJournalPort>,
    machine_instance_id: StableId,
    binding_generation: u64,
}

impl PortableCommitGate {
    #[must_use]
    pub fn new<R, J>(
        repository: R,
        journal: J,
        machine_instance_id: StableId,
        binding_generation: u64,
    ) -> Self
    where
        R: PortableCanonicalCommitPort + 'static,
        J: PortableRuntimeJournalPort + 'static,
    {
        Self::from_ports(
            Arc::new(repository),
            Arc::new(journal),
            machine_instance_id,
            binding_generation,
        )
    }

    #[must_use]
    pub fn from_ports(
        repository: Arc<dyn PortableCanonicalCommitPort>,
        journal: Arc<dyn PortableRuntimeJournalPort>,
        machine_instance_id: StableId,
        binding_generation: u64,
    ) -> Self {
        Self {
            repository,
            journal,
            machine_instance_id,
            binding_generation,
        }
    }

    /// Prepares immutable data, records pending operational state, publishes,
    /// verifies by reread, and only then links and acknowledges the head.
    pub fn commit(
        &self,
        request: &PortablePrepareV1,
    ) -> Result<PortableCommitReceiptV1, PortableGateError> {
        let prepared = self.repository.prepare(request).map_err(canonical_error)?;
        if prepared.operation_id != request.operation_id
            || prepared.commit_id != request.operation_id
            || prepared.expected_generation != request.expected_generation
        {
            return Err(PortableGateError::FenceMismatch);
        }
        let begin = PortableRuntimeBeginV1 {
            operation_id: request.operation_id.clone(),
            machine_instance_id: self.machine_instance_id.clone(),
            binding_generation: self.binding_generation,
            expected_generation: request.expected_generation,
            chat_id: request.chat_id.clone(),
            branch_id: request.branch_id.clone(),
            commit_id: prepared.commit_id.clone(),
            expected_head_hash: request.expected_head_hash.clone(),
            candidate_head_hash: prepared.object_hash.clone(),
            checkpoint_hash: request.checkpoint_hash.clone(),
        };
        self.journal.begin(&begin).map_err(journal_error)?;
        let published = match self.repository.publish(&prepared) {
            Ok(receipt) => receipt,
            Err(error) if error.uncertain_publication => self
                .repository
                .read_head(&request.branch_id)
                .map_err(canonical_error)?
                .filter(|receipt| receipt_matches_begin(receipt, &begin, request))
                .ok_or_else(|| canonical_error(error))?,
            Err(error) => return Err(canonical_error(error)),
        };
        validate_receipt(&published, &begin, request)?;
        let verified = self
            .repository
            .verify(&published)
            .map_err(canonical_error)?;
        if verified != published {
            return Err(PortableGateError::FenceMismatch);
        }
        self.journal
            .finalize(&PortableRuntimeFinalizeV1 {
                operation_id: request.operation_id.clone(),
                verified_receipt: verified.clone(),
            })
            .map_err(journal_error)?;
        match self
            .journal
            .facts(&request.operation_id)
            .map_err(journal_error)?
        {
            Some(PortableRuntimeFactsV1::HeadLinked {
                begin: durable_begin,
                receipt,
            }) if durable_begin == begin && receipt == verified => Ok(verified),
            _ => Err(PortableGateError::JournalNotLinked),
        }
    }

    /// Reconciles a crash without dispatching or republishing. A pending record
    /// is finalized only if the current head already proves the exact candidate.
    pub fn recover(
        &self,
        operation_id: &StableId,
    ) -> Result<PortableRecoveryFacts, PortableGateError> {
        let facts = self
            .journal
            .facts(operation_id)
            .map_err(journal_error)?
            .ok_or(PortableGateError::Quarantined)?;
        match facts {
            PortableRuntimeFactsV1::HeadLinked { begin, receipt } => {
                let head = self
                    .repository
                    .read_head(&begin.branch_id)
                    .map_err(canonical_error)?;
                let resumable = runtime_matches(&begin, &receipt, self) && head == Some(receipt);
                if !resumable {
                    self.journal
                        .quarantine(
                            operation_id,
                            "linked publication no longer matches the runtime binding and head",
                        )
                        .map_err(journal_error)?;
                }
                Ok(PortableRecoveryFacts {
                    resumable,
                    quarantined: !resumable,
                    branch_id: begin.branch_id.as_str().to_owned(),
                })
            }
            PortableRuntimeFactsV1::Pending { begin } => {
                if begin.machine_instance_id != self.machine_instance_id
                    || begin.binding_generation != self.binding_generation
                {
                    self.journal
                        .quarantine(
                            operation_id,
                            "pending publication belongs to a stale runtime binding",
                        )
                        .map_err(journal_error)?;
                    return Ok(PortableRecoveryFacts {
                        resumable: false,
                        quarantined: true,
                        branch_id: begin.branch_id.as_str().to_owned(),
                    });
                }
                let head = self
                    .repository
                    .read_head(&begin.branch_id)
                    .map_err(canonical_error)?;
                if let Some(receipt) = head.filter(|receipt| {
                    receipt.operation_id == begin.operation_id
                        && receipt.commit_id == begin.commit_id
                        && receipt.previous_head_hash == begin.expected_head_hash
                        && receipt.published_head_hash == begin.candidate_head_hash
                        && receipt.checkpoint_hash == begin.checkpoint_hash
                        && receipt.generation == begin.expected_generation.saturating_add(1)
                }) {
                    let verified = self.repository.verify(&receipt).map_err(canonical_error)?;
                    self.journal
                        .finalize(&PortableRuntimeFinalizeV1 {
                            operation_id: operation_id.clone(),
                            verified_receipt: verified,
                        })
                        .map_err(journal_error)?;
                    return Ok(PortableRecoveryFacts {
                        resumable: true,
                        quarantined: false,
                        branch_id: begin.branch_id.as_str().to_owned(),
                    });
                }
                self.journal
                    .quarantine(
                        operation_id,
                        "pending publication has no exact verified head",
                    )
                    .map_err(journal_error)?;
                Ok(PortableRecoveryFacts {
                    resumable: false,
                    quarantined: true,
                    branch_id: begin.branch_id.as_str().to_owned(),
                })
            }
            PortableRuntimeFactsV1::Quarantined { begin, .. } => Ok(PortableRecoveryFacts {
                resumable: false,
                quarantined: true,
                branch_id: begin.branch_id.as_str().to_owned(),
            }),
        }
    }

    /// Compatibility recovery query for a linked operation.
    pub fn recovery_facts(
        &self,
        operation_id: &str,
        expected_head_hash: &str,
    ) -> Result<PortableRecoveryFacts, PortableGateError> {
        let operation_id =
            StableId::parse(operation_id).map_err(|_| PortableGateError::FenceMismatch)?;
        let facts = self.recover(&operation_id)?;
        let head_matches = self
            .journal
            .facts(&operation_id)
            .map_err(journal_error)?
            .is_some_and(|facts| match facts {
                PortableRuntimeFactsV1::HeadLinked { receipt, .. } => {
                    receipt.published_head_hash == expected_head_hash
                }
                _ => false,
            });
        Ok(PortableRecoveryFacts {
            resumable: facts.resumable && head_matches,
            quarantined: facts.quarantined || !head_matches,
            branch_id: facts.branch_id,
        })
    }

    pub fn read_head(
        &self,
        branch_id: &StableId,
    ) -> Result<Option<PortableCommitReceiptV1>, PortableGateError> {
        self.repository
            .read_head(branch_id)
            .map_err(canonical_error)
    }
}

fn validate_receipt(
    receipt: &PortableCommitReceiptV1,
    begin: &PortableRuntimeBeginV1,
    request: &PortablePrepareV1,
) -> Result<(), PortableGateError> {
    if receipt_matches_begin(receipt, begin, request) {
        Ok(())
    } else {
        Err(PortableGateError::FenceMismatch)
    }
}

fn receipt_matches_begin(
    receipt: &PortableCommitReceiptV1,
    begin: &PortableRuntimeBeginV1,
    request: &PortablePrepareV1,
) -> bool {
    receipt.operation_id == begin.operation_id
        && receipt.commit_id == begin.commit_id
        && receipt.branch_id == begin.branch_id
        && receipt.previous_head_hash == begin.expected_head_hash
        && receipt.published_head_hash == begin.candidate_head_hash
        && receipt.checkpoint_hash == begin.checkpoint_hash
        && receipt.generation == request.expected_generation.saturating_add(1)
}

fn runtime_matches(
    begin: &PortableRuntimeBeginV1,
    receipt: &PortableCommitReceiptV1,
    gate: &PortableCommitGate,
) -> bool {
    begin.machine_instance_id == gate.machine_instance_id
        && begin.binding_generation == gate.binding_generation
        && receipt.generation == begin.expected_generation.saturating_add(1)
        && receipt.operation_id == begin.operation_id
        && receipt.commit_id == begin.commit_id
        && receipt.branch_id == begin.branch_id
        && receipt.previous_head_hash == begin.expected_head_hash
        && receipt.published_head_hash == begin.candidate_head_hash
        && receipt.checkpoint_hash == begin.checkpoint_hash
}

fn canonical_error(error: PortablePortErrorV1) -> PortableGateError {
    PortableGateError::Canonical {
        code: error.code,
        message: error.message,
        uncertain_publication: error.uncertain_publication,
    }
}

fn journal_error(error: HistoryPortErrorV1) -> PortableGateError {
    PortableGateError::Journal {
        code: error.code,
        message: error.message,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableRecoveryFacts {
    pub resumable: bool,
    pub quarantined: bool,
    pub branch_id: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PortableGateError {
    #[error("portable continuation is quarantined")]
    Quarantined,
    #[error("portable publication receipt does not match every expected fence")]
    FenceMismatch,
    #[error("portable receipt was not durably linked to its runtime journal")]
    JournalNotLinked,
    #[error("portable canonical store error {code}: {message}")]
    Canonical {
        code: String,
        message: String,
        uncertain_publication: bool,
    },
    #[error("portable runtime journal error {code}: {message}")]
    Journal { code: String, message: String },
}
