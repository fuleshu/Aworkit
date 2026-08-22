use std::sync::{Arc, Mutex};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::*;
use sha2::{Digest, Sha256};

mod ports;

pub use ports::{
    AdmissionMode, ArtifactFault, FakeArtifactIntegrity, FakeBootstrap, FakeManagement,
};
use ports::{FakeInvestigation, FakeQuiescence, MemoryRepairLedger};

pub const NOW: u64 = 1_000_000;

pub struct Harness {
    pub service: RepairOrchestratorV1,
    pub ledger: Arc<MemoryRepairLedger>,
    pub bootstrap: Arc<FakeBootstrap>,
    pub investigations: Arc<FakeInvestigation>,
    pub management: Arc<FakeManagement>,
    pub quiescence: Arc<FakeQuiescence>,
    pub artifacts: Arc<FakeArtifactIntegrity>,
    pub log: Arc<Mutex<Vec<String>>>,
}

pub struct SeededRepair {
    pub group_id: StableId,
    pub candidate: RepairCandidateV1,
    pub aggregate: RepairAggregateV1,
    pub execution_receipt: AuthenticatedInvestigationExecutionReceiptV1,
}

pub struct PreparedRepair {
    pub group_id: StableId,
    pub candidate: RepairCandidateV1,
    pub investigation: RepairInvestigationV1,
    pub execution_receipt: AuthenticatedInvestigationExecutionReceiptV1,
}

pub fn harness(mode: AdmissionMode) -> Harness {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ledger = Arc::new(MemoryRepairLedger::new(log.clone()));
    let bootstrap = Arc::new(FakeBootstrap::new(log.clone(), mode));
    let investigations = Arc::new(FakeInvestigation::new(log.clone()));
    let management = Arc::new(FakeManagement::new(log.clone()));
    let quiescence = Arc::new(FakeQuiescence::new(log.clone()));
    let artifacts = Arc::new(FakeArtifactIntegrity::new(log.clone()));
    let service = RepairOrchestratorV1::new(
        ledger.clone(),
        bootstrap.clone(),
        investigations.clone(),
        management.clone(),
        quiescence.clone(),
        artifacts.clone(),
    );
    Harness {
        service,
        ledger,
        bootstrap,
        investigations,
        management,
        quiescence,
        artifacts,
        log,
    }
}

pub fn seed_candidate(harness: &Harness, compatibility: DataCompatibilityV1) -> SeededRepair {
    let prepared = prepare_candidate(harness, compatibility);
    let aggregate = harness
        .service
        .register_candidate(RegisterRepairCandidateV1 {
            operation_id: id("operation.candidate.one"),
            expected_ledger_version: 2,
            investigation_id: prepared.investigation.investigation_id,
            execution_receipt_id: prepared.execution_receipt.receipt.receipt_id.clone(),
            expected_execution_receipt_hash: prepared
                .execution_receipt
                .receipt
                .receipt_hash
                .clone(),
            candidate: prepared.candidate.clone(),
        })
        .expect("candidate registered");
    SeededRepair {
        group_id: prepared.group_id,
        candidate: prepared.candidate,
        aggregate,
        execution_receipt: prepared.execution_receipt,
    }
}

pub fn prepare_candidate(harness: &Harness, compatibility: DataCompatibilityV1) -> PreparedRepair {
    let group_id = group_id();
    let authority = authority_manifest();
    harness
        .service
        .record_recurring_failure(RecordRecurringFailureV1 {
            operation_id: id("operation.failure.one"),
            group_id: group_id.clone(),
            expected_ledger_version: 0,
            occurrence: occurrence("one"),
        })
        .expect("failure recorded");
    let investigation = harness
        .service
        .start_bounded_investigation(
            StartInvestigationV1 {
                operation_id: id("operation.investigate.one"),
                expected_ledger_version: 1,
                investigation_id: id("investigation.one"),
                explicit_user_decision_id: id("decision.investigate.one"),
                group_id: group_id.clone(),
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
            &authority,
        )
        .expect("investigation started");
    let candidate = candidate(&group_id, &authority.manifest_hash, compatibility);
    let execution_receipt = investigation_execution_receipt(&investigation, &candidate);
    *harness
        .investigations
        .receipt
        .lock()
        .expect("execution receipt") = Some(execution_receipt.clone());
    PreparedRepair {
        group_id,
        candidate,
        investigation,
        execution_receipt,
    }
}

pub fn seed_supported(harness: &Harness) -> SeededRepair {
    let mut seeded = seed_candidate(harness, DataCompatibilityV1::RollbackCompatible);
    let report = supported_report(&seeded.candidate);
    *harness.bootstrap.report.lock().expect("report lock") = Some(report.clone());
    harness
        .service
        .query_activation_capability(QueryActivationCapabilityV1 {
            operation_id: id("operation.capability.one"),
            expected_ledger_version: seeded.aggregate.ledger_version,
            group_id: seeded.group_id.clone(),
            candidate_id: seeded.candidate.candidate_id.clone(),
            expected_candidate_version: seeded.candidate.candidate_version,
            expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
            now_epoch_ms: NOW,
        })
        .expect("capability query");
    seeded.aggregate = harness
        .service
        .load_aggregate(&seeded.group_id)
        .expect("supported aggregate");
    seeded
}

pub fn activate_command(seeded: &SeededRepair) -> ActivateAndRestartV1 {
    let report = seeded
        .aggregate
        .latest_capability_report
        .as_ref()
        .expect("capability report");
    ActivateAndRestartV1 {
        operation_id: id("operation.activate.one"),
        expected_ledger_version: seeded.aggregate.ledger_version,
        group_id: seeded.group_id.clone(),
        activation_id: id("activation.one"),
        baton_id: id("baton.one"),
        explicit_user_decision_id: id("decision.activate.one"),
        candidate_id: seeded.candidate.candidate_id.clone(),
        expected_candidate_version: seeded.candidate.candidate_version,
        expected_candidate_hash: seeded.candidate.candidate_hash.clone(),
        expected_capability_report_id: report.report_id.clone(),
        expected_capability_digest: report.capability_digest.clone(),
        current_process_generation: ProcessGeneration(1),
        deadlines: BootstrapDeadlinesV1 {
            admission_ms: 1_000,
            cleanup_ms: 2_000,
            startup_ms: 5_000,
            focused_verification_ms: 4_000,
            rollback_ms: 5_000,
            result_read_ms: 2_000,
        },
        now_epoch_ms: NOW,
    }
}

pub fn authority_manifest() -> AuthorityManifestV1 {
    let mut capability_bindings = vec![
        capability("capability.build"),
        capability("capability.test"),
    ];
    capability_bindings.sort_by(|left, right| left.capability_id.cmp(&right.capability_id));
    let bytes = serde_jcs::to_vec(&capability_bindings).expect("authority encoding");
    let manifest_hash = format!("{:x}", Sha256::digest(bytes));
    AuthorityManifestV1 {
        manifest_id: id(&format!("manifest.{}", &manifest_hash[..32])),
        manifest_hash,
        capability_bindings,
        summary: "two frozen repair capabilities".into(),
    }
}

fn capability(value: &str) -> CapabilityBindingV1 {
    CapabilityBindingV1 {
        capability_id: id(value),
        adapter_id: id(&format!("adapter.{value}")),
        adapter_version: "1.0.0".into(),
        descriptor_hash: hash('d'),
        extension: None,
        required_isolation_profile: None,
        enabled: true,
        compatible: true,
        approval: ApprovalRequirement::PerInvocation,
        allowed_node_types: vec!["management.repair".into()],
    }
}

pub fn occurrence(suffix: &str) -> ErrorOccurrenceV1 {
    ErrorOccurrenceV1 {
        occurrence_id: id(&format!("occurrence.{suffix}")),
        fingerprint: hash('f'),
        summary: "compiler failed with normalized diagnostic".into(),
        semantic_event_id: id(&format!("event.failure.{suffix}")),
        attempt_id: Some(id(&format!("attempt.{suffix}"))),
        diagnostic_record_id: Some(id(&format!("diagnostic.{suffix}"))),
        evidence: vec![artifact("diagnostic", '1')],
        observed_at_epoch_ms: NOW,
    }
}

pub fn group_id() -> StableId {
    repair_group_id_for_fingerprint_v1(&hash('f')).expect("fingerprint group")
}

pub fn investigation_execution_receipt(
    investigation: &RepairInvestigationV1,
    candidate: &RepairCandidateV1,
) -> AuthenticatedInvestigationExecutionReceiptV1 {
    let mut receipt = InvestigationExecutionReceiptV1 {
        schema_version: REPAIR_SCHEMA_VERSION_V1,
        receipt_id: id("investigation.receipt.one"),
        investigation_id: investigation.investigation_id.clone(),
        group_id: investigation.group_id.clone(),
        management_chat_id: investigation.management_chat_id.clone(),
        management_run_id: investigation.management_run_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_version: candidate.candidate_version,
        candidate_hash: candidate.candidate_hash.clone(),
        authority_manifest_id: investigation.authority.authority_manifest_id.clone(),
        authority_manifest_hash: investigation.authority.authority_manifest_hash.clone(),
        frozen_capability_ids: investigation.authority.capability_ids.clone(),
        executed_capability_ids: investigation.authority.capability_ids.clone(),
        frozen_budget: investigation.budget.clone(),
        observed_usage: RepairInvestigationUsageV1 {
            attempts: 1,
            tool_calls: investigation.authority.capability_ids.len() as u32,
            tokens: 1_000,
            elapsed_ms: 1_000,
        },
        completed_at_epoch_ms: NOW,
        receipt_hash: String::new(),
    };
    receipt.receipt_hash =
        investigation_execution_receipt_hash_v1(&receipt).expect("execution receipt hash");
    AuthenticatedInvestigationExecutionReceiptV1 {
        receipt,
        peer: InvestigationExecutionPeerProofV1 {
            same_user_authenticated: true,
            ownership_hash: hash('0'),
            channel_binding_hash: hash('1'),
        },
    }
}

pub fn candidate(
    group_id: &StableId,
    authority_hash: &str,
    compatibility: DataCompatibilityV1,
) -> RepairCandidateV1 {
    let mut plan = FocusedVerificationPlanV1 {
        plan_id: id("verification.plan.one"),
        checks: vec![FocusedVerificationCheckV1 {
            check_id: id("verification.check.smoke"),
            label: "focused smoke verification".into(),
            capability_id: id("capability.test"),
            timeout_ms: 30_000,
        }],
        plan_hash: String::new(),
    };
    plan.plan_hash = focused_verification_plan_hash_v1(&plan).expect("plan hash");
    let mut provenance = BuildProvenanceV1 {
        source_revision: "revision-123".into(),
        source_tree_hash: hash('2'),
        workspace_identity_hash: hash('3'),
        toolchain_hash: hash('4'),
        build_manifest_hash: hash('5'),
        provenance_hash: String::new(),
    };
    provenance.provenance_hash = build_provenance_hash_v1(&provenance).expect("provenance hash");
    let mut disclosure = RepairDisclosureV1 {
        source_diff: evidence("source diff", "source-diff", '6'),
        configuration_diff: RepairEvidenceDisclosureV1::NoneDeclared {
            explanation: "no configuration changes".into(),
        },
        tests: evidence("focused tests passed", "tests", '7'),
        benchmarks: RepairEvidenceDisclosureV1::NotPerformed {
            explanation: "no performance-sensitive code changed".into(),
        },
        consequences: items(vec!["replaces the failing compiler path"]),
        removed_behaviors: none_items(),
        disabled_behaviors: none_items(),
        broadened_behaviors: none_items(),
        replaced_behaviors: items(vec!["replaces the failing compiler path"]),
        uncertainties: items(vec!["platform-specific startup timing remains bounded"]),
        data_compatibility: compatibility,
        rollback_point: bundle("previous-build", '8'),
        verification_plan: plan,
        disclosure_hash: String::new(),
    };
    disclosure.disclosure_hash = repair_disclosure_hash_v1(&disclosure).expect("disclosure hash");
    let mut candidate = RepairCandidateV1 {
        candidate_id: id("candidate.one"),
        group_id: group_id.clone(),
        candidate_version: 1,
        summary: "repair compiler crash and preserve rollback".into(),
        build_bundle: bundle("candidate-build", '9'),
        provenance,
        built_under_authority_manifest_hash: authority_hash.into(),
        disclosure,
        candidate_hash: String::new(),
    };
    candidate.candidate_hash = repair_candidate_hash_v1(&candidate).expect("candidate hash");
    candidate
}

pub fn supported_report(candidate: &RepairCandidateV1) -> PlatformCapabilityReportV1 {
    capability_report(
        candidate,
        BuildOriginV1::ManagedLocal {
            enrollment_digest: hash('a'),
            active_slot_hash: hash('b'),
        },
        ActivationEligibilityV1::SupportedManagedLocal,
        Some(bundle("previous-build", '8')),
    )
}

pub fn enrollment_report(candidate: &RepairCandidateV1) -> PlatformCapabilityReportV1 {
    capability_report(
        candidate,
        BuildOriginV1::SourceCheckout {
            projected_provenance_hash: candidate.provenance.provenance_hash.clone(),
        },
        ActivationEligibilityV1::EnrollmentRequired,
        None,
    )
}

fn capability_report(
    candidate: &RepairCandidateV1,
    build_origin: BuildOriginV1,
    eligibility: ActivationEligibilityV1,
    previous_working_build: Option<BuildBundleRefV1>,
) -> PlatformCapabilityReportV1 {
    let mut report = PlatformCapabilityReportV1 {
        schema_version: REPAIR_SCHEMA_VERSION_V1,
        report_id: id("capability.report.one"),
        candidate_id: candidate.candidate_id.clone(),
        candidate_version: candidate.candidate_version,
        candidate_hash: candidate.candidate_hash.clone(),
        capability_generation: 7,
        build_origin,
        eligibility,
        reason: PlatformReasonV1 {
            code: "managed_local_status".into(),
            message: "exact managed-local activation status".into(),
            next_steps: vec!["review the disclosed candidate".into()],
        },
        current_build: bundle("current-build", 'c'),
        previous_working_build,
        valid_from_epoch_ms: NOW - 100,
        expires_at_epoch_ms: NOW + 10_000,
        capability_digest: String::new(),
    };
    report.capability_digest = capability_report_digest_v1(&report).expect("capability digest");
    report
}

pub fn verification_evidence(plan: &FocusedVerificationPlanV1) -> FocusedVerificationEvidenceV1 {
    let mut evidence = FocusedVerificationEvidenceV1 {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        results: vec![FocusedVerificationCheckResultV1 {
            check_id: plan.checks[0].check_id.clone(),
            passed: true,
            summary: "focused smoke verification passed".into(),
            evidence: vec![artifact("verification", 'e')],
        }],
        evidence_hash: String::new(),
    };
    evidence.evidence_hash =
        focused_verification_evidence_hash_v1(&evidence).expect("evidence hash");
    evidence
}

pub fn authenticated_result(
    baton: &RepairActivationBatonV1,
    result: BootstrapResultKindV1,
    recipient: ProcessGeneration,
) -> AuthenticatedBootstrapResultV1 {
    let mut receipt = BootstrapResultV1 {
        schema_version: REPAIR_SCHEMA_VERSION_V1,
        receipt_id: id("bootstrap.receipt.one"),
        activation_id: baton.activation_id.clone(),
        baton_hash: baton.baton_hash.clone(),
        management_checkpoint_id: baton.management_checkpoint.checkpoint_id.clone(),
        recipient_process_generation: recipient,
        sealed_at_epoch_ms: NOW,
        result,
        receipt_hash: String::new(),
    };
    receipt.receipt_hash = bootstrap_result_hash_v1(&receipt).expect("receipt hash");
    AuthenticatedBootstrapResultV1 {
        receipt,
        peer: BootstrapPeerProofV1 {
            same_user_authenticated: true,
            recipient_process_generation: recipient,
            ownership_hash: hash('0'),
            channel_binding_hash: hash('1'),
        },
    }
}

pub fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable id")
}

pub fn hash(character: char) -> String {
    format!(
        "sha256:{}",
        std::iter::repeat_n(character, 64).collect::<String>()
    )
}

pub fn artifact(name: &str, hash_character: char) -> RepairArtifactRefV1 {
    RepairArtifactRefV1 {
        artifact_id: id(&format!("artifact.{name}")),
        content_hash: hash(hash_character),
        byte_size: 128,
        media_type: "application/json".into(),
        logical_name: format!("{name}.json"),
    }
}

pub fn bundle(name: &str, hash_character: char) -> BuildBundleRefV1 {
    BuildBundleRefV1 {
        artifact: RepairArtifactRefV1 {
            artifact_id: id(&format!("artifact.{name}")),
            content_hash: hash(hash_character),
            byte_size: 4_096,
            media_type: "application/vnd.aworkit.build".into(),
            logical_name: format!("{name}.bundle"),
        },
        manifest_relative_entry: "bin/aworkit".into(),
    }
}

fn evidence(summary: &str, name: &str, hash_character: char) -> RepairEvidenceDisclosureV1 {
    RepairEvidenceDisclosureV1::Evidence {
        summary: summary.into(),
        artifacts: vec![artifact(name, hash_character)],
    }
}

fn items(values: Vec<&str>) -> DisclosureItemsV1 {
    DisclosureItemsV1 {
        items: values
            .into_iter()
            .enumerate()
            .map(|(index, value)| DisclosureItemV1 {
                item_id: id(&format!("disclosure.item.{index}")),
                label: value.to_owned(),
                detail: value.to_owned(),
            })
            .collect(),
        none_declared: false,
    }
}

fn none_items() -> DisclosureItemsV1 {
    DisclosureItemsV1 {
        items: Vec::new(),
        none_declared: true,
    }
}
