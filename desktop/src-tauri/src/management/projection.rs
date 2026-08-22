//! Trusted-core aggregate conversion for the Management presentation.

use aworkit_trusted_core::{
    ActivationEligibilityV1, AuthenticatedBootstrapResultV1, BootstrapResultKindV1, BuildOriginV1,
    CommittedRepairEventV1, DataCompatibilityV1, DisclosureItemsV1, RepairAggregateV1,
    RepairArtifactRefV1, RepairCandidateDispositionV1, RepairCandidateV1, RepairEventV1,
    RepairEvidenceDisclosureV1, RepairPhaseV1,
};
use serde_json::{Value, json};

use super::dto::{ErrorGroupDto, ManagementChatDto, ManagementRepairProjectionDto, RepairEventDto};

/// One validated group aggregate plus the last global sequence assigned to
/// that group. Global ordering is storage-owned and must not be inferred from
/// the group's contiguous ledger sequence.
pub struct RepairProjectionGroup<'a> {
    pub aggregate: &'a RepairAggregateV1,
    pub last_global_sequence: u64,
}

/// One validated committed event paired with its storage-owned global cursor.
pub struct RepairProjectionEvent<'a> {
    pub global_sequence: u64,
    pub committed: &'a CommittedRepairEventV1,
}

/// Converts every validated aggregate. Missing artifact bytes remain exact
/// references; the presentation never invents source, test, or benchmark data.
pub fn project_aggregates(
    groups: &[RepairProjectionGroup<'_>],
    committed: &[RepairProjectionEvent<'_>],
    after_sequence: u64,
    global_version: u64,
    now_epoch_ms: u64,
) -> ManagementRepairProjectionDto {
    let latest_investigation_group = groups
        .iter()
        .filter(|group| group.aggregate.investigation.is_some())
        .max_by_key(|group| group.last_global_sequence);
    let investigation =
        latest_investigation_group.and_then(|group| group.aggregate.investigation.as_ref());
    let chat = management_chat(investigation);

    let mut ordered_groups = groups.iter().collect::<Vec<_>>();
    ordered_groups.sort_by_key(|group| std::cmp::Reverse(group.last_global_sequence));
    let error_groups = ordered_groups
        .iter()
        .filter(|group| {
            group.aggregate.fingerprint.is_some() || !group.aggregate.occurrences.is_empty()
        })
        .map(|group| error_group(group.aggregate))
        .collect();
    let candidates = ordered_groups
        .iter()
        .flat_map(|group| {
            group
                .aggregate
                .candidates
                .iter()
                .map(|candidate| candidate_projection(group.aggregate, candidate))
        })
        .collect();
    let capability_reports = ordered_groups
        .iter()
        .filter_map(|group| {
            group
                .aggregate
                .latest_capability_report
                .as_ref()
                .map(|report| capability_projection(group.aggregate, report, now_epoch_ms))
        })
        .collect();
    let evidence = ordered_groups
        .iter()
        .flat_map(|group| evidence_projection(group.aggregate))
        .collect();
    let restart_recovery = ordered_groups
        .iter()
        .find_map(|group| recovery_projection(group.aggregate));
    ManagementRepairProjectionDto {
        version: global_version,
        last_sequence: global_version,
        events: committed
            .iter()
            .filter(|event| event.global_sequence > after_sequence)
            .map(event_projection)
            .collect(),
        chat,
        error_groups,
        investigation: latest_investigation_group.and_then(|group| {
            group
                .aggregate
                .investigation
                .as_ref()
                .map(|value| investigation_projection(group.aggregate, value))
        }),
        candidates,
        capability_reports,
        evidence,
        restart_recovery,
    }
}

pub fn empty_projection() -> ManagementRepairProjectionDto {
    ManagementRepairProjectionDto {
        version: 0,
        last_sequence: 0,
        events: Vec::new(),
        chat: ManagementChatDto {
            id: None,
            title: "Management repair".to_owned(),
            scope: "Application-wide".to_owned(),
            maintainer_tier: "No active Management Chat context".to_owned(),
        },
        error_groups: Vec::new(),
        investigation: None,
        candidates: Vec::new(),
        capability_reports: Vec::new(),
        evidence: Vec::new(),
        restart_recovery: None,
    }
}

fn management_chat(
    investigation: Option<&aworkit_trusted_core::RepairInvestigationV1>,
) -> ManagementChatDto {
    ManagementChatDto {
        id: investigation.map(|value| value.management_chat_id.to_string()),
        title: investigation.map_or_else(
            || "Management repair".to_owned(),
            |_| "Management Chat".to_owned(),
        ),
        scope: "Application-wide".to_owned(),
        maintainer_tier: investigation.map_or_else(
            || "No active Management Chat context".to_owned(),
            |_| "Management Maintainer · tier:quality".to_owned(),
        ),
    }
}

fn error_group(aggregate: &RepairAggregateV1) -> ErrorGroupDto {
    let first = aggregate
        .occurrences
        .iter()
        .map(|value| value.observed_at_epoch_ms)
        .min()
        .unwrap_or(0);
    let last = aggregate
        .occurrences
        .iter()
        .max_by_key(|value| value.observed_at_epoch_ms);
    let last_repair = aggregate
        .bootstrap_result
        .as_ref()
        .map(|result| result.receipt.sealed_at_epoch_ms.to_string());
    ErrorGroupDto {
        id: aggregate.group_id.to_string(),
        fingerprint: aggregate.fingerprint.clone().unwrap_or_default(),
        title: last
            .map(|value| value.summary.clone())
            .unwrap_or_else(|| "Recurring error".to_owned()),
        occurrence_count: aggregate.occurrences.len(),
        // The repair aggregate intentionally stores semantic event IDs rather
        // than presentation Chat counts; one is the truthful lower bound.
        chat_count: usize::from(!aggregate.occurrences.is_empty()),
        first_seen_at: first.to_string(),
        last_seen_at: last
            .map(|value| value.observed_at_epoch_ms.to_string())
            .unwrap_or_else(|| "0".to_owned()),
        last_repair_at: last_repair,
        state: group_state(aggregate.phase),
        evidence_ids: aggregate
            .occurrences
            .iter()
            .flat_map(|value| value.evidence.iter())
            .map(|artifact| artifact.artifact_id.to_string())
            .collect(),
    }
}

fn investigation_projection(
    aggregate: &RepairAggregateV1,
    investigation: &aworkit_trusted_core::RepairInvestigationV1,
) -> Value {
    let candidate_ready = !aggregate.candidates.is_empty();
    json!({
        "id": investigation.investigation_id,
        "errorGroupId": investigation.group_id,
        "state": if candidate_ready { "awaiting_review" } else { "running" },
        "boundedBy": format!(
            "{} attempts · {} tool calls · {} tokens · {} ms · frozen Management authority",
            investigation.budget.max_attempts,
            investigation.budget.max_tool_calls,
            investigation.budget.max_tokens,
            investigation.budget.deadline_ms,
        ),
        "startedAt": "committed",
        "steps": [
            {"id":"recorded", "label":"Recurring failure recorded", "state":"completed"},
            {"id":"investigating", "label":"Bounded investigation dispatched", "state": if candidate_ready {"completed"} else {"active"}},
            {"id":"candidate", "label":"Candidate registered", "state": if candidate_ready {"completed"} else {"pending"}},
            {"id":"review", "label":"Awaiting your review", "state": if candidate_ready {"active"} else {"pending"}},
        ],
    })
}

fn candidate_projection(aggregate: &RepairAggregateV1, candidate: &RepairCandidateV1) -> Value {
    let disclosure = &candidate.disclosure;
    let contract_complete = disclosure_contract_complete(candidate);
    let (source_diff_evidence, source_diff_ready) =
        disclosure_evidence_projection(&disclosure.source_diff);
    let (configuration_diff_evidence, configuration_diff_ready) =
        disclosure_evidence_projection(&disclosure.configuration_diff);
    let (test_evidence, tests_ready) = disclosure_evidence_projection(&disclosure.tests);
    let (benchmark_evidence, benchmarks_ready) =
        disclosure_evidence_projection(&disclosure.benchmarks);
    let review_complete = contract_complete
        && source_diff_ready
        && configuration_diff_ready
        && tests_ready
        && benchmarks_ready;
    let authority_frozen = aggregate
        .investigation
        .as_ref()
        .is_some_and(|investigation| {
            investigation.authority.authority_manifest_hash
                == candidate.built_under_authority_manifest_hash
        });
    json!({
        "id": candidate.candidate_id,
        "version": candidate.candidate_version,
        "errorGroupId": candidate.group_id,
        "title": candidate.summary,
        "state": candidate_state(aggregate, candidate),
        "artifactId": candidate.build_bundle.artifact.artifact_id,
        "candidateHash": candidate.candidate_hash,
        "artifactHash": candidate.build_bundle.artifact.content_hash,
        "provenanceHash": candidate.provenance.provenance_hash,
        "dataCompatibility": data_compatibility(&disclosure.data_compatibility),
        "authority": {
            "decision": if authority_frozen {"frozen"} else {"blocked_broadening"},
            "manifestDigest": candidate.built_under_authority_manifest_hash,
            "summary": if authority_frozen {
                "No authority broadening; candidate is bound to the frozen Management manifest."
            } else {
                "Candidate authority does not match the frozen Management manifest."
            },
        },
        "disclosure": {
            "contractComplete": contract_complete,
            "complete": review_complete,
            "hash": disclosure.disclosure_hash,
            "diagnosis": candidate.summary,
            "sourceDiffEvidence": source_diff_evidence,
            "sourceDiffs": [],
            "configurationDiffEvidence": configuration_diff_evidence,
            "configurationDiffs": [],
            "testEvidence": test_evidence,
            "tests": [],
            "benchmarkEvidence": benchmark_evidence,
            "benchmarks": [],
            "consequences": item_projection(&disclosure.consequences),
            "uncertainty": item_projection(&disclosure.uncertainties),
            "removals": item_projection(&disclosure.removed_behaviors),
            "disables": item_projection(&disclosure.disabled_behaviors),
            "broadenings": item_projection(&disclosure.broadened_behaviors),
            "replacements": item_projection(&disclosure.replaced_behaviors),
        },
        "rollbackPoint": {
            "build": disclosure.rollback_point.artifact.logical_name,
            "artifactHash": disclosure.rollback_point.artifact.content_hash,
            "description": "Hash-bound whole-build rollback artifact projected by the trusted core.",
        },
    })
}

fn capability_projection(
    aggregate: &RepairAggregateV1,
    report: &aworkit_trusted_core::PlatformCapabilityReportV1,
    now_epoch_ms: u64,
) -> Value {
    let candidate = aggregate.candidate_exact(&report.candidate_id, report.candidate_version);
    let code = eligibility_code(report.eligibility);
    json!({
        "id": report.report_id,
        "reportVersion": aggregate.ledger_version,
        "freshness": if now_epoch_ms >= report.valid_from_epoch_ms && now_epoch_ms <= report.expires_at_epoch_ms {"fresh"} else {"stale"},
        "candidateId": report.candidate_id,
        "candidateVersion": report.candidate_version,
        "candidateHash": report.candidate_hash,
        "disclosureHash": candidate.map(|value| value.disclosure.disclosure_hash.clone()).unwrap_or_default(),
        "capabilityGeneration": report.capability_generation,
        "capabilityDigest": report.capability_digest,
        "activationProfile": if report.eligibility == ActivationEligibilityV1::SupportedManagedLocal {Value::String("ManagedLocalBuildProfileV1".to_owned())} else {Value::Null},
        "buildOrigin": build_origin(&report.build_origin),
        "enrollment": match report.eligibility {
            ActivationEligibilityV1::SupportedManagedLocal => "enrolled",
            ActivationEligibilityV1::EnrollmentRequired => "required",
            _ => "not_applicable",
        },
        "integrity": if matches!(report.build_origin, BuildOriginV1::ManagedLocal { .. }) {
            "Same-user hash/ownership; not publisher verified"
        } else {
            "No self-activation integrity guarantee"
        },
        "eligibility": {"code": code, "reason": report.reason.message},
    })
}

fn evidence_projection(aggregate: &RepairAggregateV1) -> Vec<Value> {
    let mut evidence = Vec::new();
    for occurrence in &aggregate.occurrences {
        evidence.push(json!({
            "id": occurrence.occurrence_id,
            "kind": "occurrence",
            "title": occurrence.summary,
            "status": "failed",
            "source": "Recurring-error ledger",
            "createdAt": occurrence.observed_at_epoch_ms.to_string(),
            "summary": occurrence.summary,
            "rawReference": format!("core://repair/occurrences/{}", occurrence.occurrence_id),
        }));
        evidence.extend(
            occurrence
                .evidence
                .iter()
                .map(|artifact| artifact_evidence(artifact, "occurrence")),
        );
    }
    for candidate in &aggregate.candidates {
        for (kind, disclosure) in [
            ("diff", &candidate.disclosure.source_diff),
            ("diff", &candidate.disclosure.configuration_diff),
            ("test", &candidate.disclosure.tests),
            ("benchmark", &candidate.disclosure.benchmarks),
        ] {
            evidence.extend(
                disclosure_artifacts(disclosure)
                    .iter()
                    .map(|artifact| artifact_evidence(artifact, kind)),
            );
        }
    }
    if let Some(report) = &aggregate.latest_capability_report {
        evidence.push(json!({
            "id": report.report_id,
            "kind": "capability",
            "title": "Activation capability report",
            "status": if report.eligibility == ActivationEligibilityV1::SupportedManagedLocal {"passed"} else {"uncertain"},
            "source": format!("Trusted core capability generation {}", report.capability_generation),
            "createdAt": report.valid_from_epoch_ms.to_string(),
            "summary": report.reason.message,
            "rawReference": format!("core://repair/capability/{}", report.report_id),
        }));
    }
    evidence
}

fn recovery_projection(aggregate: &RepairAggregateV1) -> Option<Value> {
    let checkpoint = aggregate.management_checkpoint.as_ref()?;
    let (state, detail, receipt_hash) = match &aggregate.bootstrap_result {
        None if aggregate.bootstrap_admission.is_some() => (
            "handed_off",
            "Bootstrap helper admitted the exact activation baton.".to_owned(),
            Value::Null,
        ),
        None => (
            "checkpointed",
            "Management Chat checkpoint is durably committed.".to_owned(),
            Value::Null,
        ),
        Some(result) => recovery_result(result),
    };
    let activation = aggregate.activation_decision.as_ref()?;
    let candidate = aggregate.candidate_exact(
        &activation.candidate_id,
        activation.expected_candidate_version,
    )?;
    Some(json!({
        "activationId": aggregate.activation_decision.as_ref().map(|value| value.activation_id.to_string()).unwrap_or_default(),
        "state": state,
        "detail": detail,
        "receiptHash": receipt_hash,
        "checkpoint": {
            "id": checkpoint.checkpoint_id,
            "chatId": checkpoint.chat_id,
            "candidateId": candidate.candidate_id,
            "candidateVersion": candidate.candidate_version,
            "createdAt": checkpoint.committed_sequence.to_string(),
        },
    }))
}

fn event_projection(event: &RepairProjectionEvent<'_>) -> RepairEventDto {
    RepairEventDto {
        sequence: event.global_sequence,
        kind: event_kind(&event.committed.event),
        occurred_at: "committed".to_owned(),
        subject_id: event.committed.group_id.to_string(),
    }
}

fn event_kind(event: &RepairEventV1) -> &'static str {
    match event {
        RepairEventV1::FailureRecorded { .. } => "failure_recorded",
        RepairEventV1::InvestigationStarted { .. } => "investigation_started",
        RepairEventV1::CandidateRegistered { .. } => "candidate_registered",
        RepairEventV1::CapabilityReported { .. } => "capability_reported",
        RepairEventV1::EnrollmentRequested { .. } => "enrollment_requested",
        RepairEventV1::EnrollmentPrepared { .. } => "enrollment_prepared",
        RepairEventV1::CandidateDecided { .. } => "candidate_decided",
        RepairEventV1::ActivationPrepared { .. } => "activation_prepared",
        RepairEventV1::BootstrapAdmissionAccepted { .. } => "bootstrap_admission_accepted",
        RepairEventV1::CoreQuiesced { .. } => "core_quiesced",
        RepairEventV1::FocusedVerificationSubmitted { .. } => "focused_verification_submitted",
        RepairEventV1::BootstrapResultReconciled { .. } => "bootstrap_result_reconciled",
        RepairEventV1::RegressionRecorded { .. } => "regression_recorded",
    }
}

fn group_state(phase: Option<RepairPhaseV1>) -> &'static str {
    match phase {
        Some(RepairPhaseV1::Investigating) => "investigating",
        Some(RepairPhaseV1::Verified) => "verified",
        Some(RepairPhaseV1::Regression) => "regression",
        Some(RepairPhaseV1::CandidateReady)
        | Some(RepairPhaseV1::EnrollmentPending)
        | Some(RepairPhaseV1::EnrollmentPrepared)
        | Some(RepairPhaseV1::ActivationPrepared)
        | Some(RepairPhaseV1::AwaitingBootstrapResult)
        | Some(RepairPhaseV1::VerificationSubmitted) => "candidate_ready",
        _ => "open",
    }
}

fn candidate_state(aggregate: &RepairAggregateV1, candidate: &RepairCandidateV1) -> &'static str {
    let is_exact = |candidate_id: &aworkit_protocol::StableId, candidate_version: u64| {
        candidate.candidate_id == *candidate_id && candidate.candidate_version == candidate_version
    };
    if let Some(decision) = &aggregate.candidate_decision
        && is_exact(&decision.candidate_id, decision.candidate_version)
    {
        return match decision.disposition {
            RepairCandidateDispositionV1::Rejected => "rejected",
            RepairCandidateDispositionV1::Deferred => "deferred",
        };
    }
    if aggregate
        .active_candidate()
        .is_none_or(|active| !is_exact(&active.candidate_id, active.candidate_version))
    {
        return "superseded";
    }
    match aggregate.phase {
        Some(RepairPhaseV1::CandidateRejected) => "rejected",
        Some(RepairPhaseV1::ActivationPrepared)
        | Some(RepairPhaseV1::AwaitingBootstrapResult)
        | Some(RepairPhaseV1::VerificationSubmitted) => "activating",
        Some(RepairPhaseV1::Verified) => "verified",
        Some(RepairPhaseV1::RolledBack) => "rolled_back",
        Some(RepairPhaseV1::Investigating) => "testing",
        _ => "ready",
    }
}

fn data_compatibility(value: &DataCompatibilityV1) -> &'static str {
    match value {
        DataCompatibilityV1::RollbackCompatible => "rollback_compatible",
        DataCompatibilityV1::DeferredUntilVerified { .. } => "deferred",
        DataCompatibilityV1::ForwardOnlyMigrationRequired { .. } => "incompatible",
    }
}

fn eligibility_code(value: ActivationEligibilityV1) -> &'static str {
    match value {
        ActivationEligibilityV1::SupportedManagedLocal => "SupportedManagedLocal",
        ActivationEligibilityV1::EnrollmentRequired => "EnrollmentRequired",
        ActivationEligibilityV1::PackagedDistribution => "PackagedDistribution",
        ActivationEligibilityV1::UnknownOrigin => "UnknownOrigin",
        ActivationEligibilityV1::ConflictingOrigin => "ConflictingOrigin",
        ActivationEligibilityV1::MismatchedEnrollment => "MismatchedCandidate",
        ActivationEligibilityV1::Unsupported => "Unsupported",
    }
}

fn build_origin(value: &BuildOriginV1) -> &'static str {
    match value {
        BuildOriginV1::ManagedLocal { .. } | BuildOriginV1::SourceCheckout { .. } => {
            "LocalSourceBuild"
        }
        BuildOriginV1::PackagedDistribution { .. } => "PackagedDistribution",
        BuildOriginV1::Unknown | BuildOriginV1::Mismatched { .. } => "Unknown",
        BuildOriginV1::Conflicting { .. } => "Conflicting",
    }
}

fn disclosure_contract_complete(candidate: &RepairCandidateV1) -> bool {
    disclosure_evidence_contract_complete(
        &candidate.disclosure.source_diff,
        &candidate.disclosure.configuration_diff,
        &candidate.disclosure.tests,
        &candidate.disclosure.benchmarks,
    )
}

fn disclosure_evidence_contract_complete(
    source_diff: &RepairEvidenceDisclosureV1,
    configuration_diff: &RepairEvidenceDisclosureV1,
    tests: &RepairEvidenceDisclosureV1,
    benchmarks: &RepairEvidenceDisclosureV1,
) -> bool {
    let required = |value: &RepairEvidenceDisclosureV1| {
        matches!(value, RepairEvidenceDisclosureV1::Evidence { .. })
    };
    let optional = |value: &RepairEvidenceDisclosureV1| {
        matches!(
            value,
            RepairEvidenceDisclosureV1::Evidence { .. }
                | RepairEvidenceDisclosureV1::NoneDeclared { .. }
                | RepairEvidenceDisclosureV1::NotPerformed { .. }
        )
    };
    required(source_diff) && optional(configuration_diff) && required(tests) && optional(benchmarks)
}

/// Without a hash-verifying artifact-read adapter, references remain visible
/// but are never converted into source lines, result labels, or pass/fail facts.
fn disclosure_evidence_projection(value: &RepairEvidenceDisclosureV1) -> (Value, bool) {
    match value {
        RepairEvidenceDisclosureV1::Evidence { artifacts, .. } => (
            json!({
                "state": "unavailable",
                "explanation": "Artifact content is unavailable because no hash-verified repair artifact reader is configured.",
                "artifactIds": artifacts.iter().map(|artifact| artifact.artifact_id.to_string()).collect::<Vec<_>>(),
            }),
            false,
        ),
        RepairEvidenceDisclosureV1::NoneDeclared { explanation } => (
            json!({
                "state": "none_declared",
                "explanation": explanation,
                "artifactIds": [],
            }),
            true,
        ),
        RepairEvidenceDisclosureV1::NotPerformed { explanation } => (
            json!({
                "state": "not_performed",
                "explanation": explanation,
                "artifactIds": [],
            }),
            true,
        ),
    }
}

fn item_projection(value: &DisclosureItemsV1) -> Vec<Value> {
    value
        .items
        .iter()
        .map(|item| {
            json!({
                "id": item.item_id,
                "label": item.label,
                "detail": item.detail,
            })
        })
        .collect()
}

fn disclosure_artifacts(value: &RepairEvidenceDisclosureV1) -> &[RepairArtifactRefV1] {
    match value {
        RepairEvidenceDisclosureV1::Evidence { artifacts, .. } => artifacts,
        RepairEvidenceDisclosureV1::NoneDeclared { .. }
        | RepairEvidenceDisclosureV1::NotPerformed { .. } => &[],
    }
}

fn artifact_evidence(artifact: &RepairArtifactRefV1, kind: &str) -> Value {
    json!({
        "id": artifact.artifact_id,
        "kind": kind,
        "title": artifact.logical_name,
        "status": "unavailable",
        "source": "Core artifact reference",
        "createdAt": "committed",
        "summary": format!("Artifact content unavailable; expected {} bytes with media type {}.", artifact.byte_size, artifact.media_type),
        "rawReference": format!("artifact://{}#{}", artifact.artifact_id, artifact.content_hash),
    })
}

fn recovery_result(result: &AuthenticatedBootstrapResultV1) -> (&'static str, String, Value) {
    let (state, detail) = match &result.receipt.result {
        BootstrapResultKindV1::Unsupported { reason } => ("unsupported", reason.message.clone()),
        BootstrapResultKindV1::ActivatedVerified { .. } => (
            "activated_verified",
            "Candidate startup and focused verification passed.".to_owned(),
        ),
        BootstrapResultKindV1::RolledBack { reason, .. } => ("rolled_back", reason.clone()),
        BootstrapResultKindV1::ManualRecoveryRequired { instructions, .. } => {
            ("manual_recovery_required", instructions.join(" "))
        }
    };
    (
        state,
        detail,
        Value::String(result.receipt.receipt_hash.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aworkit_protocol::StableId;

    #[test]
    fn optional_none_declared_and_not_performed_keep_contract_complete() {
        let required = RepairEvidenceDisclosureV1::Evidence {
            summary: "Exact evidence reference".to_owned(),
            artifacts: vec![artifact("artifact.required")],
        };
        let none_declared = RepairEvidenceDisclosureV1::NoneDeclared {
            explanation: "No configuration changes.".to_owned(),
        };
        let not_performed = RepairEvidenceDisclosureV1::NotPerformed {
            explanation: "Benchmark not required for this repair.".to_owned(),
        };

        assert!(disclosure_evidence_contract_complete(
            &required,
            &none_declared,
            &required,
            &not_performed,
        ));
    }

    #[test]
    fn artifact_references_project_unavailable_without_synthetic_results() {
        let evidence = RepairEvidenceDisclosureV1::Evidence {
            summary: "Test artifact".to_owned(),
            artifacts: vec![artifact("artifact.test")],
        };

        let (projection, ready) = disclosure_evidence_projection(&evidence);
        assert!(!ready);
        assert_eq!(projection["state"], "unavailable");
        assert_eq!(projection["artifactIds"][0], "artifact.test");
        assert!(projection.get("status").is_none());
        assert!(projection.get("lines").is_none());
        assert!(projection.get("delta").is_none());
    }

    fn artifact(id: &str) -> RepairArtifactRefV1 {
        RepairArtifactRefV1 {
            artifact_id: StableId::parse(id).expect("artifact id"),
            content_hash: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            byte_size: 1,
            media_type: "application/json".to_owned(),
            logical_name: "evidence.json".to_owned(),
        }
    }
}
