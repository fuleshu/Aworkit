use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use super::*;
use crate::RedactionSet;

fn root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("aworkit-repair-{label}-{nonce}"))
}

fn hash(label: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(label.as_bytes()))
}

fn evidence(label: &str) -> EvidenceReference {
    EvidenceReference {
        artifact_id: format!("artifact.{label}"),
        content_hash: hash(label),
        availability: EvidenceAvailability::Available,
    }
}

fn occurrence(id: &str, at: u64) -> ErrorOccurrence {
    ErrorOccurrence {
        occurrence_id: id.to_owned(),
        fingerprint: "failure.fingerprint.1".to_owned(),
        observed_at_epoch_ms: at,
        summary: "normalized capability failure".to_owned(),
        semantic_event_id: Some(format!("event.{id}")),
        attempt_id: Some(format!("attempt.{id}")),
        diagnostics: vec![DiagnosticEvidenceReference {
            diagnostic_record_id: format!("writer.1:{at}"),
            availability: EvidenceAvailability::Available,
        }],
        evidence: vec![evidence(&format!("occurrence.{id}"))],
    }
}

fn repair_candidate(version: u64) -> RepairCandidate {
    let build = evidence(&format!("candidate.build.{version}"));
    RepairCandidate {
        candidate_id: "candidate.1".to_owned(),
        candidate_version: version,
        fingerprint: "failure.fingerprint.1".to_owned(),
        candidate_hash: build.content_hash.clone(),
        candidate_build: build,
        evidence: CandidateEvidence {
            diff: evidence(&format!("candidate.diff.{version}")),
            tests: evidence(&format!("candidate.tests.{version}")),
            benchmarks: evidence(&format!("candidate.benchmarks.{version}")),
            consequences: evidence(&format!("candidate.consequences.{version}")),
            removal_plan: evidence(&format!("candidate.removal.{version}")),
            authority_broadening: evidence(&format!("candidate.authority.{version}")),
            uncertainties: evidence(&format!("candidate.uncertainties.{version}")),
        },
        rollback_point: RollbackPoint {
            rollback_point_id: format!("rollback.point.{version}"),
            previous_working_build: evidence("build.previous"),
        },
        prepared_at_epoch_ms: 1_020 + version,
    }
}

fn awaiting(ledger: &RepairEvidenceLedger) -> (ErrorGroup, RepairCandidate, CandidateDisclosure) {
    let created = ledger
        .record_occurrence(
            &RecordOccurrenceRequest {
                operation_id: "op.occurrence.1".to_owned(),
                expected_ledger_version: None,
                occurrence: occurrence("occurrence.1", 1_000),
            },
            &RedactionSet::default(),
        )
        .expect("occurrence")
        .group;
    let investigating = ledger
        .begin_investigation(
            "op.investigate.1",
            &created.fingerprint,
            created.ledger_version,
            1_010,
            &RedactionSet::default(),
        )
        .expect("investigate");
    let candidate = repair_candidate(1);
    let prepared = ledger
        .prepare_candidate(
            &PrepareCandidateRequest {
                operation_id: "op.candidate.1".to_owned(),
                expected_ledger_version: investigating.ledger_version,
                expected_candidate_version: None,
                candidate: candidate.clone(),
            },
            &RedactionSet::default(),
        )
        .expect("candidate");
    let disclosure = CandidateDisclosure {
        disclosure_id: "disclosure.1".to_owned(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_version: candidate.candidate_version,
        management_checkpoint_id: "management.checkpoint.1".to_owned(),
        disclosure_hash: hash("disclosure.1"),
        disclosed_at_epoch_ms: 1_030,
    };
    let group = ledger
        .disclose_candidate(
            &LedgerAppendRequest {
                operation_id: "op.disclose.1".to_owned(),
                expected_ledger_version: prepared.ledger_version,
                record: disclosure.clone(),
            },
            &RedactionSet::default(),
        )
        .expect("disclosure");
    (group, candidate, disclosure)
}

fn activate(
    ledger: &RepairEvidenceLedger,
    group: &ErrorGroup,
    candidate: &RepairCandidate,
    disclosure: &CandidateDisclosure,
) -> (ErrorGroup, RestartBaton) {
    let baton = RestartBaton {
        baton_id: "baton.1".to_owned(),
        fingerprint: group.fingerprint.clone(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_version: candidate.candidate_version,
        candidate_hash: candidate.candidate_hash.clone(),
        rollback_point_id: candidate.rollback_point.rollback_point_id.clone(),
        previous_working_build_hash: candidate
            .rollback_point
            .previous_working_build
            .content_hash
            .clone(),
        management_checkpoint_id: disclosure.management_checkpoint_id.clone(),
        activation_decision_hash: hash("activation.decision.1"),
        activated_at_epoch_ms: 1_040,
    };
    ledger
        .activate_and_restart(
            &ActivateCandidateRequest {
                operation_id: "op.activate.1".to_owned(),
                expected_ledger_version: group.ledger_version,
                expected_candidate_version: candidate.candidate_version,
                baton: baton.clone(),
            },
            &RedactionSet::default(),
        )
        .expect("activate");
    let activated = ledger
        .group(&group.fingerprint)
        .expect("group")
        .expect("group exists");
    (activated, baton)
}

#[test]
fn candidate_cas_activation_verification_and_regression_are_durable() {
    let root = root("lifecycle");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let (awaiting, candidate, disclosure) = awaiting(&ledger);
    assert_eq!(awaiting.status, ErrorGroupStatus::AwaitingActivation);
    assert!(
        ledger
            .activation_eligibility(
                &awaiting.fingerprint,
                &candidate.candidate_id,
                candidate.candidate_version
            )
            .expect("eligibility")
            .eligible
    );

    let stale = ledger.prepare_candidate(
        &PrepareCandidateRequest {
            operation_id: "op.candidate.stale".to_owned(),
            expected_ledger_version: awaiting.ledger_version - 1,
            expected_candidate_version: Some(1),
            candidate: repair_candidate(2),
        },
        &RedactionSet::default(),
    );
    assert!(matches!(
        stale,
        Err(RepairLedgerError::VersionConflict { .. })
    ));

    let (activated, baton) = activate(&ledger, &awaiting, &candidate, &disclosure);
    assert_eq!(activated.status, ErrorGroupStatus::ActivatedRestarting);
    assert_eq!(
        ledger
            .restart_baton(&baton.baton_id)
            .expect("baton")
            .expect("baton exists"),
        baton
    );
    let verifying = ledger
        .begin_verification(
            &LedgerAppendRequest {
                operation_id: "op.verify.start".to_owned(),
                expected_ledger_version: activated.ledger_version,
                record: VerificationStart {
                    verification_id: "verification.1".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    candidate_version: candidate.candidate_version,
                    started_build_hash: candidate.candidate_hash.clone(),
                    started_at_epoch_ms: 1_050,
                },
            },
            &RedactionSet::default(),
        )
        .expect("verification start");
    let verified = ledger
        .complete_verification(
            &LedgerAppendRequest {
                operation_id: "op.verify.complete".to_owned(),
                expected_ledger_version: verifying.ledger_version,
                record: VerificationRecord {
                    verification_id: "verification.1".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    candidate_version: candidate.candidate_version,
                    started_build_hash: candidate.candidate_hash.clone(),
                    identity_matched: true,
                    outcome: VerificationOutcome::Passed,
                    evidence: evidence("verification.1"),
                    completed_at_epoch_ms: 1_060,
                },
            },
            &RedactionSet::default(),
        )
        .expect("verification complete");
    assert_eq!(verified.status, ErrorGroupStatus::Verified);

    let regression = ledger
        .record_occurrence(
            &RecordOccurrenceRequest {
                operation_id: "op.occurrence.regression".to_owned(),
                expected_ledger_version: Some(verified.ledger_version),
                occurrence: occurrence("occurrence.2", 1_100),
            },
            &RedactionSet::default(),
        )
        .expect("regression");
    assert_eq!(
        regression.group.status,
        ErrorGroupStatus::RegressionReopened
    );
    assert_eq!(
        regression
            .regression
            .expect("regression record")
            .prior_status,
        ErrorGroupStatus::Verified
    );
    assert_eq!(ledger.transitions(0, 64).expect("transitions").len(), 8);
    assert!(ledger.verify_integrity().expect("integrity").is_ok());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unavailable_evidence_and_baton_mismatch_block_activation() {
    let root = root("eligibility");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let (awaiting, candidate, disclosure) = awaiting(&ledger);
    ledger
        .append_evidence_tombstone(
            "op.tombstone.1",
            &EvidenceTombstone {
                tombstone_id: "tombstone.1".to_owned(),
                artifact_id: candidate.evidence.tests.artifact_id.clone(),
                content_hash: candidate.evidence.tests.content_hash.clone(),
                availability: EvidenceAvailability::Expired,
                reason: "bounded test evidence expired".to_owned(),
                recorded_at_epoch_ms: 1_035,
            },
            &RedactionSet::default(),
        )
        .expect("tombstone");
    let eligibility = ledger
        .activation_eligibility(
            &awaiting.fingerprint,
            &candidate.candidate_id,
            candidate.candidate_version,
        )
        .expect("eligibility");
    assert!(!eligibility.eligible);
    assert!(
        eligibility
            .reasons
            .iter()
            .any(|reason| reason.contains(&candidate.evidence.tests.artifact_id))
    );
    let bad_baton = RestartBaton {
        baton_id: "baton.bad".to_owned(),
        fingerprint: awaiting.fingerprint.clone(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_version: candidate.candidate_version,
        candidate_hash: hash("wrong.build"),
        rollback_point_id: candidate.rollback_point.rollback_point_id.clone(),
        previous_working_build_hash: candidate
            .rollback_point
            .previous_working_build
            .content_hash
            .clone(),
        management_checkpoint_id: disclosure.management_checkpoint_id,
        activation_decision_hash: hash("decision.bad"),
        activated_at_epoch_ms: 1_040,
    };
    assert!(matches!(
        ledger.activate_and_restart(
            &ActivateCandidateRequest {
                operation_id: "op.activate.bad".to_owned(),
                expected_ledger_version: awaiting.ledger_version,
                expected_candidate_version: candidate.candidate_version,
                baton: bad_baton,
            },
            &RedactionSet::default()
        ),
        Err(RepairLedgerError::Ineligible(_))
    ));
    assert_eq!(
        ledger
            .group(&awaiting.fingerprint)
            .expect("group")
            .expect("group")
            .status,
        ErrorGroupStatus::AwaitingActivation
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rollback_terminal_state_and_integrity_tampering_are_explicit() {
    let root = root("rollback");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let (awaiting, candidate, disclosure) = awaiting(&ledger);
    let (activated, _) = activate(&ledger, &awaiting, &candidate, &disclosure);
    let rolled_back = ledger
        .record_rollback(
            &LedgerAppendRequest {
                operation_id: "op.rollback.1".to_owned(),
                expected_ledger_version: activated.ledger_version,
                record: RollbackRecord {
                    rollback_id: "rollback.1".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    candidate_version: candidate.candidate_version,
                    restored_build_hash: candidate
                        .rollback_point
                        .previous_working_build
                        .content_hash
                        .clone(),
                    reason: "startup watchdog rejected the candidate".to_owned(),
                    evidence: evidence("rollback.1"),
                    manual_recovery_required: false,
                    completed_at_epoch_ms: 1_050,
                },
            },
            &RedactionSet::default(),
        )
        .expect("rollback");
    assert_eq!(rolled_back.status, ErrorGroupStatus::RolledBack);
    assert!(ledger.verify_integrity().expect("integrity").is_ok());

    let connection = rusqlite::Connection::open(ledger.path()).expect("raw connection");
    connection
        .execute(
            "UPDATE repair_candidates SET record_json='{}' WHERE candidate_id='candidate.1'",
            [],
        )
        .expect("tamper");
    drop(connection);
    let report = ledger.verify_integrity().expect("report");
    assert!(!report.is_ok());
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.starts_with("repair_candidates:"))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn secret_rejection_happens_before_any_repair_mutation() {
    let root = root("redaction-fence");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let redaction =
        RedactionSet::new(7, vec!["ultra-private-927".to_owned()], Vec::new()).expect("redaction");
    let mut unsafe_occurrence = occurrence("occurrence.secret", 10);
    unsafe_occurrence.summary = "failure included ultra-private-927".to_owned();
    let rejected = ledger.record_occurrence(
        &RecordOccurrenceRequest {
            operation_id: "op.secret".to_owned(),
            expected_ledger_version: None,
            occurrence: unsafe_occurrence,
        },
        &redaction,
    );
    assert!(matches!(
        rejected,
        Err(RepairLedgerError::ForbiddenSecretMaterial)
    ));
    assert!(
        ledger
            .group("failure.fingerprint.1")
            .expect("group query")
            .is_none()
    );
    assert!(ledger.transitions(0, 8).expect("transitions").is_empty());

    let committed = ledger
        .record_occurrence(
            &RecordOccurrenceRequest {
                operation_id: "op.secret".to_owned(),
                expected_ledger_version: None,
                occurrence: occurrence("occurrence.safe", 11),
            },
            &redaction,
        )
        .expect("failed write did not reserve operation id");
    assert_eq!(committed.group.ledger_version, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transition_head_full_replay_and_operation_receipts_detect_tampering() {
    let root = root("transition-head");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let request = RecordOccurrenceRequest {
        operation_id: "op.integrity".to_owned(),
        expected_ledger_version: None,
        occurrence: occurrence("occurrence.integrity", 20),
    };
    ledger
        .record_occurrence(&request, &RedactionSet::default())
        .expect("occurrence");
    let connection = rusqlite::Connection::open(ledger.path()).expect("raw connection");
    connection
        .execute(
            "UPDATE repair_operations SET response_json='{}' WHERE operation_id='op.integrity'",
            [],
        )
        .expect("tamper operation receipt");
    assert!(matches!(
        ledger.record_occurrence(&request, &RedactionSet::default()),
        Err(RepairLedgerError::Integrity)
    ));
    let report = ledger.verify_integrity().expect("integrity report");
    assert!(
        report
            .errors
            .iter()
            .any(|error| error == "operation:op.integrity")
    );

    connection
        .execute(
            "DELETE FROM repair_transitions WHERE sequence=(SELECT MAX(sequence) FROM repair_transitions)",
            [],
        )
        .expect("delete transition tail");
    assert!(matches!(
        ledger.transitions(0, 8),
        Err(RepairLedgerError::Integrity)
    ));
    let report = ledger.verify_integrity().expect("integrity report");
    assert!(
        report
            .errors
            .iter()
            .any(|error| error == "transition_head:mismatch")
    );
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.ends_with(":terminal"))
    );
    drop(connection);
    let _ = fs::remove_dir_all(root);
}

#[test]
#[allow(clippy::too_many_lines)]
fn terminal_outcomes_require_live_untombstoned_evidence() {
    let root = root("terminal-evidence");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let (awaiting, candidate, disclosure) = awaiting(&ledger);
    let (activated, _) = activate(&ledger, &awaiting, &candidate, &disclosure);
    let verifying = ledger
        .begin_verification(
            &LedgerAppendRequest {
                operation_id: "op.terminal.start".to_owned(),
                expected_ledger_version: activated.ledger_version,
                record: VerificationStart {
                    verification_id: "verification.terminal".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    candidate_version: candidate.candidate_version,
                    started_build_hash: candidate.candidate_hash.clone(),
                    started_at_epoch_ms: 2_000,
                },
            },
            &RedactionSet::default(),
        )
        .expect("verification start");
    ledger
        .append_evidence_tombstone(
            "op.terminal.tombstone.candidate",
            &EvidenceTombstone {
                tombstone_id: "tombstone.terminal.candidate".to_owned(),
                artifact_id: candidate.evidence.tests.artifact_id.clone(),
                content_hash: candidate.evidence.tests.content_hash.clone(),
                availability: EvidenceAvailability::Expired,
                reason: "candidate evidence expired after activation".to_owned(),
                recorded_at_epoch_ms: 2_001,
            },
            &RedactionSet::default(),
        )
        .expect("tombstone");
    let passed = LedgerAppendRequest {
        operation_id: "op.terminal.complete".to_owned(),
        expected_ledger_version: verifying.ledger_version,
        record: VerificationRecord {
            verification_id: "verification.terminal".to_owned(),
            candidate_id: candidate.candidate_id.clone(),
            candidate_version: candidate.candidate_version,
            started_build_hash: candidate.candidate_hash.clone(),
            identity_matched: true,
            outcome: VerificationOutcome::Passed,
            evidence: evidence("verification.terminal"),
            completed_at_epoch_ms: 2_002,
        },
    };
    assert!(matches!(
        ledger.complete_verification(&passed, &RedactionSet::default()),
        Err(RepairLedgerError::Ineligible(_))
    ));
    assert!(
        ledger
            .verification("verification.terminal")
            .expect("verification query")
            .is_none()
    );
    assert_eq!(
        ledger
            .group(&candidate.fingerprint)
            .expect("group")
            .expect("group")
            .ledger_version,
        verifying.ledger_version
    );

    let mut failed = passed;
    failed.record.outcome = VerificationOutcome::Failed;
    let still_verifying = ledger
        .complete_verification(&failed, &RedactionSet::default())
        .expect("reused operation id proves rejected terminal write was not committed");
    assert_eq!(still_verifying.status, ErrorGroupStatus::Verifying);

    ledger
        .append_evidence_tombstone(
            "op.terminal.tombstone.rollback",
            &EvidenceTombstone {
                tombstone_id: "tombstone.terminal.rollback".to_owned(),
                artifact_id: candidate
                    .rollback_point
                    .previous_working_build
                    .artifact_id
                    .clone(),
                content_hash: candidate
                    .rollback_point
                    .previous_working_build
                    .content_hash
                    .clone(),
                availability: EvidenceAvailability::Unavailable,
                reason: "previous build bytes unavailable".to_owned(),
                recorded_at_epoch_ms: 2_003,
            },
            &RedactionSet::default(),
        )
        .expect("rollback tombstone");
    let rollback = LedgerAppendRequest {
        operation_id: "op.terminal.rollback".to_owned(),
        expected_ledger_version: still_verifying.ledger_version,
        record: RollbackRecord {
            rollback_id: "rollback.terminal".to_owned(),
            candidate_id: candidate.candidate_id.clone(),
            candidate_version: candidate.candidate_version,
            restored_build_hash: candidate
                .rollback_point
                .previous_working_build
                .content_hash
                .clone(),
            reason: "verification failed".to_owned(),
            evidence: evidence("rollback.terminal"),
            manual_recovery_required: true,
            completed_at_epoch_ms: 2_004,
        },
    };
    assert!(matches!(
        ledger.record_rollback(&rollback, &RedactionSet::default()),
        Err(RepairLedgerError::Ineligible(_))
    ));
    assert!(
        ledger
            .rollback("rollback.terminal")
            .expect("rollback query")
            .is_none()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
#[allow(clippy::too_many_lines)]
fn opaque_core_event_batches_are_atomic_idempotent_and_replayable() {
    let root = root("core-events");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let request = CoreEventAppendBatchRequest {
        operation_id: "op.core.batch.1".to_owned(),
        group_id: "repair.group.1".to_owned(),
        expected_group_sequence: 0,
        events: vec![
            CoreEventInput {
                event_fingerprint: "failure_recorded".to_owned(),
                occurred_at_epoch_ms: 3_000,
                event: serde_json::json!({"kind":"failure_recorded","value":1}),
            },
            CoreEventInput {
                event_fingerprint: "regression_recorded".to_owned(),
                occurred_at_epoch_ms: 3_001,
                event: serde_json::json!({"kind":"regression_recorded","value":2}),
            },
        ],
    };
    let committed = ledger
        .append_core_events(&request, &RedactionSet::default())
        .expect("batch append");
    assert!(!committed.duplicate);
    assert_eq!(committed.events.len(), 2);
    assert_eq!(committed.current_group_sequence, 2);
    assert_eq!(committed.current_global_version, 2);
    assert_eq!(
        committed.events[1].previous_group_event_hash.as_deref(),
        Some(committed.events[0].event_hash.as_str())
    );

    let duplicate = ledger
        .append_core_events(&request, &RedactionSet::default())
        .expect("idempotent retry");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.events, committed.events);
    let mut conflict = request.clone();
    conflict.events[1].event = serde_json::json!({"kind":"changed"});
    assert!(matches!(
        ledger.append_core_events(&conflict, &RedactionSet::default()),
        Err(RepairLedgerError::OperationConflict)
    ));

    let second_group = CoreEventAppendBatchRequest {
        operation_id: "op.core.batch.2".to_owned(),
        group_id: "repair.group.2".to_owned(),
        expected_group_sequence: 0,
        events: vec![CoreEventInput {
            event_fingerprint: "candidate_prepared".to_owned(),
            occurred_at_epoch_ms: 3_002,
            event: serde_json::json!({"kind":"candidate_prepared"}),
        }],
    };
    ledger
        .append_core_events(&second_group, &RedactionSet::default())
        .expect("second group");
    assert!(matches!(
        ledger.append_core_events(
            &CoreEventAppendBatchRequest {
                operation_id: "op.core.empty".to_owned(),
                group_id: "repair.group.1".to_owned(),
                expected_group_sequence: 2,
                events: Vec::new(),
            },
            &RedactionSet::default(),
        ),
        Err(RepairLedgerError::InvalidRecord)
    ));

    let secret_redaction =
        RedactionSet::new(9, vec!["core-secret-448".to_owned()], Vec::new()).expect("redaction");
    let secret = CoreEventAppendBatchRequest {
        operation_id: "op.core.secret".to_owned(),
        group_id: "repair.group.1".to_owned(),
        expected_group_sequence: 2,
        events: vec![CoreEventInput {
            event_fingerprint: "diagnosis_recorded".to_owned(),
            occurred_at_epoch_ms: 3_003,
            event: serde_json::json!({"summary":"core-secret-448"}),
        }],
    };
    assert!(matches!(
        ledger.append_core_events(&secret, &secret_redaction),
        Err(RepairLedgerError::ForbiddenSecretMaterial)
    ));
    assert_eq!(
        ledger
            .core_event_versions("repair.group.1")
            .expect("versions")
            .current_group_sequence,
        2
    );
    drop(ledger);

    let reopened = RepairEvidenceLedger::for_store_root(&root).expect("reopen");
    assert_eq!(
        reopened.core_event_group_ids(None, 8).expect("group ids"),
        vec!["repair.group.1".to_owned(), "repair.group.2".to_owned()]
    );
    assert_eq!(
        reopened
            .load_core_events("repair.group.1", 0, 1)
            .expect("first page")[0]
            .group_sequence,
        1
    );
    assert_eq!(
        reopened
            .load_core_events("repair.group.1", 1, 8)
            .expect("second page")[0]
            .group_sequence,
        2
    );
    assert_eq!(
        reopened
            .load_all_core_events_after(0, 8)
            .expect("global replay")
            .len(),
        3
    );
    assert!(reopened.verify_integrity().expect("integrity").is_ok());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn duplicate_core_event_receipt_must_match_the_immutable_rows() {
    let root = root("core-receipt-divergence");
    let ledger = RepairEvidenceLedger::for_store_root(&root).expect("ledger");
    let request = CoreEventAppendBatchRequest {
        operation_id: "op.core.divergence".to_owned(),
        group_id: "repair.group.divergence".to_owned(),
        expected_group_sequence: 0,
        events: vec![CoreEventInput {
            event_fingerprint: "failure_recorded".to_owned(),
            occurred_at_epoch_ms: 4_000,
            event: serde_json::json!({"kind":"failure_recorded"}),
        }],
    };
    let receipt = ledger
        .append_core_events(&request, &RedactionSet::default())
        .expect("append");
    let mut divergent = receipt.events[0].clone();
    divergent.event_fingerprint = "failure_changed".to_owned();
    divergent.event_hash = super::common::canonical_hash(&(
        divergent.global_sequence,
        &divergent.group_id,
        divergent.group_sequence,
        &divergent.operation_id,
        &divergent.event_fingerprint,
        divergent.occurred_at_epoch_ms,
        &divergent.canonical_event_json,
        &divergent.event_content_hash,
        &divergent.previous_group_event_hash,
        &divergent.previous_global_event_hash,
    ))
    .expect("hash");
    let connection = rusqlite::Connection::open(ledger.path()).expect("raw connection");
    connection
        .execute(
            "UPDATE core_events SET event_fingerprint=?1, event_hash=?2 WHERE global_sequence=1",
            rusqlite::params![divergent.event_fingerprint, divergent.event_hash],
        )
        .expect("tamper event");
    connection
        .execute(
            "UPDATE core_event_groups SET head_event_hash=?1 WHERE group_id=?2",
            rusqlite::params![divergent.event_hash, divergent.group_id],
        )
        .expect("tamper group head");
    connection
        .execute(
            "UPDATE core_event_meta SET head_event_hash=?1 WHERE singleton=1",
            [&divergent.event_hash],
        )
        .expect("tamper global head");
    assert!(matches!(
        ledger.append_core_events(&request, &RedactionSet::default()),
        Err(RepairLedgerError::Integrity)
    ));
    let report = ledger.verify_integrity().expect("integrity report");
    assert!(
        report
            .errors
            .iter()
            .any(|error| error == "core_event_operation:op.core.divergence")
    );
    drop(connection);
    let _ = fs::remove_dir_all(root);
}
