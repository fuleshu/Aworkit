//! Hermetic tests for the authenticated bootstrap protocol and generation fence.

use std::sync::{Arc, Mutex};

use aworkit_protocol::{
    ActivationEligibilityV1, BootstrapAdmissionV1, BootstrapDeadlinesV1, BuildBundleRefV1,
    BuildOriginV1, BuildProvenanceV1, EnrollmentPreparedV1, FocusedVerificationPlanV1,
    ManagedLocalEnrollmentRequestV1, ManagementCheckpointRefV1, PlatformCapabilityReportV1,
    PlatformReasonV1, RepairActivationBatonV1, RepairArtifactRefV1,
};
use aworkit_protocol::{
    CapabilityOutcomeClassV1, CapabilityOutcomeV1, ProcessGeneration, StableId,
};

use crate::journal::{
    ActivationJournal, ActivationJournalPortV1, BootstrapPhaseAdvanceV1, BootstrapPhaseV1,
    InMemoryJournalStorage, JournalStorage, canonical_hash,
};

use super::{
    BootstrapCommandV1, BootstrapEnrollmentPortV1, BootstrapGateway, BootstrapPreflightPortV1,
    BootstrapProtocolPortV1, EnrollmentPlanV1, EnrollmentPreparationV1, GatewayError,
    HelperIdentityV1, LocalBuildEnrollmentStateV1, LocalBuildEnrollmentV1, OwnershipFactsV1,
    PeerIdentityV1,
};

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("valid stable id")
}

fn hash(byte: u8) -> String {
    format!("sha256:{}", format!("{byte:02x}").repeat(32))
}

fn artifact(name: &str, byte: u8) -> RepairArtifactRefV1 {
    RepairArtifactRefV1 {
        artifact_id: id(name),
        content_hash: hash(byte),
        byte_size: 4096,
        media_type: "application/octet-stream".to_owned(),
        logical_name: format!("{name}.bundle"),
    }
}

fn bundle(name: &str, byte: u8) -> BuildBundleRefV1 {
    BuildBundleRefV1 {
        artifact: artifact(name, byte),
        manifest_relative_entry: "app/manifest.json".to_owned(),
    }
}

fn peer(generation: u64) -> PeerIdentityV1 {
    PeerIdentityV1 {
        peer_process_generation: ProcessGeneration(generation),
        peer_executable_hash: hash(0x11),
        peer_os_identity_hash: hash(0x12),
    }
}

fn report() -> PlatformCapabilityReportV1 {
    let mut report = PlatformCapabilityReportV1 {
        schema_version: 1,
        report_id: id("report.1"),
        candidate_id: id("candidate.1"),
        candidate_version: 7,
        candidate_hash: hash(0x21),
        capability_generation: 3,
        build_origin: BuildOriginV1::ManagedLocal {
            enrollment_digest: hash(0x22),
            active_slot_hash: hash(0x23),
        },
        eligibility: ActivationEligibilityV1::SupportedManagedLocal,
        reason: PlatformReasonV1 {
            code: "supported".to_owned(),
            message: "managed local profile available".to_owned(),
            next_steps: Vec::new(),
        },
        current_build: bundle("current.1", 0x23),
        previous_working_build: Some(bundle("previous.1", 0x24)),
        valid_from_epoch_ms: 1,
        expires_at_epoch_ms: 100_000,
        capability_digest: String::new(),
    };
    report.capability_digest = canonical_hash(&report).expect("report hash");
    report
}

fn plan() -> EnrollmentPlanV1 {
    let mut plan = EnrollmentPlanV1 {
        plan_id: id("plan.1"),
        installation_id: id("installation.1"),
        profile_version: 1,
        helper_root_identity_hash: hash(0x31),
        initial_active_slot_root_hash: hash(0x32),
        selector_identity_hash: hash(0x33),
        journal_identity_hash: hash(0x34),
        plan_hash: String::new(),
    };
    plan.plan_hash = canonical_hash(&plan).expect("plan hash");
    plan
}

fn enrollment_request() -> ManagedLocalEnrollmentRequestV1 {
    ManagedLocalEnrollmentRequestV1 {
        request_id: id("enrollment.1"),
        explicit_user_decision_id: id("decision.1"),
        group_id: id("group.1"),
        candidate_id: id("candidate.1"),
        candidate_version: 7,
        candidate_hash: hash(0x21),
        projected_provenance_hash: hash(0x41),
        whole_bundle: bundle("candidate.1", 0x21),
        capability_report_id: id("report.1"),
        capability_digest: report().capability_digest,
    }
}

fn enrollment() -> LocalBuildEnrollmentV1 {
    LocalBuildEnrollmentV1 {
        installation_id: id("installation.1"),
        profile_version: 1,
        enrollment_digest: hash(0x22),
        active_slot_hash: hash(0x23),
        current_bundle_hash: hash(0x23),
        selector_identity_hash: hash(0x33),
        helper_identity_hash: hash(0x51),
        launcher_identity_hash: hash(0x52),
        journal_identity_hash: hash(0x34),
        ownership: OwnershipFactsV1 {
            per_user_owned: true,
            same_volume: true,
            selector_atomic: true,
            helper_survives_outside_slots: true,
        },
        state: LocalBuildEnrollmentStateV1::Enrolled,
    }
}

fn provenance() -> BuildProvenanceV1 {
    BuildProvenanceV1 {
        source_revision: "abc123".to_owned(),
        source_tree_hash: hash(0x61),
        workspace_identity_hash: hash(0x62),
        toolchain_hash: hash(0x63),
        build_manifest_hash: hash(0x64),
        provenance_hash: hash(0x65),
    }
}

fn baton(capability: &PlatformCapabilityReportV1) -> RepairActivationBatonV1 {
    let mut baton = RepairActivationBatonV1 {
        schema_version: 1,
        baton_id: id("baton.1"),
        activation_id: id("activation.1"),
        group_id: id("group.1"),
        candidate_id: capability.candidate_id.clone(),
        candidate_version: capability.candidate_version,
        candidate_hash: capability.candidate_hash.clone(),
        candidate_bundle: bundle("candidate.1", 0x21),
        disclosure_hash: hash(0x71),
        provenance_hash: hash(0x65),
        enrollment_digest: hash(0x22),
        capability_report_id: capability.report_id.clone(),
        capability_generation: capability.capability_generation,
        capability_digest: capability.capability_digest.clone(),
        previous_working_build: bundle("previous.1", 0x24),
        management_checkpoint: ManagementCheckpointRefV1 {
            checkpoint_id: id("checkpoint.1"),
            chat_id: id("chat.1"),
            run_id: id("run.1"),
            committed_sequence: 10,
            snapshot_hash: hash(0x72),
            checkpoint_hash: hash(0x73),
        },
        verification_plan: FocusedVerificationPlanV1 {
            plan_id: id("verification.1"),
            checks: Vec::new(),
            plan_hash: hash(0x74),
        },
        current_process_generation: ProcessGeneration(10),
        candidate_process_generation: ProcessGeneration(11),
        rollback_process_generation: ProcessGeneration(12),
        deadlines: BootstrapDeadlinesV1 {
            admission_ms: 5_000,
            cleanup_ms: 10_000,
            startup_ms: 10_000,
            focused_verification_ms: 10_000,
            rollback_ms: 10_000,
            result_read_ms: 5_000,
        },
        expires_at_epoch_ms: 90_000,
        baton_hash: String::new(),
    };
    baton.baton_hash = canonical_hash(&baton).expect("baton hash");
    baton
}

fn helper() -> HelperIdentityV1 {
    HelperIdentityV1 {
        helper_identity_hash: hash(0x51),
        profile_version: 1,
        enrollment_identities: crate::journal::EnrollmentIdentitiesV1 {
            managed_root_identity_hash: hash(0x31),
            launcher_identity_hash: hash(0x52),
            journal_identity_hash: hash(0x34),
            selector_identity_hash: hash(0x33),
        },
    }
}

struct Preflight {
    report: Mutex<PlatformCapabilityReportV1>,
}

impl BootstrapPreflightPortV1 for Preflight {
    fn capability_report(
        &self,
        _provenance: &BuildProvenanceV1,
        _enrollment: &LocalBuildEnrollmentV1,
        _candidate: &BuildBundleRefV1,
        _previous: Option<&BuildBundleRefV1>,
    ) -> Result<PlatformCapabilityReportV1, String> {
        Ok(self.report.lock().expect("report lock").clone())
    }

    fn revalidate_baton_binding(
        &self,
        _baton: &RepairActivationBatonV1,
    ) -> Result<PlatformCapabilityReportV1, String> {
        Ok(self.report.lock().expect("report lock").clone())
    }

    fn enrollment_plan(
        &self,
        _request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPlanV1, String> {
        Ok(plan())
    }
}

struct EnrollmentMaterializer {
    storage: Arc<InMemoryJournalStorage>,
}

impl BootstrapEnrollmentPortV1 for EnrollmentMaterializer {
    fn materialize(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
        _plan: &EnrollmentPlanV1,
    ) -> Result<EnrollmentPreparationV1, String> {
        if self.storage.record_count() != 1 {
            return Err("enrollment intent was not durable before materialization".to_owned());
        }
        Ok(EnrollmentPreparationV1 {
            observation: crate::journal::EnrollmentObservationV1 {
                initial_active_bundle_hash: hash(0x23),
                published_slot_verified: true,
            },
            prepared: EnrollmentPreparedV1 {
                preparation_id: id("preparation.1"),
                request_id: request.request_id.clone(),
                enrollment_digest: hash(0x22),
                stable_launcher: "stable-launcher".to_owned(),
                restart_instructions: vec!["restart explicitly".to_owned()],
            },
        })
    }
}

type Harness = (
    BootstrapGateway,
    Arc<ActivationJournal>,
    Arc<InMemoryJournalStorage>,
    Arc<Preflight>,
);

fn harness() -> Harness {
    let storage = Arc::new(InMemoryJournalStorage::default());
    let journal = Arc::new(ActivationJournal::new(
        Arc::clone(&storage) as Arc<dyn JournalStorage>
    ));
    let preflight = Arc::new(Preflight {
        report: Mutex::new(report()),
    });
    let gateway = BootstrapGateway::new(
        Arc::clone(&journal) as Arc<dyn ActivationJournalPortV1>,
        Arc::clone(&preflight) as Arc<dyn BootstrapPreflightPortV1>,
        Arc::new(EnrollmentMaterializer {
            storage: Arc::clone(&storage),
        }),
        helper(),
    );
    (gateway, journal, storage, preflight)
}

fn admit(
    gateway: &BootstrapGateway,
    capability: &PlatformCapabilityReportV1,
) -> (StableId, RepairActivationBatonV1) {
    let peer = peer(10);
    let challenge = gateway
        .begin_bootstrap_challenge(1_000, &peer)
        .expect("challenge");
    let baton = baton(capability);
    assert!(matches!(
        gateway
            .submit_repair_activation_baton(1_001, &peer, &baton)
            .expect("admission"),
        BootstrapAdmissionV1::Accepted(_)
    ));
    (challenge.challenge_id, baton)
}

#[test]
fn challenges_use_fresh_entropy_and_are_consumed_once() {
    let (gateway, _, _, _) = harness();
    let peer = peer(10);
    let first = gateway
        .begin_bootstrap_challenge(1_000, &peer)
        .expect("first challenge");
    let second = gateway
        .begin_bootstrap_challenge(1_000, &peer)
        .expect("second challenge");
    assert_ne!(first.nonce, second.nonce);

    let capability = report();
    let mut stale = baton(&capability);
    stale.current_process_generation = ProcessGeneration(9);
    stale.baton_hash.clear();
    stale.baton_hash = canonical_hash(&stale).expect("rehash");
    assert!(matches!(
        gateway.submit_repair_activation_baton(1_001, &peer, &stale),
        Err(GatewayError::StaleGeneration)
    ));
    assert!(matches!(
        gateway.submit_repair_activation_baton(1_002, &peer, &baton(&capability)),
        Err(GatewayError::ChallengeConsumed)
    ));
}

#[test]
fn baton_is_journaled_before_an_idempotent_admission() {
    let (gateway, journal, storage, _) = harness();
    let capability = report();
    let (_challenge_id, baton) = admit(&gateway, &capability);
    assert_eq!(storage.record_count(), 2);
    let recovery = journal
        .load_activation_recovery(&baton.activation_id)
        .expect("recovery")
        .expect("activation");
    assert_eq!(recovery.phase, BootstrapPhaseV1::BatonDurable);
    assert_eq!(recovery.baton.expect("baton").baton_id, baton.baton_id);

    let retry = gateway
        .submit_repair_activation_baton(1_002, &peer(10), &baton)
        .expect("idempotent retry");
    assert!(matches!(retry, BootstrapAdmissionV1::Accepted(_)));
    assert_eq!(storage.record_count(), 2);
}

#[test]
fn same_baton_id_with_changed_bytes_is_rejected() {
    let (gateway, _, _, _) = harness();
    let capability = report();
    let (_, _original) = admit(&gateway, &capability);
    let mut changed = baton(&capability);
    changed.disclosure_hash = hash(0xaa);
    changed.baton_hash.clear();
    changed.baton_hash = canonical_hash(&changed).expect("rehash");
    assert!(matches!(
        gateway.submit_repair_activation_baton(1_002, &peer(10), &changed),
        Err(GatewayError::CommandReplayed)
    ));
}

#[test]
fn commands_are_phase_generation_and_content_fenced() {
    let (gateway, _, storage, _) = harness();
    let capability = report();
    let (challenge_id, baton) = admit(&gateway, &capability);
    let command = BootstrapCommandV1::BeginActivation {
        command_id: id("command.1"),
        activation_id: baton.activation_id.clone(),
        challenge_id,
        expected_phase: BootstrapPhaseV1::BatonDurable,
    };
    let ack = gateway
        .submit_bootstrap_command(&command)
        .expect("command admitted");
    assert!(ack.durable);
    assert_eq!(storage.record_count(), 3);
    assert_eq!(
        gateway
            .submit_bootstrap_command(&command)
            .expect("identical retry"),
        ack
    );
    assert_eq!(storage.record_count(), 3);

    let changed = BootstrapCommandV1::BeginActivation {
        command_id: id("command.1"),
        activation_id: baton.activation_id,
        challenge_id: id("other.challenge"),
        expected_phase: BootstrapPhaseV1::BatonDurable,
    };
    assert!(matches!(
        gateway.submit_bootstrap_command(&changed),
        Err(GatewayError::CommandReplayed)
    ));
}

#[test]
fn command_deduplication_is_rebuilt_from_the_journal() {
    let (gateway, journal, storage, preflight) = harness();
    let capability = report();
    let (challenge_id, baton) = admit(&gateway, &capability);
    let command = BootstrapCommandV1::BeginActivation {
        command_id: id("command.1"),
        activation_id: baton.activation_id.clone(),
        challenge_id,
        expected_phase: BootstrapPhaseV1::BatonDurable,
    };
    let first = gateway
        .submit_bootstrap_command(&command)
        .expect("first command");
    let restarted = BootstrapGateway::new(
        Arc::clone(&journal) as Arc<dyn ActivationJournalPortV1>,
        Arc::clone(&preflight) as Arc<dyn BootstrapPreflightPortV1>,
        Arc::new(EnrollmentMaterializer {
            storage: Arc::clone(&storage),
        }),
        helper(),
    );
    restarted
        .recover_activation(&baton.activation_id, &peer(10))
        .expect("rebuild session");
    assert_eq!(
        restarted
            .submit_bootstrap_command(&command)
            .expect("durable retry"),
        first
    );
    assert_eq!(storage.record_count(), 3);
}

#[test]
fn rollback_handshake_requires_the_rollback_generation() {
    let (gateway, journal, storage, _) = harness();
    let capability = report();
    let (_, baton) = admit(&gateway, &capability);
    for (from, to) in [
        (
            BootstrapPhaseV1::BatonDurable,
            BootstrapPhaseV1::SlotsVerified,
        ),
        (
            BootstrapPhaseV1::SlotsVerified,
            BootstrapPhaseV1::QuiescingCurrent,
        ),
        (
            BootstrapPhaseV1::QuiescingCurrent,
            BootstrapPhaseV1::CandidateSelected,
        ),
        (
            BootstrapPhaseV1::CandidateSelected,
            BootstrapPhaseV1::RollingBack,
        ),
        (
            BootstrapPhaseV1::RollingBack,
            BootstrapPhaseV1::PreviousSelected,
        ),
        (
            BootstrapPhaseV1::PreviousSelected,
            BootstrapPhaseV1::PreviousRelaunching,
        ),
    ] {
        journal
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: baton.activation_id.clone(),
                expected_ordinal: storage.record_count(),
                expected_phase: from,
                next_phase: to,
            })
            .expect("advance");
    }
    let stale = BootstrapCommandV1::CandidateGenerationReady {
        command_id: id("command.ready.1"),
        activation_id: baton.activation_id.clone(),
        generation: baton.candidate_process_generation,
    };
    assert!(matches!(
        gateway.submit_bootstrap_command(&stale),
        Err(GatewayError::StaleGeneration)
    ));
    let rollback = BootstrapCommandV1::CandidateGenerationReady {
        command_id: id("command.ready.2"),
        activation_id: baton.activation_id,
        generation: baton.rollback_process_generation,
    };
    assert!(gateway.submit_bootstrap_command(&rollback).is_ok());
}

#[test]
fn unsupported_result_is_read_only_by_its_exact_recipient() {
    let (gateway, _, _, preflight) = harness();
    let capability = report();
    let baton = baton(&capability);
    let mut drifted = capability.clone();
    drifted.eligibility = ActivationEligibilityV1::Unsupported;
    drifted.capability_digest.clear();
    drifted.capability_digest = canonical_hash(&drifted).expect("drifted digest");
    *preflight.report.lock().expect("report lock") = drifted;
    gateway
        .begin_bootstrap_challenge(1_000, &peer(10))
        .expect("challenge");
    assert!(matches!(
        gateway
            .submit_repair_activation_baton(1_001, &peer(10), &baton)
            .expect("unsupported receipt"),
        BootstrapAdmissionV1::Unsupported(_)
    ));
    assert!(
        gateway
            .read_bootstrap_result(&ProcessGeneration(10))
            .is_ok()
    );
    assert!(matches!(
        gateway.read_bootstrap_result(&ProcessGeneration(11)),
        Err(GatewayError::RecipientMismatch)
    ));
}

#[test]
fn enrollment_intent_is_durable_before_materialization() {
    let (gateway, journal, _, _) = harness();
    let request = enrollment_request();
    let prepared = gateway
        .prepare_managed_local_enrollment(&request)
        .expect("prepared enrollment");
    assert_eq!(prepared.request_id, request.request_id);
    assert!(
        journal
            .load_enrollment_recovery(&request.request_id)
            .expect("recovery")
            .expect("enrollment")
            .terminal
            .is_some()
    );
}

#[test]
fn capability_queries_reject_unsealed_reports() {
    let (gateway, _, _, preflight) = harness();
    preflight
        .report
        .lock()
        .expect("report lock")
        .capability_digest = hash(0xff);
    assert!(matches!(
        gateway.query_activation_capability(
            &provenance(),
            &enrollment(),
            &bundle("candidate.1", 0x21),
            Some(&bundle("previous.1", 0x24)),
        ),
        Err(GatewayError::Bounded(_))
    ));
}

#[test]
fn focused_verification_is_bound_to_candidate_generation_and_plan() {
    let (gateway, journal, storage, _) = harness();
    let capability = report();
    let (_, baton) = admit(&gateway, &capability);
    for (from, to) in [
        (
            BootstrapPhaseV1::BatonDurable,
            BootstrapPhaseV1::SlotsVerified,
        ),
        (
            BootstrapPhaseV1::SlotsVerified,
            BootstrapPhaseV1::QuiescingCurrent,
        ),
        (
            BootstrapPhaseV1::QuiescingCurrent,
            BootstrapPhaseV1::CandidateSelected,
        ),
        (
            BootstrapPhaseV1::CandidateSelected,
            BootstrapPhaseV1::CandidateLaunching,
        ),
        (
            BootstrapPhaseV1::CandidateLaunching,
            BootstrapPhaseV1::AwaitingCandidateIdentity,
        ),
        (
            BootstrapPhaseV1::AwaitingCandidateIdentity,
            BootstrapPhaseV1::CandidateVerifying,
        ),
    ] {
        journal
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: baton.activation_id.clone(),
                expected_ordinal: storage.record_count(),
                expected_phase: from,
                next_phase: to,
            })
            .expect("advance");
    }
    let wrong_plan = BootstrapCommandV1::FocusedVerificationCompleted {
        command_id: id("command.verify.1"),
        activation_id: baton.activation_id.clone(),
        generation: baton.candidate_process_generation,
        verification_plan_hash: hash(0xee),
        outcome: CapabilityOutcomeV1 {
            outcome_id: id("outcome.1"),
            invocation_id: id("invocation.1"),
            class: CapabilityOutcomeClassV1::Success,
            retry_safe_proof: false,
            payload: serde_json::json!({"passed": true}),
            usage: None,
        },
    };
    assert!(matches!(
        gateway.submit_bootstrap_command(&wrong_plan),
        Err(GatewayError::Bounded(_))
    ));
}
