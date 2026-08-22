use std::sync::atomic::Ordering;

use aworkit_trusted_core::*;

use crate::support::{
    AdmissionMode, NOW, activate_command, bundle, enrollment_report, harness, id, seed_candidate,
    seed_supported, supported_report,
};

#[test]
fn supported_activation_commits_every_gate_before_quiescence_and_handoff() {
    let harness = harness(AdmissionMode::Accepted);
    let seeded = seed_supported(&harness);
    harness.log.lock().expect("log").clear();

    let outcome = harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("activation handoff");
    let (baton, admission, quiescence) = match outcome {
        ActivationHandoffOutcomeV1::ReadyForCoreExit {
            baton,
            admission,
            quiescence,
        } => (baton, admission, quiescence),
        ActivationHandoffOutcomeV1::Unsupported(_) => panic!("activation should be admitted"),
    };

    assert_eq!(
        repair_activation_baton_hash_v1(&baton).expect("baton hash"),
        baton.baton_hash
    );
    assert_eq!(baton.candidate_hash, seeded.candidate.candidate_hash);
    assert_eq!(
        baton.disclosure_hash,
        seeded.candidate.disclosure.disclosure_hash
    );
    assert_eq!(baton.management_checkpoint.chat_id, id("chat.management"));
    assert_eq!(admission.baton_hash, baton.baton_hash);
    assert_eq!(
        quiescence.process_generation,
        baton.current_process_generation
    );

    let log = harness.log.lock().expect("log");
    assert_ordered(
        &log,
        &[
            "management.checkpoint",
            "ledger.activation_prepared",
            "bootstrap.admit",
            "ledger.admission",
            "core.quiescence",
            "ledger.quiesced",
            "bootstrap.quiescence",
        ],
    );
    assert_eq!(
        harness.management.checkpoint_calls.load(Ordering::SeqCst),
        1
    );
    assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.bootstrap.quiescence_calls.load(Ordering::SeqCst), 1);
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert_eq!(
        aggregate.phase,
        Some(RepairPhaseV1::AwaitingBootstrapResult)
    );
}

#[test]
fn unsupported_admission_is_committed_before_quiescence_and_keeps_current_core_alive() {
    let harness = harness(AdmissionMode::Unsupported);
    let seeded = seed_supported(&harness);
    harness.log.lock().expect("log").clear();

    let outcome = harness
        .service
        .activate_and_restart(activate_command(&seeded))
        .expect("Unsupported result");
    let receipt = match outcome {
        ActivationHandoffOutcomeV1::Unsupported(receipt) => receipt,
        ActivationHandoffOutcomeV1::ReadyForCoreExit { .. } => {
            panic!("Unsupported must not quiesce")
        }
    };
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::Unsupported { .. }
    ));
    assert_eq!(receipt.recipient_process_generation.0, 1);
    assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.bootstrap.quiescence_calls.load(Ordering::SeqCst), 0);
    let log = harness.log.lock().expect("log");
    assert_ordered(
        &log,
        &[
            "management.checkpoint",
            "ledger.activation_prepared",
            "bootstrap.admit",
            "ledger.result",
            "management.resume",
        ],
    );
    assert!(!log.iter().any(|entry| entry == "core.quiescence"));
    assert!(!log.iter().any(|entry| entry == "ledger.admission"));
    drop(log);
    assert_eq!(harness.management.resume_calls.load(Ordering::SeqCst), 1);
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert_eq!(aggregate.phase, Some(RepairPhaseV1::CandidateReady));
    assert!(aggregate.bootstrap_result.is_some());
}

#[test]
fn unsafe_quiescence_never_commits_or_reaches_the_helper() {
    for unsafe_mode in [(true, false), (false, true)] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_supported(&harness);
        *harness.quiescence.unsafe_mode.lock().expect("unsafe mode") = Some(unsafe_mode);
        harness.log.lock().expect("log").clear();

        assert!(matches!(
            harness
                .service
                .activate_and_restart(activate_command(&seeded)),
            Err(RepairError::UnsafeQuiescence)
        ));
        assert_eq!(harness.bootstrap.quiescence_calls.load(Ordering::SeqCst), 0);
        let log = harness.log.lock().expect("log");
        assert!(!log.iter().any(|entry| entry == "ledger.quiesced"));
        assert!(!log.iter().any(|entry| entry == "bootstrap.quiescence"));
        drop(log);
        let aggregate = harness
            .service
            .load_aggregate(&seeded.group_id)
            .expect("aggregate");
        assert!(aggregate.bootstrap_admission.is_some());
        assert!(aggregate.quiescence.is_none());
    }
}

#[test]
fn forward_only_data_changes_are_never_handed_to_bootstrap() {
    let harness = harness(AdmissionMode::Accepted);
    let mut seeded = seed_candidate(
        &harness,
        DataCompatibilityV1::ForwardOnlyMigrationRequired {
            explanation: "migration cannot be rolled back".into(),
        },
    );
    let report = supported_report(&seeded.candidate);
    *harness.bootstrap.report.lock().expect("report") = Some(report);
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.forward"),
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
    assert!(matches!(
        harness
            .service
            .activate_and_restart(activate_command(&seeded)),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(
        harness.management.checkpoint_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn changed_rollback_point_stops_before_checkpoint_and_handoff() {
    let harness = harness(AdmissionMode::Accepted);
    let mut seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
    let mut report = supported_report(&seeded.candidate);
    report.previous_working_build = Some(bundle("unexpected-previous-build", 'd'));
    report.capability_digest = capability_report_digest_v1(&report).expect("report digest");
    *harness.bootstrap.report.lock().expect("report") = Some(report);
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.changed-rollback"),
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

    assert!(matches!(
        harness
            .service
            .activate_and_restart(activate_command(&seeded)),
        Err(RepairError::InvalidContract(_))
    ));
    assert_eq!(
        harness.management.checkpoint_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn enrollment_is_a_separate_durable_user_request_and_never_auto_activates() {
    let harness = harness(AdmissionMode::Accepted);
    let mut seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
    let report = enrollment_report(&seeded.candidate);
    *harness.bootstrap.report.lock().expect("report") = Some(report.clone());
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.enrollment"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            now_epoch_ms: NOW,
        })
        .expect("EnrollmentRequired report");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    harness.log.lock().expect("log").clear();

    let prepared = harness
        .service
        .request_managed_local_enrollment(RequestManagedLocalEnrollmentV1 {
            operation_id: id("operation.enrollment.request"),
            request_id: id("enrollment.request.one"),
            explicit_user_decision_id: id("decision.enrollment.one"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            expected_capability_report_id: report.report_id,
            expected_capability_digest: report.capability_digest,
            now_epoch_ms: NOW,
        })
        .expect("enrollment prepared");
    assert_eq!(prepared.request_id, id("enrollment.request.one"));
    let log = harness.log.lock().expect("log");
    assert_ordered(
        &log,
        &[
            "ledger.enrollment_requested",
            "bootstrap.enroll",
            "ledger.enrollment_prepared",
        ],
    );
    drop(log);
    assert_eq!(harness.bootstrap.enrollment_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        harness.management.checkpoint_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(harness.quiescence.calls.load(Ordering::SeqCst), 0);
    let aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    assert_eq!(aggregate.phase, Some(RepairPhaseV1::EnrollmentPrepared));
    assert!(aggregate.activation_baton.is_none());
}

#[test]
fn prepared_enrollment_can_requery_a_new_generation_and_then_activate() {
    let harness = harness(AdmissionMode::Accepted);
    let mut seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
    let enrollment = enrollment_report(&seeded.candidate);
    *harness.bootstrap.report.lock().expect("report") = Some(enrollment.clone());
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.enrollment-before-requery"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            now_epoch_ms: NOW,
        })
        .expect("enrollment capability");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    harness
        .service
        .request_managed_local_enrollment(RequestManagedLocalEnrollmentV1 {
            operation_id: id("operation.enrollment.before-requery"),
            request_id: id("enrollment.request.before-requery"),
            explicit_user_decision_id: id("decision.enrollment.before-requery"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            expected_capability_report_id: enrollment.report_id,
            expected_capability_digest: enrollment.capability_digest,
            now_epoch_ms: NOW,
        })
        .expect("prepared enrollment");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("prepared aggregate");
    let mut supported = supported_report(&seeded.candidate);
    supported.report_id = id("capability.report.post-enrollment");
    supported.capability_generation = 8;
    supported.capability_digest =
        capability_report_digest_v1(&supported).expect("post-enrollment digest");
    *harness.bootstrap.report.lock().expect("report") = Some(supported);
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.post-enrollment"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            now_epoch_ms: NOW,
        })
        .expect("post-enrollment managed-local capability");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("supported aggregate");

    assert!(matches!(
        harness
            .service
            .activate_and_restart(activate_command(&seeded))
            .expect("post-enrollment activation"),
        ActivationHandoffOutcomeV1::ReadyForCoreExit { .. }
    ));
}

fn assert_ordered(log: &[String], expected: &[&str]) {
    let positions = expected
        .iter()
        .map(|entry| {
            log.iter()
                .position(|observed| observed == entry)
                .unwrap_or_else(|| panic!("missing log entry {entry:?} in {log:?}"))
        })
        .collect::<Vec<_>>();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{log:?}"
    );
}
