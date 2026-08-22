use std::sync::atomic::Ordering;

use aworkit_trusted_core::*;

use crate::support::{
    AdmissionMode, ArtifactFault, NOW, activate_command, artifact, authenticated_result,
    enrollment_report, group_id, harness, id, investigation_execution_receipt, occurrence,
    prepare_candidate, seed_candidate, seed_supported, verification_evidence,
};

#[test]
fn recurring_failure_evidence_is_exact_read_before_the_first_ledger_event() {
    for fault in [
        ArtifactFault::Missing,
        ArtifactFault::Unavailable,
        ArtifactFault::HashMismatch,
        ArtifactFault::SizeMismatch,
    ] {
        let harness = harness(AdmissionMode::Accepted);
        let occurrence = occurrence("artifact-gate");
        let target = occurrence.evidence[0].artifact_id.clone();
        *harness.artifacts.fault.lock().expect("artifact fault") = Some((target, fault));

        assert!(matches!(
            harness
                .service
                .record_recurring_failure(RecordRecurringFailureV1 {
                    operation_id: id("operation.failure.artifact-gate"),
                    group_id: group_id(),
                    expected_ledger_version: 0,
                    occurrence,
                }),
            Err(RepairError::ArtifactNotReady { .. })
        ));
        assert_eq!(
            harness
                .service
                .load_aggregate(&group_id())
                .expect("aggregate")
                .ledger_version,
            0
        );
    }
}

#[test]
fn enrollment_exact_reads_the_whole_bundle_before_commit_or_helper_use() {
    for fault in [
        ArtifactFault::Missing,
        ArtifactFault::Unavailable,
        ArtifactFault::HashMismatch,
        ArtifactFault::SizeMismatch,
    ] {
        let harness = harness(AdmissionMode::Accepted);
        let mut seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
        let report = enrollment_report(&seeded.candidate);
        *harness.bootstrap.report.lock().expect("report") = Some(report.clone());
        query_enrollment_capability(&harness, &seeded, "artifact");
        seeded.aggregate = harness
            .service
            .load_aggregate(&seeded.group_id)
            .expect("aggregate");
        let target = seeded.candidate.build_bundle.artifact.artifact_id.clone();
        *harness.artifacts.fault.lock().expect("artifact fault") = Some((target, fault));

        assert!(matches!(
            harness
                .service
                .request_managed_local_enrollment(enrollment_command(
                    &seeded,
                    &report,
                    "artifact-gate",
                )),
            Err(RepairError::ArtifactNotReady { .. })
        ));
        assert_eq!(harness.bootstrap.enrollment_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness
                .service
                .load_aggregate(&seeded.group_id)
                .expect("aggregate")
                .ledger_version,
            seeded.aggregate.ledger_version
        );
    }
}

#[test]
fn enrollment_redrive_rechecks_a_committed_bundle_before_helper_handoff() {
    let harness = harness(AdmissionMode::Accepted);
    let mut seeded = seed_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
    let report = enrollment_report(&seeded.candidate);
    *harness.bootstrap.report.lock().expect("report") = Some(report.clone());
    query_enrollment_capability(&harness, &seeded, "artifact-redrive");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("aggregate");
    let command = enrollment_command(&seeded, &report, "artifact-redrive");
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
    *harness.artifacts.fault.lock().expect("artifact fault") = Some((
        seeded.candidate.build_bundle.artifact.artifact_id.clone(),
        ArtifactFault::Missing,
    ));

    assert!(matches!(
        harness.service.request_managed_local_enrollment(command),
        Err(RepairError::ArtifactNotReady { .. })
    ));
    assert_eq!(harness.bootstrap.enrollment_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn candidate_registration_exact_reads_every_required_artifact_and_fails_closed() {
    let cases = [
        ("candidate-build", ArtifactFault::Missing),
        ("previous-build", ArtifactFault::Unavailable),
        ("source-diff", ArtifactFault::HashMismatch),
        ("tests", ArtifactFault::SizeMismatch),
        ("configuration", ArtifactFault::Missing),
        ("benchmark", ArtifactFault::Unavailable),
    ];
    for (logical_name, fault) in cases {
        let harness = harness(AdmissionMode::Accepted);
        let mut prepared = prepare_candidate(&harness, DataCompatibilityV1::RollbackCompatible);
        prepared.candidate.disclosure.configuration_diff = evidence(
            "configuration diff is disclosed",
            artifact("configuration", '2'),
        );
        prepared.candidate.disclosure.benchmarks = evidence(
            "benchmark evidence is disclosed",
            artifact("benchmark", '3'),
        );
        reseal_candidate(&mut prepared.candidate);
        prepared.execution_receipt =
            investigation_execution_receipt(&prepared.investigation, &prepared.candidate);
        *harness.investigations.receipt.lock().expect("receipt") =
            Some(prepared.execution_receipt.clone());
        let target = candidate_artifacts(&prepared.candidate)
            .into_iter()
            .find(|artifact| artifact.logical_name.starts_with(logical_name))
            .expect("target artifact")
            .artifact_id
            .clone();
        *harness.artifacts.fault.lock().expect("artifact fault") = Some((target, fault));

        assert!(matches!(
            harness
                .service
                .register_candidate(RegisterRepairCandidateV1 {
                    operation_id: id(&format!("operation.candidate.artifact.{logical_name}")),
                    expected_ledger_version: 2,
                    investigation_id: prepared.investigation.investigation_id,
                    execution_receipt_id: prepared.execution_receipt.receipt.receipt_id,
                    expected_execution_receipt_hash: prepared
                        .execution_receipt
                        .receipt
                        .receipt_hash,
                    candidate: prepared.candidate,
                }),
            Err(RepairError::ArtifactNotReady { .. })
        ));
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

#[test]
fn activation_rechecks_candidate_and_runtime_build_references_before_checkpoint() {
    for runtime_current in [false, true] {
        let harness = harness(AdmissionMode::Accepted);
        let seeded = seed_supported(&harness);
        let target = if runtime_current {
            seeded
                .aggregate
                .latest_capability_report
                .as_ref()
                .expect("report")
                .current_build
                .artifact
                .artifact_id
                .clone()
        } else {
            seeded.candidate.build_bundle.artifact.artifact_id.clone()
        };
        *harness.artifacts.fault.lock().expect("artifact fault") =
            Some((target, ArtifactFault::Unavailable));

        assert!(matches!(
            harness
                .service
                .activate_and_restart(activate_command(&seeded)),
            Err(RepairError::ArtifactNotReady { .. })
        ));
        assert_eq!(
            harness.management.checkpoint_calls.load(Ordering::SeqCst),
            0
        );
        assert_eq!(harness.bootstrap.admission_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn focused_verification_artifacts_are_rechecked_before_receipt_commit() {
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
    let evidence = verification_evidence(&baton.verification_plan);
    let aggregate = harness
        .service
        .complete_focused_verification_evidence(CompleteFocusedVerificationEvidenceV1 {
            operation_id: id("operation.verification.artifact"),
            expected_ledger_version: aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            activation_id: baton.activation_id.clone(),
            current_process_generation: baton.candidate_process_generation,
            evidence: evidence.clone(),
        })
        .expect("verification evidence");
    let target = evidence.results[0].evidence[0].artifact_id.clone();
    *harness.artifacts.fault.lock().expect("artifact fault") =
        Some((target, ArtifactFault::HashMismatch));
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
                operation_id: id("operation.result.artifact-mismatch"),
                expected_ledger_version: aggregate.ledger_version,
                group_id: seeded.group_id.clone(),
                activation_id: baton.activation_id,
                current_process_generation: baton.candidate_process_generation,
                now_epoch_ms: NOW,
            }),
        Err(RepairError::ArtifactNotReady { .. })
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
fn focused_verification_redrive_rechecks_committed_evidence_before_helper_submit() {
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
    let evidence = verification_evidence(&baton.verification_plan);
    let command = CompleteFocusedVerificationEvidenceV1 {
        operation_id: id("operation.verification.artifact-redrive"),
        expected_ledger_version: aggregate.ledger_version,
        group_id: seeded.group_id.clone(),
        activation_id: baton.activation_id,
        current_process_generation: baton.candidate_process_generation,
        evidence: evidence.clone(),
    };
    harness
        .ledger
        .fail_append_after_commit_once
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        harness
            .service
            .complete_focused_verification_evidence(command.clone()),
        Err(RepairError::Port { .. })
    ));
    *harness.artifacts.fault.lock().expect("artifact fault") = Some((
        evidence.results[0].evidence[0].artifact_id.clone(),
        ArtifactFault::Unavailable,
    ));

    assert!(matches!(
        harness
            .service
            .complete_focused_verification_evidence(command),
        Err(RepairError::ArtifactNotReady { .. })
    ));
    assert!(
        harness
            .bootstrap
            .submitted_verification
            .lock()
            .expect("submitted verification")
            .is_empty()
    );
}

fn evidence(summary: &str, artifact: RepairArtifactRefV1) -> RepairEvidenceDisclosureV1 {
    RepairEvidenceDisclosureV1::Evidence {
        summary: summary.into(),
        artifacts: vec![artifact],
    }
}

fn reseal_candidate(candidate: &mut RepairCandidateV1) {
    candidate.disclosure.disclosure_hash =
        repair_disclosure_hash_v1(&candidate.disclosure).expect("disclosure hash");
    candidate.candidate_hash = repair_candidate_hash_v1(candidate).expect("candidate hash");
}

fn candidate_artifacts(candidate: &RepairCandidateV1) -> Vec<&RepairArtifactRefV1> {
    let mut artifacts = vec![
        &candidate.build_bundle.artifact,
        &candidate.disclosure.rollback_point.artifact,
    ];
    for disclosure in [
        &candidate.disclosure.source_diff,
        &candidate.disclosure.configuration_diff,
        &candidate.disclosure.tests,
        &candidate.disclosure.benchmarks,
    ] {
        if let RepairEvidenceDisclosureV1::Evidence {
            artifacts: evidence,
            ..
        } = disclosure
        {
            artifacts.extend(evidence);
        }
    }
    artifacts
}

fn query_enrollment_capability(
    harness: &crate::support::Harness,
    seeded: &crate::support::SeededRepair,
    suffix: &str,
) {
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id(&format!("operation.capability.enrollment-{suffix}")),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            now_epoch_ms: NOW,
        })
        .expect("enrollment capability");
}

fn enrollment_command(
    seeded: &crate::support::SeededRepair,
    report: &PlatformCapabilityReportV1,
    suffix: &str,
) -> RequestManagedLocalEnrollmentV1 {
    RequestManagedLocalEnrollmentV1 {
        operation_id: id(&format!("operation.enrollment.{suffix}")),
        request_id: id(&format!("enrollment.request.{suffix}")),
        explicit_user_decision_id: id(&format!("decision.enrollment.{suffix}")),
        expected_ledger_version: seeded.aggregate.ledger_version,
        group_id: seeded.group_id.clone(),
        candidate_id: seeded.candidate.candidate_id.clone(),
        expected_candidate_version: seeded.candidate.candidate_version,
        expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
        expected_capability_report_id: report.report_id.clone(),
        expected_capability_digest: report.capability_digest.clone(),
        now_epoch_ms: NOW,
    }
}
