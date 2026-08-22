//! Durable local-store adapter for trusted-core repair events.

use std::path::Path;

use aworkit_local_store::{
    CoreEventAppendBatchRequest, CoreEventInput, RedactionSet, RepairEvidenceLedger,
    RepairLedgerError, StoredCoreEvent,
};
use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    CommittedRepairEventV1, RepairEventV1, RepairLedgerAppendOutcomeV1, RepairLedgerAppendV1,
    RepairLedgerPortV1, RepairPortErrorV1,
};

use super::ports::{GloballyCommittedRepairEventV1, ManagementRepairProjectionPortV1};

const PAGE_SIZE: u32 = 512;
const MAX_GROUPS: usize = 16_384;
const MAX_REPLAY_EVENTS: usize = 100_000;

/// Bridges the process-neutral trusted-core ledger port to the SQLite-backed
/// opaque event store. The redaction policy is invocation-scoped and is
/// applied again immediately before each atomic persistence operation.
pub struct LocalRepairLedgerAdapter {
    store: RepairEvidenceLedger,
    redaction: RedactionSet,
}

impl LocalRepairLedgerAdapter {
    /// Opens the durable repair ledger below an application-owned data root.
    pub fn for_store_root(
        root: impl AsRef<Path>,
        redaction: RedactionSet,
    ) -> Result<Self, RepairPortErrorV1> {
        RepairEvidenceLedger::for_store_root(root)
            .map(|store| Self { store, redaction })
            .map_err(store_error)
    }

    #[cfg(test)]
    fn from_store(store: RepairEvidenceLedger, redaction: RedactionSet) -> Self {
        Self { store, redaction }
    }

    fn read_group(&self, group_id: &StableId) -> Result<Vec<StoredCoreEvent>, RepairPortErrorV1> {
        let mut cursor = 0_u64;
        let mut events = Vec::new();
        loop {
            let page = self
                .store
                .load_core_events(group_id.as_str(), cursor, PAGE_SIZE)
                .map_err(store_error)?;
            if page.is_empty() {
                break;
            }
            if events.len().saturating_add(page.len()) > MAX_REPLAY_EVENTS {
                return Err(replay_bound_error());
            }
            cursor = page
                .last()
                .map(|event| event.group_sequence)
                .ok_or_else(integrity_error)?;
            let complete = page.len() < PAGE_SIZE as usize;
            events.extend(page);
            if complete {
                break;
            }
        }
        Ok(events)
    }

    fn read_global(&self) -> Result<Vec<StoredCoreEvent>, RepairPortErrorV1> {
        let global_version = self.current_global_version()?;
        if global_version > MAX_REPLAY_EVENTS as u64 {
            return Err(replay_bound_error());
        }
        let mut cursor = 0_u64;
        let mut events = Vec::with_capacity(global_version as usize);
        while cursor < global_version {
            let page = self
                .store
                .load_all_core_events_after(cursor, PAGE_SIZE)
                .map_err(store_error)?;
            if page.is_empty() || events.len().saturating_add(page.len()) > MAX_REPLAY_EVENTS {
                return Err(integrity_error());
            }
            cursor = page
                .last()
                .map(|event| event.global_sequence)
                .ok_or_else(integrity_error)?;
            events.extend(page);
        }
        if cursor != global_version {
            return Err(integrity_error());
        }
        Ok(events)
    }
}

impl RepairLedgerPortV1 for LocalRepairLedgerAdapter {
    fn load_group(
        &self,
        group_id: &StableId,
    ) -> Result<Vec<CommittedRepairEventV1>, RepairPortErrorV1> {
        self.read_group(group_id)?
            .into_iter()
            .map(decode_committed)
            .collect()
    }

    fn append(
        &self,
        request: RepairLedgerAppendV1,
    ) -> Result<RepairLedgerAppendOutcomeV1, RepairPortErrorV1> {
        if request.events.is_empty() {
            return Err(RepairPortErrorV1 {
                code: "repair_ledger_empty_append".to_owned(),
                message: "A repair ledger append must contain at least one event.".to_owned(),
                retryable: false,
            });
        }
        let operation_id = request.operation_id.to_string();
        let events = request
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let one_based = index
                    .checked_add(1)
                    .ok_or_else(|| contract_error("repair event batch index overflowed"))?;
                Ok(CoreEventInput {
                    event_fingerprint: format!("repair.event.{operation_id}.{one_based}"),
                    // Ordering and the typed event carry authoritative time.
                    // Zero explicitly means that the adapter added no clock fact.
                    occurred_at_epoch_ms: 0,
                    event: serde_json::to_value(event).map_err(|_| {
                        contract_error("repair event could not be encoded for persistence")
                    })?,
                })
            })
            .collect::<Result<Vec<_>, RepairPortErrorV1>>()?;
        let receipt = self
            .store
            .append_core_events(
                &CoreEventAppendBatchRequest {
                    operation_id,
                    group_id: request.group_id.to_string(),
                    expected_group_sequence: request.expected_ledger_version,
                    events,
                },
                &self.redaction,
            )
            .map_err(store_error)?;
        Ok(RepairLedgerAppendOutcomeV1 {
            ledger_version: receipt.current_group_sequence,
            duplicate: receipt.duplicate,
        })
    }
}

impl ManagementRepairProjectionPortV1 for LocalRepairLedgerAdapter {
    fn group_ids(&self) -> Result<Vec<StableId>, RepairPortErrorV1> {
        let mut cursor: Option<String> = None;
        let mut groups = Vec::new();
        loop {
            let page = self
                .store
                .core_event_group_ids(cursor.as_deref(), PAGE_SIZE)
                .map_err(store_error)?;
            if page.is_empty() {
                break;
            }
            if groups.len().saturating_add(page.len()) > MAX_GROUPS {
                return Err(replay_bound_error());
            }
            let complete = page.len() < PAGE_SIZE as usize;
            for group in page {
                cursor = Some(group.clone());
                groups.push(StableId::parse(group).map_err(|_| integrity_error())?);
            }
            if complete {
                break;
            }
        }
        Ok(groups)
    }

    fn load_all_global_events(
        &self,
    ) -> Result<Vec<GloballyCommittedRepairEventV1>, RepairPortErrorV1> {
        self.read_global()?
            .into_iter()
            .map(|stored| {
                let global_sequence = stored.global_sequence;
                Ok(GloballyCommittedRepairEventV1 {
                    global_sequence,
                    committed: decode_committed(stored)?,
                })
            })
            .collect()
    }

    fn current_global_version(&self) -> Result<u64, RepairPortErrorV1> {
        self.store
            .core_event_versions("management.global.cursor")
            .map(|versions| versions.current_global_version)
            .map_err(store_error)
    }
}

fn decode_committed(stored: StoredCoreEvent) -> Result<CommittedRepairEventV1, RepairPortErrorV1> {
    let group_id = StableId::parse(stored.group_id).map_err(|_| integrity_error())?;
    let operation_id = StableId::parse(stored.operation_id).map_err(|_| integrity_error())?;
    let expected_prefix = format!("repair.event.{}.", operation_id.as_str());
    if !stored.event_fingerprint.starts_with(&expected_prefix)
        || stored.event_fingerprint[expected_prefix.len()..]
            .parse::<usize>()
            .ok()
            .filter(|index| *index > 0)
            .is_none()
    {
        return Err(integrity_error());
    }
    let event: RepairEventV1 =
        serde_json::from_str(&stored.canonical_event_json).map_err(|_| integrity_error())?;
    Ok(CommittedRepairEventV1 {
        group_id,
        ledger_sequence: stored.group_sequence,
        operation_id,
        event,
    })
}

fn store_error(error: RepairLedgerError) -> RepairPortErrorV1 {
    match error {
        RepairLedgerError::CoreEventVersionConflict { .. }
        | RepairLedgerError::VersionConflict { .. } => RepairPortErrorV1 {
            code: "repair_ledger_version_conflict".to_owned(),
            message: "The durable repair group changed before this operation committed.".to_owned(),
            retryable: true,
        },
        RepairLedgerError::OperationConflict | RepairLedgerError::IdentityConflict => {
            RepairPortErrorV1 {
                code: "repair_ledger_operation_conflict".to_owned(),
                message: "A repair operation identifier was reused with different content."
                    .to_owned(),
                retryable: false,
            }
        }
        RepairLedgerError::ForwardSchema { .. } => RepairPortErrorV1 {
            code: "repair_ledger_schema_newer".to_owned(),
            message:
                "The repair ledger was created by a newer application version and is read-only."
                    .to_owned(),
            retryable: false,
        },
        RepairLedgerError::Integrity | RepairLedgerError::Corrupt => integrity_error(),
        RepairLedgerError::ForbiddenSecretMaterial => RepairPortErrorV1 {
            code: "repair_ledger_redaction_rejected".to_owned(),
            message: "Repair evidence did not pass the persistence redaction boundary.".to_owned(),
            retryable: false,
        },
        RepairLedgerError::Io(_) | RepairLedgerError::Sql(_) | RepairLedgerError::Poisoned => {
            RepairPortErrorV1 {
                code: "repair_ledger_unavailable".to_owned(),
                message: "Durable repair persistence is temporarily unavailable.".to_owned(),
                retryable: true,
            }
        }
        RepairLedgerError::Json(_)
        | RepairLedgerError::InvalidId
        | RepairLedgerError::InvalidRecord
        | RepairLedgerError::InvalidHash
        | RepairLedgerError::UnknownGroup
        | RepairLedgerError::UnknownCandidate
        | RepairLedgerError::UnknownBaton
        | RepairLedgerError::CandidateVersionConflict
        | RepairLedgerError::InvalidTransition { .. }
        | RepairLedgerError::Ineligible(_)
        | RepairLedgerError::NumericOverflow
        | RepairLedgerError::InvalidPage => {
            contract_error("The repair event did not satisfy the durable ledger contract.")
        }
    }
}

fn integrity_error() -> RepairPortErrorV1 {
    RepairPortErrorV1 {
        code: "repair_ledger_integrity".to_owned(),
        message: "Durable repair evidence failed integrity verification.".to_owned(),
        retryable: false,
    }
}

fn replay_bound_error() -> RepairPortErrorV1 {
    RepairPortErrorV1 {
        code: "repair_ledger_replay_bound".to_owned(),
        message: "The repair history exceeds this application's bounded replay limit.".to_owned(),
        retryable: false,
    }
}

fn contract_error(message: &str) -> RepairPortErrorV1 {
    RepairPortErrorV1 {
        code: "repair_ledger_contract".to_owned(),
        message: message.to_owned(),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aworkit_trusted_core::{ErrorOccurrenceV1, RepairEventV1};

    use super::*;
    use crate::management::ManagementRepairGateway;

    fn id(value: &str) -> StableId {
        StableId::parse(value.to_owned()).expect("stable test id")
    }

    fn failure(suffix: &str, fingerprint: &str, observed_at_epoch_ms: u64) -> RepairEventV1 {
        RepairEventV1::FailureRecorded {
            occurrence: ErrorOccurrenceV1 {
                occurrence_id: id(&format!("occurrence.{suffix}")),
                fingerprint: fingerprint.to_owned(),
                summary: format!("Bounded failure {suffix}"),
                semantic_event_id: id(&format!("semantic.{suffix}")),
                attempt_id: None,
                diagnostic_record_id: None,
                evidence: Vec::new(),
                observed_at_epoch_ms,
            },
        }
    }

    #[test]
    fn atomic_batch_reopens_with_group_and_global_order() {
        let root = tempfile::tempdir().expect("temporary ledger root");
        let fingerprint = format!("sha256:{}", "a".repeat(64));
        let group_id = id(&format!("repair.group.{}", "a".repeat(64)));
        let request = RepairLedgerAppendV1 {
            operation_id: id("operation.batch.one"),
            group_id: group_id.clone(),
            expected_ledger_version: 0,
            events: vec![
                failure("one", &fingerprint, 10),
                failure("two", &fingerprint, 20),
            ],
        };

        let first = LocalRepairLedgerAdapter::for_store_root(root.path(), RedactionSet::default())
            .expect("first adapter");
        let receipt = first.append(request.clone()).expect("atomic append");
        assert_eq!(receipt.ledger_version, 2);
        assert!(!receipt.duplicate);
        drop(first);

        let reopened =
            LocalRepairLedgerAdapter::for_store_root(root.path(), RedactionSet::default())
                .expect("reopened adapter");
        let duplicate = reopened.append(request).expect("idempotent reopen retry");
        assert_eq!(duplicate.ledger_version, 2);
        assert!(duplicate.duplicate);

        let second_fingerprint = format!("sha256:{}", "b".repeat(64));
        let second_group_id = id(&format!("repair.group.{}", "b".repeat(64)));
        let second_receipt = reopened
            .append(RepairLedgerAppendV1 {
                operation_id: id("operation.batch.two"),
                group_id: second_group_id.clone(),
                expected_ledger_version: 0,
                events: vec![failure("three", &second_fingerprint, 30)],
            })
            .expect("second group append");
        assert_eq!(second_receipt.ledger_version, 1);

        let group = reopened.load_group(&group_id).expect("group replay");
        assert_eq!(group.len(), 2);
        assert_eq!(group[0].ledger_sequence, 1);
        assert_eq!(group[1].ledger_sequence, 2);
        assert_eq!(group[0].event, failure("one", &fingerprint, 10));
        assert_eq!(group[1].event, failure("two", &fingerprint, 20));

        let global = reopened.load_all_global_events().expect("global replay");
        assert_eq!(
            global
                .iter()
                .map(|event| event.global_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(reopened.current_global_version().expect("version"), 3);
        assert_eq!(
            reopened.group_ids().expect("groups"),
            vec![group_id.clone(), second_group_id.clone()]
        );

        let gateway = ManagementRepairGateway::with_durable_ledger(Arc::new(reopened));
        let projection = gateway.snapshot(0).expect("native durable projection");
        assert_eq!(projection.version, 3);
        assert_eq!(projection.last_sequence, 3);
        assert_eq!(projection.events.len(), 3);
        assert_eq!(projection.error_groups.len(), 2);
        assert_eq!(
            projection
                .error_groups
                .iter()
                .map(|group| group.occurrence_count)
                .sum::<usize>(),
            3
        );

        let delta = gateway.snapshot(2).expect("global cursor delta");
        assert_eq!(delta.version, 3);
        assert_eq!(delta.events.len(), 1);
        assert_eq!(delta.events[0].sequence, 3);
        assert_eq!(delta.error_groups.len(), 2);

        let mut reopened_gateway = ManagementRepairGateway::with_durable_ledger(Arc::new(
            LocalRepairLedgerAdapter::for_store_root(root.path(), RedactionSet::default())
                .expect("gateway reopen"),
        ));
        let durable_retry = reopened_gateway
            .command(
                crate::management::ManagementRepairCommandInput::InvestigateAndFix {
                    command_id: "operation.batch.one".to_owned(),
                    error_group_id: group_id.to_string(),
                },
                0,
            )
            .expect("persisted operation reaches trusted replay path");
        assert!(!durable_retry.accepted);
        assert!(
            durable_retry
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.contains("version conflict"))
        );
        assert_eq!(durable_retry.current_version, 3);

        let new_stale_command = reopened_gateway
            .command(
                crate::management::ManagementRepairCommandInput::InvestigateAndFix {
                    command_id: "operation.not.committed".to_owned(),
                    error_group_id: group_id.to_string(),
                },
                0,
            )
            .expect("new stale operation is explicitly rejected");
        assert!(!new_stale_command.accepted);
        assert!(
            new_stale_command
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("version conflict"))
        );

        let cross_group_reuse = reopened_gateway
            .command(
                crate::management::ManagementRepairCommandInput::InvestigateAndFix {
                    command_id: "operation.batch.one".to_owned(),
                    error_group_id: second_group_id.to_string(),
                },
                0,
            )
            .expect("cross-group ID reuse is explicitly rejected");
        assert!(!cross_group_reuse.accepted);
        assert!(
            cross_group_reuse
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("version conflict"))
        );
        assert_eq!(
            reopened_gateway
                .snapshot(0)
                .expect("unchanged projection")
                .version,
            3
        );
    }

    #[test]
    fn operation_reuse_with_different_batch_fails_closed() {
        let root = tempfile::tempdir().expect("temporary ledger root");
        let adapter = LocalRepairLedgerAdapter::from_store(
            RepairEvidenceLedger::for_store_root(root.path()).expect("store"),
            RedactionSet::default(),
        );
        let fingerprint = format!("sha256:{}", "b".repeat(64));
        let group_id = id(&format!("repair.group.{}", "b".repeat(64)));
        let base = RepairLedgerAppendV1 {
            operation_id: id("operation.conflict"),
            group_id,
            expected_ledger_version: 0,
            events: vec![failure("base", &fingerprint, 10)],
        };
        adapter.append(base.clone()).expect("first append");
        let mut changed = base;
        changed.events = vec![failure("changed", &fingerprint, 11)];
        let error = adapter.append(changed).expect_err("conflicting operation");
        assert_eq!(error.code, "repair_ledger_operation_conflict");
        assert!(!error.retryable);
    }

    #[test]
    fn stale_group_compare_and_swap_is_retryable_and_does_not_mutate() {
        let root = tempfile::tempdir().expect("temporary ledger root");
        let adapter =
            LocalRepairLedgerAdapter::for_store_root(root.path(), RedactionSet::default())
                .expect("adapter");
        let fingerprint = format!("sha256:{}", "c".repeat(64));
        let group_id = id(&format!("repair.group.{}", "c".repeat(64)));
        adapter
            .append(RepairLedgerAppendV1 {
                operation_id: id("operation.cas.first"),
                group_id: group_id.clone(),
                expected_ledger_version: 0,
                events: vec![failure("cas-first", &fingerprint, 10)],
            })
            .expect("first append");

        let error = adapter
            .append(RepairLedgerAppendV1 {
                operation_id: id("operation.cas.stale"),
                group_id: group_id.clone(),
                expected_ledger_version: 0,
                events: vec![failure("cas-stale", &fingerprint, 20)],
            })
            .expect_err("stale group CAS");
        assert_eq!(error.code, "repair_ledger_version_conflict");
        assert!(error.retryable);
        assert_eq!(adapter.load_group(&group_id).expect("group").len(), 1);
        assert_eq!(adapter.current_global_version().expect("global version"), 1);
    }
}
