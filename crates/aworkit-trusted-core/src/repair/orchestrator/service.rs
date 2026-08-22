//! Orchestrator construction, durable loading, and shared append helpers.

use std::sync::Arc;

use aworkit_protocol::StableId;
use sha2::{Digest, Sha256};

use super::super::validation::validate_capability_report_fresh;
use super::super::{
    CoreQuiescencePortV1, ManagementCheckpointPortV1, PlatformCapabilityReportV1,
    RepairAggregateV1, RepairArtifactIntegrityPortV1, RepairBootstrapPortV1, RepairCandidateV1,
    RepairEventV1, RepairInvestigationPortV1, RepairLedgerAppendOutcomeV1, RepairLedgerAppendV1,
    RepairLedgerOperationV1, RepairLedgerPortV1, RepairPortErrorV1,
};
use super::error::RepairError;

/// Trusted-core repair service assembled from narrow process-neutral ports.
pub struct RepairOrchestratorV1 {
    pub(super) ledger: Arc<dyn RepairLedgerPortV1>,
    pub(super) bootstrap: Arc<dyn RepairBootstrapPortV1>,
    pub(super) investigations: Arc<dyn RepairInvestigationPortV1>,
    pub(super) management: Arc<dyn ManagementCheckpointPortV1>,
    pub(super) quiescence: Arc<dyn CoreQuiescencePortV1>,
    pub(super) artifacts: Arc<dyn RepairArtifactIntegrityPortV1>,
}

impl RepairOrchestratorV1 {
    #[must_use]
    pub fn new(
        ledger: Arc<dyn RepairLedgerPortV1>,
        bootstrap: Arc<dyn RepairBootstrapPortV1>,
        investigations: Arc<dyn RepairInvestigationPortV1>,
        management: Arc<dyn ManagementCheckpointPortV1>,
        quiescence: Arc<dyn CoreQuiescencePortV1>,
        artifacts: Arc<dyn RepairArtifactIntegrityPortV1>,
    ) -> Self {
        Self {
            ledger,
            bootstrap,
            investigations,
            management,
            quiescence,
            artifacts,
        }
    }

    /// Returns the core-owned projection after validating the entire stream.
    pub fn load_aggregate(&self, group_id: &StableId) -> Result<RepairAggregateV1, RepairError> {
        let events = self
            .ledger
            .load_group(group_id)
            .map_err(|source| port_error("repair ledger read", source))?;
        RepairAggregateV1::rehydrate(group_id.clone(), &events).map_err(Into::into)
    }

    pub(super) fn append_and_reload(
        &self,
        aggregate: &RepairAggregateV1,
        operation_id: StableId,
        events: Vec<RepairEventV1>,
    ) -> Result<AppendResult, RepairError> {
        if events.is_empty() {
            return Err(RepairError::InvalidContract(
                "repair ledger append cannot be empty",
            ));
        }
        if let Some(existing) = self.load_operation(&aggregate.group_id, &operation_id)? {
            if existing.events != events {
                return Err(RepairError::OperationConflict);
            }
            return Ok(AppendResult {
                aggregate: self.load_aggregate(&aggregate.group_id)?,
                duplicate: true,
            });
        }
        let preview = aggregate.preview(&events)?;
        let outcome = self
            .ledger
            .append(RepairLedgerAppendV1 {
                operation_id,
                group_id: aggregate.group_id.clone(),
                expected_ledger_version: aggregate.ledger_version,
                events,
            })
            .map_err(|source| port_error("repair ledger append", source))?;
        validate_append_outcome(aggregate, &preview, &outcome)?;
        let reloaded = self.load_aggregate(&aggregate.group_id)?;
        if reloaded.ledger_version < preview.ledger_version {
            return Err(RepairError::InvalidContract(
                "repair ledger acknowledgement is not durably readable",
            ));
        }
        Ok(AppendResult {
            aggregate: reloaded,
            duplicate: outcome.duplicate,
        })
    }

    pub(super) fn load_operation(
        &self,
        group_id: &StableId,
        operation_id: &StableId,
    ) -> Result<Option<RepairLedgerOperationV1>, RepairError> {
        let indexed = self
            .ledger
            .load_operation(group_id, operation_id)
            .map_err(|source| port_error("repair operation lookup", source))?;
        let committed = self
            .ledger
            .load_group(group_id)
            .map_err(|source| port_error("repair ledger read", source))?;
        RepairAggregateV1::rehydrate(group_id.clone(), &committed)?;
        let events = committed
            .into_iter()
            .filter(|event| event.operation_id == *operation_id)
            .map(|event| event.event)
            .collect::<Vec<_>>();
        let canonical = if events.is_empty() {
            None
        } else {
            Some(RepairLedgerOperationV1 {
                operation_id: operation_id.clone(),
                group_id: group_id.clone(),
                events,
            })
        };
        if indexed != canonical {
            return Err(RepairError::InvalidContract(
                "repair operation index does not match the committed stream",
            ));
        }
        Ok(canonical)
    }
}

pub(super) struct AppendResult {
    pub(super) aggregate: RepairAggregateV1,
    pub(super) duplicate: bool,
}

pub(super) fn ensure_version(
    aggregate: &RepairAggregateV1,
    expected: u64,
) -> Result<(), RepairError> {
    if aggregate.ledger_version == expected {
        Ok(())
    } else {
        Err(RepairError::StaleLedgerVersion {
            expected,
            actual: aggregate.ledger_version,
        })
    }
}

pub(super) fn active_candidate_exact<'a>(
    aggregate: &'a RepairAggregateV1,
    candidate_id: &StableId,
    version: u64,
    hash: &str,
) -> Result<&'a RepairCandidateV1, RepairError> {
    aggregate
        .active_candidate()
        .filter(|candidate| {
            candidate.candidate_id == *candidate_id
                && candidate.candidate_version == version
                && candidate.candidate_hash == hash
        })
        .ok_or(RepairError::CandidateMismatch)
}

pub(super) fn exact_report<'a>(
    aggregate: &'a RepairAggregateV1,
    report_id: &StableId,
    digest: &str,
    now_epoch_ms: u64,
) -> Result<&'a PlatformCapabilityReportV1, RepairError> {
    let report =
        aggregate
            .latest_capability_report
            .as_ref()
            .ok_or(RepairError::InvalidContract(
                "activation capability has not been queried",
            ))?;
    if report.report_id != *report_id || report.capability_digest != digest {
        return Err(RepairError::InvalidContract(
            "activation capability decision is stale",
        ));
    }
    validate_capability_report_fresh(report, now_epoch_ms).map_err(RepairError::InvalidContract)?;
    Ok(report)
}

fn validate_append_outcome(
    before: &RepairAggregateV1,
    preview: &RepairAggregateV1,
    outcome: &RepairLedgerAppendOutcomeV1,
) -> Result<(), RepairError> {
    let valid = if outcome.duplicate {
        outcome.ledger_version >= before.ledger_version
    } else {
        outcome.ledger_version == preview.ledger_version
    };
    if valid {
        Ok(())
    } else {
        Err(RepairError::InvalidContract(
            "repair ledger returned an invalid append acknowledgement",
        ))
    }
}

pub(super) fn derived_id(prefix: &str, source: &StableId) -> Result<StableId, RepairError> {
    let digest = format!("{:x}", Sha256::digest(source.as_str().as_bytes()));
    StableId::parse(format!("repair.{prefix}.{}", &digest[..24]))
        .map_err(|_| RepairError::InvalidContract("derived repair id is invalid"))
}

pub(super) fn port_error(boundary: &'static str, source: RepairPortErrorV1) -> RepairError {
    RepairError::Port { boundary, source }
}
