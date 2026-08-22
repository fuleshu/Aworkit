use std::sync::atomic::Ordering;

use aworkit_protocol::ProcessGeneration;
use aworkit_trusted_core::*;

use crate::support::{
    AdmissionMode, NOW, activate_command, artifact, authenticated_result, authority_manifest,
    harness, id, occurrence, seed_supported, verification_evidence,
};

#[test]
fn historical_receipt_retry_never_resumes_after_a_new_lifecycle_supersedes_it() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("activation handoff");
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let baton = aggregate.activation_baton.clone().expect("baton");
    *harness.bootstrap.result.lock().expect("result") = Some(authenticated_result(
        &baton,
        BootstrapResultKindV1::RolledBack {
            reason: "first lifecycle rolled back".into(),
            rollback_evidence: vec![artifact("first-lifecycle-rollback", '2')],
        },
        baton.rollback_process_generation,
    ));
    let command = ReconcileBootstrapResultV1 {
        operation_id: id("operation.result.first-lifecycle"),
        expected_ledger_version: aggregate.ledger_version,
        group_id: seeded.group_id.clone(),
        activation_id: baton.activation_id,
        current_process_generation: baton.rollback_process_generation,
        now_epoch_ms: NOW,
    };
    harness
        .service
        .reconcile_bootstrap_result(command.clone())
        .expect("first receipt");
    let terminal = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("terminal aggregate");
    let after_failure = harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.second-lifecycle"),
            group_id: seeded.group_id.clone(),
            expected_ledger_version: terminal.ledger_version,
            occurrence: occurrence("second-lifecycle"),
        })
        .expect("second occurrence");
    harness
        .service
        .start_bounded_investigation(
            StartInvestigationV1 {
                operation_id: id("operation.investigation.second-lifecycle"),
                expected_ledger_version: after_failure.ledger_version,
                investigation_id: id("investigation.second-lifecycle"),
                explicit_user_decision_id: id("decision.investigation.second-lifecycle"),
                group_id: seeded.group_id,
                management_chat_id: id("chat.management"),
                management_run_id: id("run.management.second-lifecycle"),
                requested_capability_ids: vec![id("capability.build")],
                budget: RepairInvestigationBudgetV1 {
                    max_attempts: 2,
                    max_tool_calls: 4,
                    max_tokens: 5_000,
                    deadline_ms: 30_000,
                },
            },
            &authority_manifest(),
        )
        .expect("second investigation");
    let resume_calls = harness.management.resume_calls.load(Ordering::SeqCst);

    assert!(matches!(
        harness.service.reconcile_bootstrap_result(command),
        Err(RepairError::OperationConflict)
    ));
    assert_eq!(
        harness.management.resume_calls.load(Ordering::SeqCst),
        resume_calls
    );
}

#[test]
fn authenticated_verified_receipt_commits_before_same_chat_resume() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    let (baton, evidence, aggregate) = activate_and_submit_verification(&harness, &seeded);
    *harness.bootstrap.result.lock().expect("result") = Some(authenticated_result(
        &baton,
        BootstrapResultKindV1::ActivatedVerified {
            focused_verification: evidence,
        },
        baton.candidate_process_generation,
    ));
    harness.log.lock().expect("log").clear();

    let outcome = harness
        .service
        .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
            operation_id: id("operation.result.verified"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id.clone(),
            current_process_generation: baton.candidate_process_generation,
            now_epoch_ms: NOW,
        })
        .expect("receipt reconciliation");
    assert!(!outcome.duplicate);
    assert!(outcome.resume_dispatched);
    let log = harness.log.lock().expect("log");
    let result_commit = position(&log, "ledger.result");
    let resume = position(&log, "management.resume");
    assert!(result_commit < resume, "{log:?}");
    drop(log);
    let resumes = harness.management.resumes.lock().expect("resumes");
    assert_eq!(resumes.len(), 1);
    assert_eq!(resumes[0].checkpoint.chat_id, id("chat.management"));
    assert_eq!(resumes[0].checkpoint.run_id, id("run.management"));
    assert_eq!(
        resumes[0].checkpoint.checkpoint_id,
        baton.management_checkpoint.checkpoint_id
    );
    drop(resumes);
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert_eq!(aggregate.phase, Some(RepairPhaseV1::Verified));
}

#[test]
fn untrusted_or_wrong_generation_receipts_never_commit_or_resume() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    let (baton, evidence, aggregate) = activate_and_submit_verification(&harness, &seeded);
    let mut result = authenticated_result(
        &baton,
        BootstrapResultKindV1::ActivatedVerified {
            focused_verification: evidence,
        },
        baton.candidate_process_generation,
    );
    result.peer.same_user_authenticated = false;
    *harness.bootstrap.result.lock().expect("result") = Some(result);

    assert!(matches!(
        harness
            .service
            .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
                operation_id: id("operation.result.untrusted"),
                expected_ledger_version: aggregate.ledger_version,
                group_id: seeded.group_id.clone(),
                activation_id: baton.activation_id.clone(),
                current_process_generation: baton.candidate_process_generation,
                now_epoch_ms: NOW,
            }),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 0);
    let unchanged = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert_eq!(unchanged.ledger_version, aggregate.ledger_version);
    assert!(unchanged.bootstrap_result.is_none());

    assert!(matches!(
        harness
            .service
            .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
                operation_id: id("operation.result.wrong-generation"),
                expected_ledger_version: aggregate.ledger_version,
                group_id: seeded.group_id,
                activation_id: baton.activation_id,
                current_process_generation: ProcessGeneration(99),
                now_epoch_ms: NOW,
            }),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn activated_verified_receipt_cannot_claim_success_for_a_failed_focused_check() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("activation handoff");
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let baton = aggregate.activation_baton.clone().expect("baton");
    let mut evidence = verification_evidence(&baton.verification_plan);
    evidence.results[0].passed = false;
    evidence.results[0].summary = "focused smoke verification failed".into();
    evidence.evidence_hash =
        focused_verification_evidence_hash_v1(&evidence).expect("evidence hash");
    let aggregate = harness
        .service
        .complete_focused_verification_evidence(CompleteFocusedVerificationEvidenceV1 {
            operation_id: id("operation.verification.failed"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id.clone(),
            current_process_generation: baton.candidate_process_generation,
            evidence: evidence.clone(),
        })
        .expect("failed verification evidence is still committed for rollback");
    *harness.bootstrap.result.lock().expect("result") = Some(authenticated_result(
        &baton,
        BootstrapResultKindV1::ActivatedVerified {
            focused_verification: evidence,
        },
        baton.candidate_process_generation,
    ));

    assert!(matches!(
        harness
            .service
            .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
                operation_id: id("operation.result.false-verified"),
                expected_ledger_version: aggregate.ledger_version,
                group_id: seeded.group_id.clone(),
                activation_id: baton.activation_id,
                current_process_generation: baton.candidate_process_generation,
                now_epoch_ms: NOW,
            }),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 0);
    assert!(
        harness
            .service
            .load_aggregate(&seeded.group_id)
            .expect("aggregate")
            .bootstrap_result
            .is_none()
    );
}

#[test]
fn committed_receipt_can_retry_idempotent_resume_after_an_interruption() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    let (baton, evidence, aggregate) = activate_and_submit_verification(&harness, &seeded);
    *harness.bootstrap.result.lock().expect("result") = Some(authenticated_result(
        &baton,
        BootstrapResultKindV1::ActivatedVerified {
            focused_verification: evidence,
        },
        baton.candidate_process_generation,
    ));
    harness
        .management
        .fail_resume_once
        .store(true, Ordering::SeqCst);
    let command = ReconcileBootstrapResultV1 {
        operation_id: id("operation.result.retry"),
        expected_ledger_version: aggregate.ledger_version,
        group_id: seeded.group_id.clone(),
        activation_id: baton.activation_id,
        current_process_generation: baton.candidate_process_generation,
        now_epoch_ms: NOW,
    };
    assert!(matches!(
        harness.service.reconcile_bootstrap_result(command.clone()),
        Err(RepairError::Port { .. })
    ));
    assert_eq!(
        harness
            .service
            .load_aggregate(&seeded.group_id)
            .expect("committed result")
            .ledger_version,
        aggregate.ledger_version + 1
    );

    let retry = harness
        .service
        .reconcile_bootstrap_result(command)
        .expect("idempotent resume retry");
    assert!(retry.duplicate);
    assert!(retry.resume_dispatched);
    assert_eq!(harness.bootstrap.result_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 2);
    assert_eq!(harness.management.resumes.lock().expect("resumes").len(), 1);
}

#[test]
fn rollback_receipt_resumes_the_same_management_chat_in_rollback_generation() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("activation handoff");
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let baton = aggregate.activation_baton.clone().expect("baton");
    *harness.bootstrap.result.lock().expect("result") = Some(authenticated_result(
        &baton,
        BootstrapResultKindV1::RolledBack {
            reason: "focused verification failed".into(),
            rollback_evidence: vec![artifact("rollback", 'b')],
        },
        baton.rollback_process_generation,
    ));
    harness
        .service
        .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
            operation_id: id("operation.result.rollback"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id,
            current_process_generation: baton.rollback_process_generation,
            now_epoch_ms: NOW,
        })
        .expect("rollback receipt");
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert_eq!(aggregate.phase, Some(RepairPhaseV1::RolledBack));
    let admission_calls = harness.bootstrap.admission_calls.load(Ordering::SeqCst);
    let quiescence_calls = harness.bootstrap.quiescence_calls.load(Ordering::SeqCst);
    assert!(matches!(
        harness
            .service
            .activate_and_restart(activate_command(&seeded)),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(
        harness.bootstrap.admission_calls.load(Ordering::SeqCst),
        admission_calls
    );
    assert_eq!(
        harness.bootstrap.quiescence_calls.load(Ordering::SeqCst),
        quiescence_calls
    );
    let resume = harness.management.resumes.lock().expect("resumes");
    assert_eq!(resume[0].checkpoint.chat_id, id("chat.management"));
    assert_eq!(
        resume[0].recipient_process_generation,
        baton.rollback_process_generation
    );
}

#[test]
fn recurrence_after_verified_repair_records_regression_without_auto_repair() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    let (baton, evidence, aggregate) = activate_and_submit_verification(&harness, &seeded);
    *harness.bootstrap.result.lock().expect("result") = Some(authenticated_result(
        &baton,
        BootstrapResultKindV1::ActivatedVerified {
            focused_verification: evidence,
        },
        baton.candidate_process_generation,
    ));
    harness
        .service
        .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
            operation_id: id("operation.result.before-regression"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id,
            current_process_generation: baton.candidate_process_generation,
            now_epoch_ms: NOW,
        })
        .expect("verified receipt");
    let before = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("before regression");
    let investigation_calls = harness.investigations.calls.load(Ordering::SeqCst);
    let admission_calls = harness.bootstrap.admission_calls.load(Ordering::SeqCst);
    let after = harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.regression"),
            group_id: seeded.group_id,
            expected_ledger_version: before.ledger_version,
            occurrence: occurrence("regression"),
        })
        .expect("regression occurrence");
    assert_eq!(after.ledger_version, before.ledger_version + 2);
    assert_eq!(after.phase, Some(RepairPhaseV1::Regression));
    assert_eq!(after.regressions.len(), 1);
    assert_eq!(
        harness.investigations.calls.load(Ordering::SeqCst),
        investigation_calls
    );
    assert_eq!(
        harness.bootstrap.admission_calls.load(Ordering::SeqCst),
        admission_calls
    );
}

fn activate_and_submit_verification(
    harness: &crate::support::Harness,
    seeded: &crate::support::SeededRepair,
) -> (
    RepairActivationBatonV1,
    FocusedVerificationEvidenceV1,
    RepairAggregateV1,
) {
    harness
        .service
        .activate_and_restart(activate_command(seeded))
        .expect("activation handoff");
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let baton = aggregate.activation_baton.clone().expect("baton");
    let evidence = verification_evidence(&baton.verification_plan);
    let wrong_generation = harness.service.complete_focused_verification_evidence(
        CompleteFocusedVerificationEvidenceV1 {
            operation_id: id("operation.verification.wrong-generation"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id.clone(),
            current_process_generation: baton.rollback_process_generation,
            evidence: evidence.clone(),
        },
    );
    assert!(matches!(
        wrong_generation,
        Err(RepairError::InvalidContract(_))
    ));
    assert!(
        harness
            .bootstrap
            .submitted_verification
            .lock()
            .expect("verification")
            .is_empty()
    );
    let aggregate = harness
        .service
        .complete_focused_verification_evidence(CompleteFocusedVerificationEvidenceV1 {
            operation_id: id("operation.verification.complete"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id.clone(),
            current_process_generation: baton.candidate_process_generation,
            evidence: evidence.clone(),
        })
        .expect("focused verification");
    (baton, evidence, aggregate)
}

fn position(log: &[String], expected: &str) -> usize {
    log.iter()
        .position(|entry| entry == expected)
        .unwrap_or_else(|| panic!("missing {expected:?} in {log:?}"))
}
