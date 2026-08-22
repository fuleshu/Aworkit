use std::sync::atomic::Ordering;

use aworkit_trusted_core::*;

use crate::support::{
    AdmissionMode, NOW, activate_command, authority_manifest, enrollment_report, group_id, harness,
    id, occurrence, seed_candidate, seed_supported,
};

#[test]
fn committed_investigation_outbox_redrives_after_ledger_or_dispatch_uncertainty() {
    for fail_ledger_ack in [true, false] {
        let harness = harness(AdmissionMode::Accepted);
        let group_id = group_id();
        harness
            .service
            .record_recurring_failure(RecordRecurringFailureV1 {
                operation_id: id("operation.failure.retry-investigation"),
                group_id: group_id.clone(),
                expected_ledger_version: 0,
                occurrence: occurrence("retry-investigation"),
            })
            .expect("failure");
        let command = investigation_command(group_id.clone());
        if fail_ledger_ack {
            harness
                .ledger
                .fail_append_after_commit_once
                .store(true, Ordering::SeqCst);
        } else {
            harness
                .investigations
                .fail_dispatch_after_effect_once
                .store(true, Ordering::SeqCst);
        }
        assert!(matches!(
            harness
                .service
                .start_bounded_investigation(command.clone(), &authority_manifest()),
            Err(RepairError::Port { .. })
        ));
        assert_eq!(
            harness
                .service
                .load_aggregate(&group_id)
                .expect("durable investigation")
                .ledger_version,
            2
        );

        harness
            .service
            .start_bounded_investigation(command.clone(), &authority_manifest())
            .expect("redrive investigation");
        harness
            .service
            .start_bounded_investigation(command, &authority_manifest())
            .expect("exact retry");
        assert_eq!(harness.investigations.effects.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn investigation_retry_revalidates_manifest_and_never_redispatches_after_completion() {
    let manifest_harness = harness(AdmissionMode::Accepted);
    let repair_group_id = group_id();
    manifest_harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.retry-manifest"),
            group_id: repair_group_id.clone(),
            expected_ledger_version: 0,
            occurrence: occurrence("retry-manifest"),
        })
        .expect("failure");
    let command = investigation_command(repair_group_id);
    let manifest = authority_manifest();
    manifest_harness
        .service
        .start_bounded_investigation(command.clone(), &manifest)
        .expect("investigation");
    let mut corrupt_manifest = manifest;
    corrupt_manifest.capability_bindings[0].enabled = false;
    assert!(matches!(
        manifest_harness
            .service
            .start_bounded_investigation(command, &corrupt_manifest),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(
        manifest_harness.investigations.calls.load(Ordering::SeqCst),
        1
    );

    let completed = harness(AdmissionMode::Accepted);
    seed_candidate(&completed, DataCompatibilityV1::RollbackCompatible);
    let calls = completed.investigations.calls.load(Ordering::SeqCst);
    completed
        .service
        .start_bounded_investigation(
            StartInvestigationV1 {
                operation_id: id("operation.investigate.one"),
                expected_ledger_version: 1,
                investigation_id: id("investigation.one"),
                explicit_user_decision_id: id("decision.investigate.one"),
                group_id: group_id(),
                management_chat_id: id("chat.management"),
                management_run_id: id("run.management"),
                requested_capability_ids: vec![id("capability.build"), id("capability.test")],
                budget: RepairInvestigationBudgetV1 {
                    max_attempts: 4,
                    max_tool_calls: 20,
                    max_tokens: 50_000,
                    deadline_ms: 60_000,
                },
            },
            &authority_manifest(),
        )
        .expect("completed exact retry");
    assert_eq!(completed.investigations.calls.load(Ordering::SeqCst), calls);
}

#[test]
fn enrollment_preparation_redrives_from_the_committed_request_without_duplicate_effect() {
    let harness = harness(AdmissionMode::Accepted);
    let mut seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
    let report = enrollment_report(&seeded.candidate);
    *harness.bootstrap.report.lock().expect("report") = Some(report.clone());
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.retry-enrollment"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            now_epoch_ms: NOW,
        })
        .expect("capability report");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let command = RequestManagedLocalEnrollmentV1 {
        operation_id: id("operation.enrollment.retry"),
        request_id: id("enrollment.request.retry"),
        explicit_user_decision_id: id("decision.enrollment.retry"),
        expected_ledger_version: seeded.aggregate.ledger_version,
        group_id: seeded.group_id.clone(),
        candidate_id: seeded.candidate.candidate_id.clone(),
        expected_candidate_version: seeded.candidate.candidate_version,
        expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
        expected_capability_report_id: report.report_id,
        expected_capability_digest: report.capability_digest,
        now_epoch_ms: NOW,
    };
    harness
        .bootstrap
        .fail_enrollment_after_effect_once
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        harness
            .service
            .request_managed_local_enrollment(command.clone()),
        Err(RepairError::Port { .. })
    ));

    let prepared = harness
        .service
        .request_managed_local_enrollment(command.clone())
        .expect("enrollment redrive");
    assert_eq!(prepared.request_id, command.request_id);
    harness
        .service
        .request_managed_local_enrollment(command)
        .expect("exact enrollment retry");
    assert_eq!(
        harness.bootstrap.enrollment_effects.load(Ordering::SeqCst),
        1
    );
}

#[test]
fn activation_checkpoint_and_admission_uncertainty_redrive_one_baton() {
    for fail_checkpoint in [true, false] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_supported(&harness);
        let command = activate_command(&seeded);
        if fail_checkpoint {
            harness
                .management
                .fail_checkpoint_after_effect_once
                .store(true, Ordering::SeqCst);
        } else {
            harness
                .bootstrap
                .fail_admission_after_effect_once
                .store(true, Ordering::SeqCst);
        }
        assert!(matches!(
            harness.service.activate_and_restart(command.clone()),
            Err(RepairError::Port { .. })
        ));

        assert!(matches!(
            harness
                .service
                .activate_and_restart(command)
                .expect("activation redrive"),
            ActivationHandoffOutcomeV1::ReadyForCoreExit { .. }
        ));
        assert_eq!(
            harness.management.checkpoint_effects.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            harness.bootstrap.admission_effects.load(Ordering::SeqCst),
            1
        );
        assert_eq!(harness.quiescence.effects.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn quiescence_and_helper_handoff_uncertainty_have_safe_independent_redrives() {
    for fail_core_quiescence in [true, false] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_supported(&harness);
        let command = activate_command(&seeded);
        if fail_core_quiescence {
            harness
                .quiescence
                .fail_after_effect_once
                .store(true, Ordering::SeqCst);
        } else {
            harness
                .bootstrap
                .fail_quiescence_after_effect_once
                .store(true, Ordering::SeqCst);
        }
        assert!(matches!(
            harness.service.activate_and_restart(command.clone()),
            Err(RepairError::Port { .. })
        ));

        harness
            .service
            .activate_and_restart(command)
            .expect("quiescence redrive");
        assert_eq!(harness.quiescence.effects.load(Ordering::SeqCst), 1);
        assert_eq!(
            harness.bootstrap.quiescence_effects.load(Ordering::SeqCst),
            1
        );
    }
}

#[test]
fn unsupported_resume_uncertainty_retries_without_a_new_checkpoint_or_admission() {
    let harness = harness(AdmissionMode::Unsupported);
    let seeded = seed_supported(&harness);
    let command = activate_command(&seeded);
    harness
        .management
        .fail_resume_once
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        harness.service.activate_and_restart(command.clone()),
        Err(RepairError::Port { .. })
    ));

    assert!(matches!(
        harness
            .service
            .activate_and_restart(command)
            .expect("Unsupported resume redrive"),
        ActivationHandoffOutcomeV1::Unsupported(_)
    ));
    assert_eq!(
        harness.management.checkpoint_calls.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        harness.bootstrap.admission_effects.load(Ordering::SeqCst),
        1
    );
    assert_eq!(harness.management.resume_effects.load(Ordering::SeqCst), 1);
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 2);
}

fn investigation_command(group_id: aworkit_protocol::StableId) -> StartInvestigationV1 {
    StartInvestigationV1 {
        operation_id: id("operation.investigation.retry"),
        expected_ledger_version: 1,
        investigation_id: id("investigation.retry"),
        explicit_user_decision_id: id("decision.investigation.retry"),
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
    }
}
