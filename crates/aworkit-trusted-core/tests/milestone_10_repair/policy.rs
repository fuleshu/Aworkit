use std::sync::atomic::Ordering;

use aworkit_trusted_core::*;

use crate::support::{
    AdmissionMode, NOW, activate_command, authority_manifest, group_id, harness, id, occurrence,
    seed_candidate, seed_supported,
};

#[test]
fn recording_a_failure_is_passive_until_the_user_starts_an_investigation() {
    let harness = harness(AdmissionMode::Accepted);
    let group_id = group_id();
    let aggregate = harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.passive"),
            group_id,
            expected_ledger_version: 0,
            occurrence: occurrence("passive"),
        })
        .expect("record failure");

    assert_eq!(aggregate.phase, Some(RepairPhaseV1::Observed));
    assert_eq!(aggregate.ledger_version, 1);
    assert_eq!(harness.investigations.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.bootstrap.query_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn investigation_authority_is_an_exact_subset_of_the_frozen_manifest() {
    let harness = harness(AdmissionMode::Accepted);
    let group_id = group_id();
    harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.authority"),
            group_id: group_id.clone(),
            expected_ledger_version: 0,
            occurrence: occurrence("authority"),
        })
        .expect("record failure");
    let authority = authority_manifest();
    let invalid = harness.service.start_bounded_investigation(
        StartInvestigationV1 {
            operation_id: id("operation.investigate.invalid"),
            expected_ledger_version: 1,
            investigation_id: id("investigation.invalid"),
            explicit_user_decision_id: id("decision.investigate.invalid"),
            group_id: group_id.clone(),
            management_chat_id: id("chat.management"),
            management_run_id: id("run.management"),
            requested_capability_ids: vec![id("capability.not-frozen")],
            budget: RepairInvestigationBudgetV1 {
                max_attempts: 2,
                max_tool_calls: 4,
                max_tokens: 5_000,
                deadline_ms: 30_000,
            },
        },
        &authority,
    );
    assert!(matches!(invalid, Err(RepairError::InvalidContract(_))));
    assert_eq!(harness.investigations.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        harness
            .service
            .load_aggregate(&group_id)
            .expect("aggregate")
            .ledger_version,
        1
    );

    let investigation = harness
        .service
        .start_bounded_investigation(
            StartInvestigationV1 {
                operation_id: id("operation.investigate.valid"),
                expected_ledger_version: 1,
                investigation_id: id("investigation.valid"),
                explicit_user_decision_id: id("decision.investigate.valid"),
                group_id,
                management_chat_id: id("chat.management"),
                management_run_id: id("run.management"),
                requested_capability_ids: vec![id("capability.build")],
                budget: RepairInvestigationBudgetV1 {
                    max_attempts: 2,
                    max_tool_calls: 4,
                    max_tokens: 5_000,
                    deadline_ms: 30_000,
                },
            },
            &authority,
        )
        .expect("bounded investigation");
    assert_eq!(
        investigation.authority.capability_ids,
        vec![id("capability.build")]
    );
    assert_eq!(
        investigation.authority.authority_manifest_hash,
        authority.manifest_hash
    );
    assert_eq!(harness.investigations.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn incomplete_candidate_disclosure_never_enters_the_ledger() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
    let mut candidate = seeded.candidate.clone();
    candidate.candidate_id = id("candidate.incomplete");
    candidate.disclosure.source_diff = RepairEvidenceDisclosureV1::NotPerformed {
        explanation: "source diff accidentally omitted".into(),
    };
    candidate.disclosure.disclosure_hash =
        repair_disclosure_hash_v1(&candidate.disclosure).expect("disclosure hash");
    candidate.candidate_hash = repair_candidate_hash_v1(&candidate).expect("candidate hash");
    let result = harness
        .service
        .register_candidate(RegisterRepairCandidateV1 {
            operation_id: id("operation.candidate.incomplete"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            investigation_id: id("investigation.one"),
            execution_receipt_id: seeded.execution_receipt.receipt.receipt_id,
            expected_execution_receipt_hash: seeded.execution_receipt.receipt.receipt_hash,
            candidate,
        });

    assert!(matches!(result, Err(RepairError::InvalidContract(_))));
    assert_eq!(
        harness
            .service
            .load_aggregate(&seeded.group_id)
            .expect("aggregate")
            .ledger_version,
        seeded.aggregate.ledger_version
    );
}

#[test]
fn behavior_disclosures_require_unique_ids_and_independent_details() {
    for duplicate_id in [false, true] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
        let mut candidate = seeded.candidate.clone();
        candidate.candidate_id = id(if duplicate_id {
            "candidate.duplicate-disclosure-id"
        } else {
            "candidate.missing-disclosure-detail"
        });
        if duplicate_id {
            let replacement = candidate
                .disclosure
                .replaced_behaviors
                .items
                .first()
                .expect("replacement disclosure")
                .clone();
            candidate
                .disclosure
                .replaced_behaviors
                .items
                .push(replacement);
        } else {
            candidate
                .disclosure
                .replaced_behaviors
                .items
                .first_mut()
                .expect("replacement disclosure")
                .detail
                .clear();
        }
        candidate.disclosure.disclosure_hash =
            repair_disclosure_hash_v1(&candidate.disclosure).expect("disclosure hash");
        candidate.candidate_hash = repair_candidate_hash_v1(&candidate).expect("candidate hash");

        assert!(matches!(
            harness
                .service
                .register_candidate(RegisterRepairCandidateV1 {
                    operation_id: id(if duplicate_id {
                        "operation.candidate.duplicate-disclosure-id"
                    } else {
                        "operation.candidate.missing-disclosure-detail"
                    }),
                    expected_ledger_version: seeded.aggregate.ledger_version,
                    investigation_id: id("investigation.one"),
                    execution_receipt_id: seeded.execution_receipt.receipt.receipt_id,
                    expected_execution_receipt_hash: seeded.execution_receipt.receipt.receipt_hash,
                    candidate,
                }),
            Err(RepairError::InvalidContract(_))
        ));
    }
}

#[test]
fn a_new_failure_cannot_replace_an_active_activation_handoff() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("activation handoff");
    let active = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("active aggregate");
    let after_failure = harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.during-activation"),
            group_id: seeded.group_id.clone(),
            expected_ledger_version: active.ledger_version,
            occurrence: occurrence("during-activation"),
        })
        .expect("failure evidence remains recordable");
    assert_eq!(
        after_failure.phase,
        Some(RepairPhaseV1::AwaitingBootstrapResult)
    );
    let dispatches = harness.investigations.calls.load(Ordering::SeqCst);
    assert!(matches!(
        harness.service.start_bounded_investigation(
            StartInvestigationV1 {
                operation_id: id("operation.investigate.during-activation"),
                expected_ledger_version: after_failure.ledger_version,
                investigation_id: id("investigation.during-activation"),
                explicit_user_decision_id: id("decision.investigate.during-activation"),
                group_id: seeded.group_id,
                management_chat_id: id("chat.management"),
                management_run_id: id("run.management"),
                requested_capability_ids: vec![id("capability.build")],
                budget: RepairInvestigationBudgetV1 {
                    max_attempts: 2,
                    max_tool_calls: 4,
                    max_tokens: 5_000,
                    deadline_ms: 30_000,
                },
            },
            &authority_manifest(),
        ),
        Err(RepairError::CorruptLedger(
            RepairAggregateError::IllegalTransition(_)
        ))
    ));
    assert_eq!(
        harness.investigations.calls.load(Ordering::SeqCst),
        dispatches
    );
}

#[test]
fn reject_and_defer_paths_never_create_a_baton_or_call_bootstrap() {
    for disposition in [
        RepairCandidateDispositionV1::Rejected,
        RepairCandidateDispositionV1::Deferred,
    ] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
        let aggregate = harness
            .service
            .reject_or_defer_candidate(RejectCandidateV1 {
                operation_id: id(match disposition {
                    RepairCandidateDispositionV1::Rejected => "operation.reject",
                    RepairCandidateDispositionV1::Deferred => "operation.defer",
                }),
                expected_ledger_version: seeded.aggregate.ledger_version,
                group_id: seeded.group_id,
                decision: RepairCandidateDecisionV1 {
                    decision_id: id("decision.candidate"),
                    candidate_id: seeded.candidate.candidate_id,
                    candidate_version: seeded.candidate.candidate_version,
                    disposition,
                    reason: "user kept the current build".into(),
                },
            })
            .expect("candidate decision");
        assert_eq!(aggregate.phase, Some(RepairPhaseV1::CandidateRejected));
        assert!(aggregate.activation_baton.is_none());
        assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
        assert_eq!(harness.bootstrap.enrollment_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.management.checkpoint_calls.load(Ordering::SeqCst),
            0
        );
        assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn stale_capability_or_ledger_decisions_stop_before_checkpoint_and_handoff() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    let mut stale_capability = crate::support::activate_command(&seeded);
    stale_capability.now_epoch_ms = NOW + 20_000;
    assert!(matches!(
        harness.service.activate_and_restart(stale_capability),
        Err(RepairError::InvalidContract(_))
    ));

    let mut stale_version = crate::support::activate_command(&seeded);
    stale_version.expected_ledger_version -= 1;
    assert!(matches!(
        harness.service.activate_and_restart(stale_version),
        Err(RepairError::StaleLedgerVersion { .. })
    ));
    assert_eq!(
        harness.management.checkpoint_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 0);
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert!(aggregate.activation_baton.is_none());
}
