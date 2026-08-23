//! Hermetic launch, cleanup, identity, deadline, and verification tests.

use std::collections::HashMap;
use std::sync::Arc;

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    BootstrapDeadlinesV1, FocusedVerificationCheckV1, FocusedVerificationPlanV1,
    ManualRecoveryNoticeV1, ReasonCodeV1, focused_verification_plan_hash_v1,
};

use crate::journal::canonical_hash;
use crate::profile::ActiveSelectorObservationV1;
use crate::slots::{
    BuildSlotError, BuildSlotVerifyPortV1, ManagedSlotRolesV1, OpenBuildSlotHandleV1,
    SlotDataCompatibilityV1, SlotObservationV1, VerifiedBuildSlotV1, VerifiedStagedBuildV1,
};

use super::*;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("valid id")
}

fn hash(byte: u8) -> String {
    format!("sha256:{}", format!("{byte:02x}").repeat(32))
}

fn slot() -> VerifiedBuildSlotV1 {
    VerifiedBuildSlotV1 {
        build_content_hash: hash(0x10),
        manifest_hash: hash(0x11),
        root_identity_hash: hash(0x12),
        owner_identity_hash: hash(0x13),
        volume_identity_hash: hash(0x14),
        expected_core_entry: "bin/aworkit-core".to_owned(),
        data_compatibility: SlotDataCompatibilityV1::RollbackCompatible,
        handle: OpenBuildSlotHandleV1 {
            handle_id: id("slot.handle.1"),
            build_content_hash: hash(0x10),
            root_identity_hash: hash(0x12),
            manifest_hash: hash(0x11),
            verification_generation: 1,
        },
    }
}

struct StaticSlots(HashMap<StableId, VerifiedBuildSlotV1>);

impl BuildSlotVerifyPortV1 for StaticSlots {
    fn verify_staged_artifact(
        &self,
        _bundle: &aworkit_trusted_core::BuildBundleRefV1,
        _provenance: &aworkit_trusted_core::BuildProvenanceV1,
    ) -> Result<VerifiedStagedBuildV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }
    fn materialize_immutable_slot(
        &self,
        _bundle: &aworkit_trusted_core::BuildBundleRefV1,
        _provenance: &aworkit_trusted_core::BuildProvenanceV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }
    fn open_verified_slot(&self, _hash: &str) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }
    fn reverify_opened_slot(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        self.0
            .get(&handle.handle_id)
            .filter(|value| value.handle == *handle)
            .cloned()
            .ok_or(BuildSlotError::IdentityChanged)
    }
    fn stage_candidate(&self, _candidate: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError> {
        Ok(())
    }
    fn set_initial_active(&self, _active: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError> {
        Ok(())
    }
    fn mark_candidate_activated_verified(&self) -> Result<(), BuildSlotError> {
        Ok(())
    }
    fn roles(&self) -> ManagedSlotRolesV1 {
        ManagedSlotRolesV1::default()
    }
    fn produce_slot_observation(
        &self,
        _handle: &OpenBuildSlotHandleV1,
    ) -> Result<SlotObservationV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }
}

fn deadlines() -> BootstrapDeadlinesV1 {
    BootstrapDeadlinesV1 {
        admission_ms: 1_000,
        cleanup_ms: 1_000,
        startup_ms: 1_000,
        focused_verification_ms: 1_000,
        rollback_ms: 1_000,
        result_read_ms: 1_000,
    }
}

fn cleanup(generation: u64) -> ProcessTreeCleanupV1 {
    let mut proof = ProcessTreeCleanupV1 {
        process_generation: ProcessGeneration(generation),
        cooperative_requested: true,
        forced_termination_used: false,
        descendants_observed: 3,
        tree_empty: true,
        orphan_risk: false,
        proof_hash: String::new(),
    };
    proof.proof_hash = canonical_hash(&proof).expect("cleanup hash");
    proof
}

fn spec(role: GenerationRoleV1) -> GenerationLaunchSpecV1 {
    let generation = if role == GenerationRoleV1::Candidate {
        11
    } else {
        12
    };
    let prior = if role == GenerationRoleV1::Candidate {
        10
    } else {
        11
    };
    let slot = slot();
    let mut selector = ActiveSelectorObservationV1 {
        selector_identity_hash: hash(0x20),
        selected_build_content_hash: slot.build_content_hash.clone(),
        selected_root_identity_hash: slot.root_identity_hash.clone(),
        capability_generation: 3,
        observation_hash: String::new(),
    };
    selector.observation_hash = canonical_hash(&selector).expect("selector hash");
    let mut verification_plan = FocusedVerificationPlanV1 {
        plan_id: id("verification.plan.1"),
        checks: vec![FocusedVerificationCheckV1 {
            check_id: id("verification.check.1"),
            label: "focused smoke check".to_owned(),
            capability_id: id("capability.smoke"),
            timeout_ms: 500,
        }],
        plan_hash: String::new(),
    };
    verification_plan.plan_hash =
        focused_verification_plan_hash_v1(&verification_plan).expect("verification plan hash");
    GenerationLaunchSpecV1 {
        activation_id: id("activation.1"),
        attempt_id: id(if role == GenerationRoleV1::Candidate {
            "attempt.candidate"
        } else {
            "attempt.previous"
        }),
        role,
        installation_id: id("installation.1"),
        enrollment_digest: hash(0x21),
        capability_generation: 3,
        capability_digest: hash(0x22),
        verification_plan_hash: verification_plan.plan_hash.clone(),
        verification_plan,
        process_generation: ProcessGeneration(generation),
        expected_prior_process_generation: ProcessGeneration(prior),
        slot,
        selector,
        prior_cleanup: cleanup(prior),
        helper_detached_and_surviving: true,
        deadlines: deadlines(),
    }
}

fn harness() -> (ApplicationLaunchWatchdog, Arc<HermeticPlatformProcessPort>) {
    let process = Arc::new(HermeticPlatformProcessPort::default());
    let slot = slot();
    let slots = Arc::new(StaticSlots(HashMap::from([(
        slot.handle.handle_id.clone(),
        slot,
    )])));
    (
        ApplicationLaunchWatchdog::new(
            Arc::clone(&process) as Arc<dyn PlatformProcessPortV1>,
            slots as Arc<dyn BuildSlotVerifyPortV1>,
            1,
        ),
        process,
    )
}

#[test]
fn cleanup_is_cooperative_then_forced_then_proven_empty() {
    let (watchdog, process) = harness();
    let script = HermeticGenerationScriptV1 {
        cooperative_exit: false,
        ..HermeticGenerationScriptV1::default()
    };
    process.script(ProcessGeneration(10), script, true);
    let proof = watchdog
        .cleanup_generation(
            &id("activation.1"),
            &id("cleanup.1"),
            ProcessGeneration(10),
            &deadlines(),
            false,
        )
        .expect("cleanup");
    assert!(proof.tree_empty);
    assert!(proof.forced_termination_used);
    assert_eq!(
        process.events(),
        ["shutdown:10", "await_exit:10", "force:10", "prove_empty:10"]
    );
}

#[test]
fn successful_candidate_requires_handshake_health_and_focused_verification() {
    let (watchdog, process) = harness();
    process.script(
        ProcessGeneration(11),
        HermeticGenerationScriptV1::default(),
        false,
    );
    assert!(matches!(
        watchdog.launch_and_watch(&spec(GenerationRoleV1::Candidate)),
        Ok(GenerationWatchdogSuccessV1::CandidateVerified { .. })
    ));
    assert_eq!(
        process.events(),
        [
            "spawn:11",
            "await_handshake:11",
            "health:11",
            "verification_handoff:11",
            "await_verification:11"
        ]
    );
}

#[test]
fn readiness_and_health_do_not_substitute_for_verification() {
    let (watchdog, process) = harness();
    process.script(
        ProcessGeneration(11),
        HermeticGenerationScriptV1 {
            verification_passed: false,
            ..HermeticGenerationScriptV1::default()
        },
        false,
    );
    let failure = watchdog
        .launch_and_watch(&spec(GenerationRoleV1::Candidate))
        .expect_err("verification must fail");
    assert_eq!(failure.stage, WatchdogFailureStageV1::FocusedVerification);
    assert!(failure.rollback_required);
    assert!(
        process
            .events()
            .iter()
            .any(|event| event == "prove_empty:11")
    );
}

#[test]
fn identity_mismatch_and_startup_timeout_are_rollback_biased() {
    for script in [
        HermeticGenerationScriptV1 {
            handshake_identity_mismatch: true,
            ..HermeticGenerationScriptV1::default()
        },
        HermeticGenerationScriptV1 {
            handshake_available: false,
            ..HermeticGenerationScriptV1::default()
        },
    ] {
        let (watchdog, process) = harness();
        process.script(ProcessGeneration(11), script, false);
        let failure = watchdog
            .launch_and_watch(&spec(GenerationRoleV1::Candidate))
            .expect_err("launch must fail");
        assert!(matches!(
            failure.stage,
            WatchdogFailureStageV1::Identity | WatchdogFailureStageV1::Startup
        ));
        assert!(failure.rollback_required);
    }
}

#[test]
fn rollback_launch_uses_fresh_generation_and_skips_candidate_verification() {
    let (watchdog, process) = harness();
    process.script(
        ProcessGeneration(12),
        HermeticGenerationScriptV1::default(),
        false,
    );
    assert!(matches!(
        watchdog.launch_and_watch(&spec(GenerationRoleV1::Previous)),
        Ok(GenerationWatchdogSuccessV1::PreviousHealthy { .. })
    ));
    assert!(
        !process
            .events()
            .iter()
            .any(|event| event.contains("verification"))
    );
}

#[test]
fn ambiguous_cleanup_or_attached_helper_blocks_launch() {
    let (watchdog, _) = harness();
    let mut ambiguous = spec(GenerationRoleV1::Candidate);
    ambiguous.prior_cleanup.orphan_risk = true;
    ambiguous.prior_cleanup.proof_hash.clear();
    ambiguous.prior_cleanup.proof_hash =
        canonical_hash(&ambiguous.prior_cleanup).expect("proof hash");
    assert_eq!(
        watchdog
            .launch_and_watch(&ambiguous)
            .expect_err("orphan risk")
            .stage,
        WatchdogFailureStageV1::Preconditions
    );

    let mut attached = spec(GenerationRoleV1::Candidate);
    attached.helper_detached_and_surviving = false;
    assert!(watchdog.launch_and_watch(&attached).is_err());
}

#[test]
fn stable_launcher_notice_has_only_bounded_recovery_actions() {
    let (watchdog, _) = harness();
    let notice = ManualRecoveryNoticeV1 {
        notice_id: id("notice.1"),
        activation_id: id("activation.1"),
        reason: ReasonCodeV1::RollbackFailure,
        observed_slot_state_hash: hash(0x40),
        diagnostic_id: id("diagnostic.1"),
        instructions: vec!["open the helper-controlled recovery folder".to_owned()],
    };
    let rendered = watchdog.stable_launcher_notice(&notice);
    assert!(rendered.copy_diagnostic_id_allowed);
    assert!(rendered.open_recovery_instructions_allowed);
    assert!(rendered.exits_after_notice);
    assert_eq!(rendered.notice, notice);
}
