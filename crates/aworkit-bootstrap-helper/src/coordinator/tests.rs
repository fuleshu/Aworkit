//! Hermetic success, rollback, pre-switch abort, and enrollment scenarios.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aworkit_protocol::{
    BootstrapDeadlinesV1, BootstrapResultKindV1, BuildBundleRefV1, BuildProvenanceV1,
    FocusedVerificationCheckV1, FocusedVerificationPlanV1, ManagedLocalEnrollmentRequestV1,
    RepairArtifactRefV1, focused_verification_plan_hash_v1,
};
use aworkit_protocol::{ProcessGeneration, StableId};

use crate::journal::{
    ActivationJournal, ActivationJournalPortV1, BatonAcceptedV1, BootstrapEffectV1,
    BootstrapJournalMutationV1, BootstrapPhaseAdvanceV1, BootstrapPhaseV1, InMemoryJournalStorage,
    canonical_hash,
};
use crate::profile::{
    ActivationPlanV1, ActiveSelectorObservationV1, PlatformActivationPortV1, ProfileError,
    SelectorMutationKindV1, SelectorMutationReceiptV1,
};
use crate::protocol::{BootstrapEnrollmentPortV1, EnrollmentPlanV1};
use crate::slots::{
    BuildSlotError, BuildSlotVerifyPortV1, ManagedSlotRolesV1, OpenBuildSlotHandleV1,
    SlotDataCompatibilityV1, SlotObservationV1, VerifiedBuildSlotV1, VerifiedStagedBuildV1,
};
use crate::watchdog::{
    ApplicationLaunchWatchdog, HermeticGenerationScriptV1, HermeticPlatformProcessPort,
};

use super::{ActivationControlPortV1, ActivationExecutionV1, ActivationRollbackCoordinator};

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("valid stable id")
}

fn digest(byte: u8) -> String {
    format!("sha256:{}", format!("{byte:02x}").repeat(32))
}

fn slot(name: &str, byte: u8) -> VerifiedBuildSlotV1 {
    let build = digest(byte);
    let root = digest(byte + 1);
    VerifiedBuildSlotV1 {
        build_content_hash: build.clone(),
        manifest_hash: digest(byte + 2),
        root_identity_hash: root.clone(),
        owner_identity_hash: digest(0x90),
        volume_identity_hash: digest(0x91),
        expected_core_entry: "app/core".to_owned(),
        data_compatibility: SlotDataCompatibilityV1::RollbackCompatible,
        handle: OpenBuildSlotHandleV1 {
            handle_id: id(&format!("slot.{name}")),
            build_content_hash: build,
            root_identity_hash: root,
            manifest_hash: digest(byte + 2),
            verification_generation: 1,
        },
    }
}

struct TestSlots {
    slots: HashMap<String, VerifiedBuildSlotV1>,
    materialized: VerifiedBuildSlotV1,
    roles: Mutex<ManagedSlotRolesV1>,
}

impl TestSlots {
    fn new(
        current: &VerifiedBuildSlotV1,
        candidate: &VerifiedBuildSlotV1,
        previous: &VerifiedBuildSlotV1,
    ) -> Self {
        Self {
            slots: [current, candidate, previous]
                .into_iter()
                .map(|value| (value.build_content_hash.clone(), value.clone()))
                .collect(),
            materialized: candidate.clone(),
            roles: Mutex::new(ManagedSlotRolesV1 {
                active: Some(current.build_content_hash.clone()),
                candidate: Some(candidate.build_content_hash.clone()),
                previous_known_good: Some(previous.build_content_hash.clone()),
            }),
        }
    }
}

impl BuildSlotVerifyPortV1 for TestSlots {
    fn verify_staged_artifact(
        &self,
        _bundle: &BuildBundleRefV1,
        _provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedStagedBuildV1, BuildSlotError> {
        Err(BuildSlotError::Unsupported("not used by coordinator test"))
    }

    fn materialize_immutable_slot(
        &self,
        _bundle: &BuildBundleRefV1,
        _provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        Ok(self.materialized.clone())
    }

    fn open_verified_slot(
        &self,
        build_content_hash: &str,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        self.slots
            .get(build_content_hash)
            .cloned()
            .ok_or(BuildSlotError::NotFound)
    }

    fn reverify_opened_slot(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        let slot = self.open_verified_slot(&handle.build_content_hash)?;
        if slot.handle == *handle {
            Ok(slot)
        } else {
            Err(BuildSlotError::IdentityChanged)
        }
    }

    fn stage_candidate(&self, _candidate: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError> {
        Ok(())
    }

    fn set_initial_active(&self, active: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError> {
        let mut roles = self.roles.lock().expect("roles lock");
        roles.active = Some(active.build_content_hash.clone());
        roles.candidate = None;
        roles.previous_known_good = None;
        Ok(())
    }

    fn mark_candidate_activated_verified(&self) -> Result<(), BuildSlotError> {
        let mut roles = self.roles.lock().expect("roles lock");
        roles.previous_known_good = roles.active.clone();
        roles.active = roles.candidate.take();
        Ok(())
    }

    fn roles(&self) -> ManagedSlotRolesV1 {
        self.roles.lock().expect("roles lock").clone()
    }

    fn produce_slot_observation(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<SlotObservationV1, BuildSlotError> {
        let slot = self.reverify_opened_slot(handle)?;
        let mut observation = SlotObservationV1 {
            build_content_hash: slot.build_content_hash,
            manifest_hash: slot.manifest_hash,
            root_identity_hash: slot.root_identity_hash,
            verification_generation: handle.verification_generation,
            observation_hash: String::new(),
        };
        observation.observation_hash = canonical_hash(&(
            &observation.build_content_hash,
            &observation.manifest_hash,
            &observation.root_identity_hash,
            observation.verification_generation,
        ))
        .map_err(|_| BuildSlotError::Integrity("observation".to_owned()))?;
        Ok(observation)
    }
}

struct TestSelector {
    current: Mutex<ActiveSelectorObservationV1>,
}

impl TestSelector {
    fn new(plan: &ActivationPlanV1) -> Self {
        Self {
            current: Mutex::new(selector_observation(plan, &plan.current)),
        }
    }

    fn select_for_test(&self, plan: &ActivationPlanV1, slot: &VerifiedBuildSlotV1) {
        *self.current.lock().expect("selector lock") = selector_observation(plan, slot);
    }

    fn receipt(
        &self,
        plan: &ActivationPlanV1,
        kind: SelectorMutationKindV1,
        source: &VerifiedBuildSlotV1,
        target: &VerifiedBuildSlotV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError> {
        let mut current = self.current.lock().expect("selector lock");
        if current.selected_build_content_hash != source.build_content_hash {
            return Err(ProfileError::SelectorDrift);
        }
        let before = current.clone();
        let after = selector_observation(plan, target);
        *current = after.clone();
        let mut receipt = SelectorMutationReceiptV1 {
            mutation_id: id(match kind {
                SelectorMutationKindV1::SelectCandidate => "mutation.candidate",
                SelectorMutationKindV1::RestorePrevious => "mutation.previous",
            }),
            activation_id: plan.activation_id.clone(),
            kind,
            before,
            after,
            mutation_hash: digest(0xa0),
            receipt_hash: String::new(),
        };
        receipt.receipt_hash =
            canonical_hash(&receipt).map_err(|_| ProfileError::Invalid("receipt hash"))?;
        Ok(receipt)
    }
}

impl PlatformActivationPortV1 for TestSelector {
    fn observe_active_selector(
        &self,
        _plan: &ActivationPlanV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError> {
        Ok(self.current.lock().expect("selector lock").clone())
    }

    fn apply_candidate_selector(
        &self,
        plan: &ActivationPlanV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError> {
        if expected_phase != BootstrapPhaseV1::QuiescingCurrent {
            return Err(ProfileError::Invalid("candidate phase"));
        }
        self.receipt(
            plan,
            SelectorMutationKindV1::SelectCandidate,
            &plan.current,
            &plan.candidate,
        )
    }

    fn restore_previous_selector(
        &self,
        plan: &ActivationPlanV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError> {
        if expected_phase != BootstrapPhaseV1::RollingBack {
            return Err(ProfileError::Invalid("rollback phase"));
        }
        self.receipt(
            plan,
            SelectorMutationKindV1::RestorePrevious,
            &plan.candidate,
            &plan.previous,
        )
    }

    fn verify_selector(
        &self,
        plan: &ActivationPlanV1,
        expected: &OpenBuildSlotHandleV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError> {
        let current = self.observe_active_selector(plan)?;
        if current.selected_build_content_hash == expected.build_content_hash
            && current.selected_root_identity_hash == expected.root_identity_hash
        {
            Ok(current)
        } else {
            Err(ProfileError::SelectorDrift)
        }
    }
}

fn selector_observation(
    plan: &ActivationPlanV1,
    slot: &VerifiedBuildSlotV1,
) -> ActiveSelectorObservationV1 {
    let mut observation = ActiveSelectorObservationV1 {
        selector_identity_hash: plan.selector_identity_hash.clone(),
        selected_build_content_hash: slot.build_content_hash.clone(),
        selected_root_identity_hash: slot.root_identity_hash.clone(),
        capability_generation: plan.capability_generation,
        observation_hash: String::new(),
    };
    observation.observation_hash = canonical_hash(&observation).expect("observation hash");
    observation
}

fn deadlines() -> BootstrapDeadlinesV1 {
    BootstrapDeadlinesV1 {
        admission_ms: 100,
        cleanup_ms: 100,
        startup_ms: 100,
        focused_verification_ms: 100,
        rollback_ms: 100,
        result_read_ms: 100,
    }
}

fn setup(
    candidate_script: HermeticGenerationScriptV1,
    current_script: HermeticGenerationScriptV1,
    previous_script: HermeticGenerationScriptV1,
) -> (
    Arc<ActivationRollbackCoordinator>,
    Arc<ActivationJournal>,
    Arc<TestSlots>,
    Arc<TestSelector>,
    ActivationExecutionV1,
) {
    let current = slot("current", 0x10);
    let candidate = slot("candidate", 0x20);
    let previous = slot("previous", 0x30);
    let plan = ActivationPlanV1 {
        activation_id: id("activation.11.6"),
        capability_generation: 7,
        capability_digest: digest(0x70),
        selector_identity_hash: digest(0x71),
        current: current.clone(),
        candidate: candidate.clone(),
        previous: previous.clone(),
        current_process_generation: ProcessGeneration(1),
        candidate_process_generation: ProcessGeneration(2),
        rollback_process_generation: ProcessGeneration(3),
        deadlines: deadlines(),
    };
    let mut verification_plan = FocusedVerificationPlanV1 {
        plan_id: id("verification.plan"),
        checks: vec![FocusedVerificationCheckV1 {
            check_id: id("verification.check.smoke"),
            label: "Focused smoke check".to_owned(),
            capability_id: id("capability.focused-smoke"),
            timeout_ms: 100,
        }],
        plan_hash: String::new(),
    };
    verification_plan.plan_hash =
        focused_verification_plan_hash_v1(&verification_plan).expect("plan hash");
    let execution = ActivationExecutionV1 {
        plan: plan.clone(),
        installation_id: id("installation.1"),
        management_checkpoint_id: id("checkpoint.1"),
        verification_plan: verification_plan.clone(),
        helper_detached_and_surviving: true,
        sealed_at_epoch_ms: 10,
    };
    let slots = Arc::new(TestSlots::new(&current, &candidate, &previous));
    let selector = Arc::new(TestSelector::new(&plan));
    let process = Arc::new(HermeticPlatformProcessPort::default());
    process.script(ProcessGeneration(1), current_script, true);
    process.script(ProcessGeneration(2), candidate_script, false);
    process.script(ProcessGeneration(3), previous_script, false);
    let watchdog = Arc::new(ApplicationLaunchWatchdog::new(process, slots.clone(), 1));
    let journal = Arc::new(ActivationJournal::new(Arc::new(
        InMemoryJournalStorage::default(),
    )));
    journal.acquire_single_flight().expect("journal lock");
    let baton = BatonAcceptedV1 {
        activation_id: plan.activation_id.clone(),
        baton_id: id("baton.1"),
        baton_hash: digest(0x80),
        command_hash: digest(0x81),
        challenge_id: id("challenge.1"),
        challenge_hash: digest(0x82),
        peer_executable_hash: digest(0x83),
        peer_os_identity_hash: digest(0x84),
        admission_id: id("admission.1"),
        admission_hash: digest(0x85),
        management_checkpoint_id: execution.management_checkpoint_id.clone(),
        profile_version: 1,
        provenance_digest: digest(0x86),
        enrollment_digest: digest(0x87),
        capability_generation: plan.capability_generation,
        capability_digest: plan.capability_digest.clone(),
        candidate_slot_hash: candidate.build_content_hash,
        previous_slot_hash: previous.build_content_hash,
        verification_plan_hash: verification_plan.plan_hash,
        current_process_generation: plan.current_process_generation,
        candidate_process_generation: plan.candidate_process_generation,
        rollback_process_generation: plan.rollback_process_generation,
        deadlines: plan.deadlines.clone(),
    };
    journal
        .append_baton_accepted(&baton)
        .expect("baton durable");
    journal
        .advance_phase(&BootstrapPhaseAdvanceV1 {
            activation_id: plan.activation_id.clone(),
            expected_ordinal: 1,
            expected_phase: BootstrapPhaseV1::AdmittingBaton,
            next_phase: BootstrapPhaseV1::BatonDurable,
        })
        .expect("baton phase");
    let coordinator = Arc::new(ActivationRollbackCoordinator::new(
        journal.clone(),
        slots.clone(),
        selector.clone(),
        watchdog,
    ));
    (coordinator, journal, slots, selector, execution)
}

#[test]
fn candidate_is_activated_only_after_focused_verification_and_durable_result() {
    let (coordinator, journal, slots, _, execution) = setup(
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
    );
    let receipt = coordinator
        .execute_activation(&execution)
        .expect("verified activation");
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::ActivatedVerified { .. }
    ));
    assert_eq!(
        receipt.receipt_hash,
        aworkit_protocol::bootstrap_result_hash_v1(&receipt).expect("shared receipt hash")
    );
    let BootstrapResultKindV1::ActivatedVerified {
        focused_verification,
    } = &receipt.result
    else {
        unreachable!()
    };
    assert_eq!(
        focused_verification.results.len(),
        execution.verification_plan.checks.len()
    );
    assert_eq!(receipt.recipient_process_generation, ProcessGeneration(2));
    assert_eq!(
        slots.roles().active,
        Some(execution.plan.candidate.build_content_hash)
    );
    assert_eq!(
        journal
            .load_activation_recovery(&execution.plan.activation_id)
            .expect("recovery")
            .expect("state")
            .phase,
        BootstrapPhaseV1::ResultAvailable
    );
}

#[test]
fn failed_candidate_verification_restores_and_relaunches_previous() {
    let candidate = HermeticGenerationScriptV1 {
        verification_passed: false,
        ..HermeticGenerationScriptV1::default()
    };
    let (coordinator, _, _, _, execution) = setup(
        candidate,
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
    );
    let receipt = coordinator
        .execute_activation(&execution)
        .expect("automatic rollback");
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::RolledBack { .. }
    ));
    let BootstrapResultKindV1::RolledBack {
        rollback_evidence, ..
    } = &receipt.result
    else {
        unreachable!()
    };
    assert!(!rollback_evidence.is_empty());
    assert_eq!(receipt.recipient_process_generation, ProcessGeneration(3));
}

#[test]
fn unproven_current_cleanup_aborts_without_switching() {
    let current = HermeticGenerationScriptV1 {
        orphan_risk: true,
        ..HermeticGenerationScriptV1::default()
    };
    let (coordinator, _, _, _, execution) = setup(
        HermeticGenerationScriptV1::default(),
        current,
        HermeticGenerationScriptV1::default(),
    );
    let receipt = coordinator
        .execute_activation(&execution)
        .expect("protected pre-switch result");
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::Unsupported { .. }
    ));
    assert_eq!(receipt.recipient_process_generation, ProcessGeneration(1));
}

#[test]
fn failed_previous_relaunch_seals_manual_recovery_notice_and_result() {
    let candidate = HermeticGenerationScriptV1 {
        verification_passed: false,
        ..HermeticGenerationScriptV1::default()
    };
    let previous = HermeticGenerationScriptV1 {
        healthy: false,
        ..HermeticGenerationScriptV1::default()
    };
    let (coordinator, journal, _, _, execution) =
        setup(candidate, HermeticGenerationScriptV1::default(), previous);
    let receipt = coordinator
        .execute_activation(&execution)
        .expect("sealed manual recovery");
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::ManualRecoveryRequired { .. }
    ));
    let recovered = journal
        .load_activation_recovery(&execution.plan.activation_id)
        .expect("recovery")
        .expect("state");
    assert!(recovered.manual_recovery.is_some());
    assert_eq!(recovered.phase, BootstrapPhaseV1::ResultAvailable);
}

#[test]
fn recovery_with_current_selector_is_rollback_biased_to_pre_switch_abort() {
    let (coordinator, _, _, _, execution) = setup(
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
    );
    let receipt = coordinator
        .recover_activation(&execution.plan.activation_id, Some(&execution))
        .expect("recovered terminal receipt");
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::Unsupported { .. }
    ));
    assert_eq!(receipt.recipient_process_generation, ProcessGeneration(1));
}

fn advance_for_crash(
    journal: &ActivationJournal,
    activation_id: &StableId,
    next: BootstrapPhaseV1,
) {
    let state = journal
        .load_activation_recovery(activation_id)
        .expect("recovery")
        .expect("state");
    journal
        .advance_phase(&BootstrapPhaseAdvanceV1 {
            activation_id: activation_id.clone(),
            expected_ordinal: state.head_ordinal + 1,
            expected_phase: state.phase,
            next_phase: next,
        })
        .expect("crash boundary phase");
}

#[test]
fn every_post_switch_crash_phase_recovers_by_relaunching_previous() {
    let cases: &[(BootstrapPhaseV1, &[BootstrapPhaseV1])] = &[
        (
            BootstrapPhaseV1::CandidateSelected,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
            ],
        ),
        (
            BootstrapPhaseV1::CandidateLaunching,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::CandidateLaunching,
            ],
        ),
        (
            BootstrapPhaseV1::AwaitingCandidateIdentity,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::CandidateLaunching,
                BootstrapPhaseV1::AwaitingCandidateIdentity,
            ],
        ),
        (
            BootstrapPhaseV1::CandidateVerifying,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::CandidateLaunching,
                BootstrapPhaseV1::AwaitingCandidateIdentity,
                BootstrapPhaseV1::CandidateVerifying,
            ],
        ),
        (
            BootstrapPhaseV1::Verified,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::CandidateLaunching,
                BootstrapPhaseV1::AwaitingCandidateIdentity,
                BootstrapPhaseV1::CandidateVerifying,
                BootstrapPhaseV1::Verified,
            ],
        ),
        (
            BootstrapPhaseV1::RollingBack,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::RollingBack,
            ],
        ),
        (
            BootstrapPhaseV1::PreviousSelected,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::RollingBack,
                BootstrapPhaseV1::PreviousSelected,
            ],
        ),
        (
            BootstrapPhaseV1::PreviousRelaunching,
            &[
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::RollingBack,
                BootstrapPhaseV1::PreviousSelected,
                BootstrapPhaseV1::PreviousRelaunching,
            ],
        ),
    ];
    for (target, path) in cases {
        let (coordinator, journal, _, selector, execution) = setup(
            HermeticGenerationScriptV1::default(),
            HermeticGenerationScriptV1::default(),
            HermeticGenerationScriptV1::default(),
        );
        for phase in *path {
            advance_for_crash(&journal, &execution.plan.activation_id, *phase);
        }
        let selected = if matches!(
            target,
            BootstrapPhaseV1::PreviousSelected | BootstrapPhaseV1::PreviousRelaunching
        ) {
            &execution.plan.previous
        } else {
            &execution.plan.candidate
        };
        selector.select_for_test(&execution.plan, selected);
        let receipt = coordinator
            .recover_activation(&execution.plan.activation_id, Some(&execution))
            .unwrap_or_else(|error| panic!("{target:?} recovery failed: {error}"));
        assert!(
            matches!(receipt.result, BootstrapResultKindV1::RolledBack { .. }),
            "{target:?}"
        );
    }
}

#[test]
fn recovery_reconciles_an_open_cleanup_intent_before_changing_phase() {
    let (coordinator, journal, _, _, execution) = setup(
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
    );
    advance_for_crash(
        &journal,
        &execution.plan.activation_id,
        BootstrapPhaseV1::SlotsVerified,
    );
    advance_for_crash(
        &journal,
        &execution.plan.activation_id,
        BootstrapPhaseV1::QuiescingCurrent,
    );
    let state = journal
        .load_activation_recovery(&execution.plan.activation_id)
        .expect("recovery")
        .expect("state");
    journal
        .append_effect_intent(&BootstrapJournalMutationV1 {
            activation_id: execution.plan.activation_id.clone(),
            expected_ordinal: state.head_ordinal + 1,
            expected_phase: state.phase,
            effect: BootstrapEffectV1 {
                current_slot_hash: execution.plan.current.build_content_hash.clone(),
                target_slot_hash: execution.plan.candidate.build_content_hash.clone(),
                capability_generation: execution.plan.capability_generation,
                process_generation: execution.plan.current_process_generation,
                observation_hash: String::new(),
            },
        })
        .expect("durable interrupted cleanup intent");
    let receipt = coordinator
        .recover_activation(&execution.plan.activation_id, Some(&execution))
        .expect("reconciled open intent");
    assert!(matches!(
        receipt.result,
        BootstrapResultKindV1::Unsupported { .. }
    ));
}

#[test]
fn enrollment_materializes_but_does_not_launch_or_activate_candidate_flow() {
    let (coordinator, _, slots, _, execution) = setup(
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
        HermeticGenerationScriptV1::default(),
    );
    let artifact = RepairArtifactRefV1 {
        artifact_id: id("artifact.1"),
        content_hash: digest(0x40),
        byte_size: 100,
        media_type: "application/octet-stream".to_owned(),
        logical_name: "bundle".to_owned(),
    };
    let request = ManagedLocalEnrollmentRequestV1 {
        request_id: id("enrollment.1"),
        explicit_user_decision_id: id("decision.1"),
        group_id: id("group.1"),
        candidate_id: id("candidate.1"),
        candidate_version: 1,
        candidate_hash: execution.plan.candidate.build_content_hash.clone(),
        projected_provenance_hash: digest(0x41),
        whole_bundle: BuildBundleRefV1 {
            artifact,
            manifest_relative_entry: "manifest.json".to_owned(),
        },
        capability_report_id: id("report.1"),
        capability_digest: digest(0x42),
    };
    let mut plan = EnrollmentPlanV1 {
        plan_id: id("enrollment.plan"),
        installation_id: id("installation.1"),
        profile_version: 1,
        helper_root_identity_hash: digest(0x43),
        initial_active_slot_root_hash: execution.plan.candidate.root_identity_hash.clone(),
        selector_identity_hash: digest(0x44),
        journal_identity_hash: digest(0x45),
        plan_hash: String::new(),
    };
    plan.plan_hash = canonical_hash(&plan).expect("plan hash");
    let prepared = coordinator
        .materialize(&request, &plan)
        .expect("nonactivating enrollment");
    assert!(prepared.observation.published_slot_verified);
    assert_eq!(slots.roles().active, Some(request.candidate_hash));
    assert!(slots.roles().candidate.is_none());
}
