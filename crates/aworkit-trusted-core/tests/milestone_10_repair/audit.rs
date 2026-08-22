use std::sync::atomic::Ordering;

use aworkit_trusted_core::*;

use crate::support::{
    AdmissionMode, NOW, activate_command, artifact, authenticated_result, authority_manifest,
    group_id, harness, hash, id, investigation_execution_receipt, occurrence, prepare_candidate,
    seed_candidate, seed_supported, supported_report, verification_evidence,
};

#[test]
fn capability_generation_never_regresses_or_changes_in_place() {
    for same_generation in [false, true] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
        let mut newest = supported_report(&seeded.candidate);
        newest.report_id = id("capability.report.generation-eight");
        newest.capability_generation = 8;
        newest.capability_digest = capability_report_digest_v1(&newest).expect("newest digest");
        *harness.bootstrap.report.lock().expect("report") = Some(newest.clone());
        harness
            .service
            .query_activation_capability(capability_command(
                &seeded,
                "generation-eight",
                seeded.aggregate.ledger_version,
            ))
            .expect("newest report");
        let aggregate = harness
            .service
            .load_aggregate(&seeded.group_id)
            .expect("aggregate");
        let mut stale = supported_report(&seeded.candidate);
        stale.report_id = id(if same_generation {
            "capability.report.changed-generation-eight"
        } else {
            "capability.report.regressed-generation-seven"
        });
        stale.capability_generation = if same_generation { 8 } else { 7 };
        stale.capability_digest = capability_report_digest_v1(&stale).expect("stale digest");
        *harness.bootstrap.report.lock().expect("report") = Some(stale.clone());

        assert!(matches!(
            harness
                .service
                .query_activation_capability(capability_command(
                    &seeded,
                    if same_generation {
                        "changed-generation-eight"
                    } else {
                        "regressed-generation-seven"
                    },
                    aggregate.ledger_version,
                )),
            Err(RepairError::CorruptLedger(
                RepairAggregateError::IllegalTransition(_)
            ))
        ));
        assert_eq!(
            harness
                .service
                .load_aggregate(&seeded.group_id)
                .expect("unchanged aggregate")
                .latest_capability_report,
            Some(newest)
        );

        let mut events = harness
            .ledger
            .load_group(&seeded.group_id)
            .expect("ledger events");
        events.push(CommittedRepairEventV1 {
            group_id: seeded.group_id.clone(),
            ledger_sequence: events.len() as u64 + 1,
            operation_id: id("operation.capability.corrupt-generation"),
            event: RepairEventV1::CapabilityReported {
                queried_at_epoch_ms: NOW,
                report: stale,
            },
        });
        assert!(matches!(
            RepairAggregateV1::rehydrate(seeded.group_id.clone(), &events),
            Err(RepairAggregateError::IllegalTransition(_))
        ));
    }
}

#[test]
fn replay_rejects_unsafe_quiescence_even_with_a_valid_resealed_hash() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("activation handoff");
    let mut events = harness
        .ledger
        .load_group(&seeded.group_id)
        .expect("ledger events");
    let mut found = false;
    for committed in &mut events {
        if let RepairEventV1::CoreQuiesced { facts } = &mut committed.event {
            facts.timed_out = true;
            facts.facts_hash =
                core_quiescence_facts_hash_v1(facts).expect("resealed quiescence facts");
            found = true;
        }
    }
    assert!(found);
    assert!(matches!(
        RepairAggregateV1::rehydrate(seeded.group_id, &events),
        Err(RepairAggregateError::InvalidEvent(_))
    ));
}

#[test]
fn replay_requires_focused_results_to_exactly_cover_the_sealed_plan() {
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
    harness
        .service
        .complete_focused_verification_evidence(CompleteFocusedVerificationEvidenceV1 {
            operation_id: id("operation.verification.exact-coverage"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id,
            current_process_generation: baton.candidate_process_generation,
            evidence: verification_evidence(&baton.verification_plan),
        })
        .expect("valid focused evidence");
    let mut events = harness
        .ledger
        .load_group(&seeded.group_id)
        .expect("ledger events");
    let mut found = false;
    for committed in &mut events {
        if let RepairEventV1::FocusedVerificationSubmitted { evidence, .. } = &mut committed.event {
            evidence.results.push(FocusedVerificationCheckResultV1 {
                check_id: id("verification.check.unsealed-extra"),
                passed: true,
                summary: "an extra unsealed check cannot expand the plan".into(),
                evidence: vec![artifact("extra-verification", '4')],
            });
            evidence.evidence_hash = focused_verification_evidence_hash_v1(evidence)
                .expect("resealed verification evidence");
            found = true;
        }
    }
    assert!(found);
    assert!(matches!(
        RepairAggregateV1::rehydrate(seeded.group_id, &events),
        Err(RepairAggregateError::InvalidEvent(_))
    ));
}

#[test]
fn active_handoff_candidate_and_capability_bindings_cannot_be_displaced() {
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
    let investigation = aggregate.investigation.clone().expect("investigation");
    let mut replacement = seeded.candidate.clone();
    replacement.candidate_version = 2;
    replacement.summary = "a later candidate must wait for a new lifecycle".into();
    replacement.candidate_hash = repair_candidate_hash_v1(&replacement).expect("candidate hash");
    let receipt = investigation_execution_receipt(&investigation, &replacement);
    *harness.investigations.receipt.lock().expect("receipt") = Some(receipt.clone());
    let artifact_calls = harness.artifacts.calls.load(Ordering::SeqCst);

    assert!(matches!(
        harness
            .service
            .register_candidate(RegisterRepairCandidateV1 {
                operation_id: id("operation.candidate.active-replacement"),
                expected_ledger_version: aggregate.ledger_version,
                investigation_id: investigation.investigation_id,
                execution_receipt_id: receipt.receipt.receipt_id.clone(),
                expected_execution_receipt_hash: receipt.receipt.receipt_hash.clone(),
                candidate: replacement.clone(),
            }),
        Err(RepairError::OperationAlreadyActive)
    ));
    assert_eq!(
        harness.artifacts.calls.load(Ordering::SeqCst),
        artifact_calls
    );
    let query_calls = harness.bootstrap.query_calls.load(Ordering::SeqCst);
    let report = aggregate
        .latest_capability_report
        .clone()
        .expect("capability report");
    assert!(matches!(
        harness
            .service
            .query_activation_capability(QueryActivationCapabilityV1 {
                operation_id: id("operation.capability.active-replacement"),
                expected_ledger_version: aggregate.ledger_version,
                group_id: seeded.group_id.clone(),
                candidate_id: seeded.candidate.candidate_id.clone(),
                expected_candidate_version: seeded.candidate.candidate_version,
                expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
                now_epoch_ms: NOW,
            }),
        Err(RepairError::OperationAlreadyActive)
    ));
    assert_eq!(
        harness.bootstrap.query_calls.load(Ordering::SeqCst),
        query_calls
    );

    let mut candidate_events = harness
        .ledger
        .load_group(&seeded.group_id)
        .expect("candidate replay events");
    candidate_events.push(CommittedRepairEventV1 {
        group_id: seeded.group_id.clone(),
        ledger_sequence: candidate_events.len() as u64 + 1,
        operation_id: id("operation.candidate.corrupt-replay"),
        event: RepairEventV1::CandidateRegistered {
            candidate: replacement,
            execution_receipt: receipt,
        },
    });
    assert!(matches!(
        RepairAggregateV1::rehydrate(seeded.group_id.clone(), &candidate_events),
        Err(RepairAggregateError::IllegalTransition(_))
    ));

    let mut report_events = harness
        .ledger
        .load_group(&seeded.group_id)
        .expect("report replay events");
    report_events.push(CommittedRepairEventV1 {
        group_id: seeded.group_id.clone(),
        ledger_sequence: report_events.len() as u64 + 1,
        operation_id: id("operation.capability.corrupt-replay"),
        event: RepairEventV1::CapabilityReported {
            queried_at_epoch_ms: NOW,
            report,
        },
    });
    assert!(matches!(
        RepairAggregateV1::rehydrate(seeded.group_id, &report_events),
        Err(RepairAggregateError::IllegalTransition(_))
    ));
}

#[test]
fn nonunsupported_result_requires_a_committed_safe_quiescence_handoff() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    *harness.quiescence.unsafe_mode.lock().expect("unsafe mode") = Some((true, false));
    assert!(matches!(
        harness
            .service
            .activate_and_restart(activate_command(&seeded)),
        Err(RepairError::UnsafeQuiescence)
    ));
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let baton = aggregate.activation_baton.clone().expect("baton");
    let result = authenticated_result(
        &baton,
        BootstrapResultKindV1::RolledBack {
            reason: "helper must not claim rollback before safe handoff".into(),
            rollback_evidence: vec![artifact("pre-handoff-rollback", '5')],
        },
        baton.rollback_process_generation,
    );
    *harness.bootstrap.result.lock().expect("result") = Some(result.clone());

    assert!(matches!(
        harness
            .service
            .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
                operation_id: id("operation.result.before-safe-handoff"),
                expected_ledger_version: aggregate.ledger_version,
                group_id: seeded.group_id.clone(),
                activation_id: baton.activation_id,
                current_process_generation: baton.rollback_process_generation,
                now_epoch_ms: NOW,
            }),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 0);
    let mut events = harness
        .ledger
        .load_group(&seeded.group_id)
        .expect("ledger events");
    events.push(CommittedRepairEventV1 {
        group_id: seeded.group_id.clone(),
        ledger_sequence: events.len() as u64 + 1,
        operation_id: id("operation.result.corrupt-pre-handoff"),
        event: RepairEventV1::BootstrapResultReconciled {
            reconciled_at_epoch_ms: NOW,
            result,
        },
    });
    assert!(matches!(
        RepairAggregateV1::rehydrate(seeded.group_id, &events),
        Err(RepairAggregateError::IllegalTransition(_))
    ));
}

#[test]
fn terminal_result_fences_new_focused_verification_but_not_its_exact_retry() {
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
    let result = authenticated_result(
        &baton,
        BootstrapResultKindV1::RolledBack {
            reason: "focused verification failed".into(),
            rollback_evidence: vec![artifact("terminal-rollback", '6')],
        },
        baton.rollback_process_generation,
    );
    *harness.bootstrap.result.lock().expect("result") = Some(result);
    harness
        .service
        .reconcile_bootstrap_result(ReconcileBootstrapResultV1 {
            operation_id: id("operation.result.terminal-rollback"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id.clone(),
            current_process_generation: baton.rollback_process_generation,
            now_epoch_ms: NOW,
        })
        .expect("rollback result");
    let terminal = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("terminal aggregate");
    let evidence = verification_evidence(&baton.verification_plan);
    let submitted = harness
        .bootstrap
        .submitted_verification
        .lock()
        .expect("submitted verification")
        .len();
    assert!(matches!(
        harness.service.complete_focused_verification_evidence(
            CompleteFocusedVerificationEvidenceV1 {
                operation_id: id("operation.verification.after-terminal"),
                expected_ledger_version: terminal.ledger_version,
                group_id: seeded.group_id.clone(),
                activation_id: baton.activation_id.clone(),
                current_process_generation: baton.candidate_process_generation,
                evidence: evidence.clone(),
            }
        ),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(
        harness
            .bootstrap
            .submitted_verification
            .lock()
            .expect("submitted verification")
            .len(),
        submitted
    );
    let mut events = harness
        .ledger
        .load_group(&seeded.group_id)
        .expect("ledger events");
    events.push(CommittedRepairEventV1 {
        group_id: seeded.group_id.clone(),
        ledger_sequence: events.len() as u64 + 1,
        operation_id: id("operation.verification.corrupt-terminal"),
        event: RepairEventV1::FocusedVerificationSubmitted {
            activation_id: baton.activation_id,
            process_generation: baton.candidate_process_generation,
            evidence,
        },
    });
    assert!(matches!(
        RepairAggregateV1::rehydrate(seeded.group_id, &events),
        Err(RepairAggregateError::IllegalTransition(_))
    ));
}

#[test]
fn investigation_token_ceiling_accepts_the_boundary_and_rejects_one_over() {
    for (tokens, accepted) in [
        (MAX_REPAIR_INVESTIGATION_TOKENS_V1, true),
        (MAX_REPAIR_INVESTIGATION_TOKENS_V1 + 1, false),
    ] {
        let harness = harness(AdmissionMode::Accepted);
        let group_id = group_id();
        harness
            .service
            .record_recurring_failure(RecordRecurringFailureV1 {
                operation_id: id("operation.failure.token-boundary"),
                group_id: group_id.clone(),
                expected_ledger_version: 0,
                occurrence: occurrence("token-boundary"),
            })
            .expect("failure");
        let result = harness.service.start_bounded_investigation(
            StartInvestigationV1 {
                operation_id: id("operation.investigation.token-boundary"),
                expected_ledger_version: 1,
                investigation_id: id("investigation.token-boundary"),
                explicit_user_decision_id: id("decision.token-boundary"),
                group_id: group_id.clone(),
                management_chat_id: id("chat.management"),
                management_run_id: id("run.management"),
                requested_capability_ids: vec![id("capability.build")],
                budget: RepairInvestigationBudgetV1 {
                    max_attempts: 1,
                    max_tool_calls: 1,
                    max_tokens: tokens,
                    deadline_ms: 1_000,
                },
            },
            &authority_manifest(),
        );
        assert_eq!(result.is_ok(), accepted);
        assert_eq!(
            harness.investigations.effects.load(Ordering::SeqCst),
            usize::from(accepted)
        );
        assert_eq!(
            harness
                .service
                .load_aggregate(&group_id)
                .expect("aggregate")
                .ledger_version,
            if accepted { 2 } else { 1 }
        );
    }
}

#[test]
fn fingerprint_has_one_stable_group_and_cannot_create_duplicate_streams() {
    let harness = harness(AdmissionMode::Accepted);
    let occurrence = occurrence("stable-group");
    let resolved = repair_group_id_for_fingerprint_v1(&occurrence.fingerprint)
        .expect("stable fingerprint group");
    assert_eq!(resolved, group_id());
    let wrong_group = id("repair.group.caller-selected");

    assert!(matches!(
        harness
            .service
            .record_recurring_failure(RecordRecurringFailureV1 {
                operation_id: id("operation.failure.wrong-group"),
                group_id: wrong_group.clone(),
                expected_ledger_version: 0,
                occurrence: occurrence.clone(),
            }),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(
        harness
            .service
            .load_aggregate(&wrong_group)
            .expect("wrong group")
            .ledger_version,
        0
    );
    harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.stable-group"),
            group_id: resolved.clone(),
            expected_ledger_version: 0,
            occurrence,
        })
        .expect("resolved group");
    assert_eq!(
        harness
            .service
            .load_aggregate(&resolved)
            .expect("resolved aggregate")
            .ledger_version,
        1
    );
}

#[test]
fn candidate_requires_authenticated_exact_investigation_execution_set_and_budget() {
    for failure in [
        "unauthenticated",
        "frozen_set",
        "executed_set",
        "frozen_budget",
        "observed_usage",
    ] {
        let harness = harness(AdmissionMode::Accepted);
        let prepared = prepare_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
        let mut receipt =
            investigation_execution_receipt(&prepared.investigation, &prepared.candidate);
        match failure {
            "unauthenticated" => receipt.peer.same_user_authenticated = false,
            "frozen_set" => {
                receipt.receipt.frozen_capability_ids = vec![id("capability.test")];
                reseal_receipt(&mut receipt);
            }
            "executed_set" => {
                receipt.receipt.executed_capability_ids = vec![id("capability.test")];
                reseal_receipt(&mut receipt);
            }
            "frozen_budget" => {
                receipt.receipt.frozen_budget.max_tokens -= 1;
                reseal_receipt(&mut receipt);
            }
            "observed_usage" => {
                receipt.receipt.observed_usage.tokens =
                    receipt.receipt.frozen_budget.max_tokens + 1;
                reseal_receipt(&mut receipt);
            }
            _ => unreachable!(),
        }
        *harness.investigations.receipt.lock().expect("receipt") = Some(receipt.clone());
        let artifact_calls = harness.artifacts.calls.load(Ordering::SeqCst);

        assert!(matches!(
            harness
                .service
                .register_candidate(RegisterRepairCandidateV1 {
                    operation_id: id(&format!("operation.candidate.receipt.{failure}")),
                    expected_ledger_version: 2,
                    investigation_id: prepared.investigation.investigation_id,
                    execution_receipt_id: receipt.receipt.receipt_id,
                    expected_execution_receipt_hash: receipt.receipt.receipt_hash,
                    candidate: prepared.candidate,
                }),
            Err(RepairError::InvalidContract(_))
        ));
        assert_eq!(
            harness.artifacts.calls.load(Ordering::SeqCst),
            artifact_calls
        );
        assert_eq!(
            harness
                .service
                .load_aggregate(&prepared.group_id)
                .expect("aggregate")
                .ledger_version,
            2
        );
    }
}

fn reseal_receipt(value: &mut AuthenticatedInvestigationExecutionReceiptV1) {
    value.receipt.receipt_hash =
        investigation_execution_receipt_hash_v1(&value.receipt).expect("receipt hash");
    value.peer.ownership_hash = hash('0');
    value.peer.channel_binding_hash = hash('1');
}

fn capability_command(
    seeded: &crate::support::SeededRepair,
    suffix: &str,
    expected_ledger_version: u64,
) -> QueryActivationCapabilityV1 {
    QueryActivationCapabilityV1 {
        operation_id: id(&format!("operation.capability.{suffix}")),
        expected_ledger_version,
        group_id: seeded.group_id.clone(),
        candidate_id: seeded.candidate.candidate_id.clone(),
        expected_candidate_version: seeded.candidate.candidate_version,
        expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
        now_epoch_ms: NOW,
    }
}
