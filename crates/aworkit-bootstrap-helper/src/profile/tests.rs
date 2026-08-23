//! Hermetic capability-matrix and selector-fencing tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    ActivationEligibilityV1, BootstrapDeadlinesV1, BuildBundleRefV1, BuildOriginV1,
    BuildProvenanceV1, ManagedLocalEnrollmentRequestV1, RepairArtifactRefV1,
};

use crate::journal::{BootstrapPhaseV1, canonical_hash};
use crate::protocol::{
    BootstrapPreflightPortV1, LocalBuildEnrollmentStateV1, LocalBuildEnrollmentV1, OwnershipFactsV1,
};
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

fn bundle(name: &str, byte: u8) -> BuildBundleRefV1 {
    BuildBundleRefV1 {
        artifact: RepairArtifactRefV1 {
            artifact_id: id(name),
            content_hash: hash(byte),
            byte_size: 4096,
            media_type: "application/vnd.aworkit.bundle-v1".to_owned(),
            logical_name: format!("{name}.bundle"),
        },
        manifest_relative_entry: "SlotManifest.json".to_owned(),
    }
}

fn provenance() -> BuildProvenanceV1 {
    BuildProvenanceV1 {
        source_revision: "abc123".to_owned(),
        source_tree_hash: hash(0x10),
        workspace_identity_hash: hash(0x11),
        toolchain_hash: hash(0x12),
        build_manifest_hash: hash(0x13),
        provenance_hash: hash(0x14),
    }
}

fn layout() -> ManagedLocalLayoutV1 {
    ManagedLocalLayoutV1 {
        installation_id: id("installation.1"),
        helper_root_identity_hash: hash(0x20),
        helper_identity_hash: hash(0x21),
        launcher_identity_hash: hash(0x22),
        initial_active_slot_root_hash: hash(0x23),
        selector_identity_hash: hash(0x24),
        journal_identity_hash: hash(0x25),
    }
}

fn enrollment(state: LocalBuildEnrollmentStateV1) -> LocalBuildEnrollmentV1 {
    LocalBuildEnrollmentV1 {
        installation_id: id("installation.1"),
        profile_version: 1,
        enrollment_digest: hash(0x30),
        active_slot_hash: hash(0x31),
        current_bundle_hash: hash(0x32),
        selector_identity_hash: hash(0x24),
        helper_identity_hash: hash(0x21),
        launcher_identity_hash: hash(0x22),
        journal_identity_hash: hash(0x25),
        ownership: OwnershipFactsV1 {
            per_user_owned: true,
            same_volume: true,
            selector_atomic: true,
            helper_survives_outside_slots: true,
        },
        state,
    }
}

fn observations(origin: BuildOriginV1) -> ProfileRuntimeObservationsV1 {
    ProfileRuntimeObservationsV1 {
        detected_origin: origin,
        embedded_provenance_digest: hash(0x14),
        candidate_id: id("candidate.1"),
        candidate_version: 7,
        candidate_build_content_hash: hash(0x33),
        current_build: bundle("current.1", 0x32),
        active_selector_hash: hash(0x31),
        installation_identity_matches: true,
        helper_identity_matches: true,
        launcher_identity_matches: true,
        journal_identity_matches: true,
        selector_identity_matches: true,
        candidate_slot_verified: true,
        previous_slot_verified: true,
        per_user_owned: true,
        writable_without_elevation: true,
        same_local_durable_volume: true,
        atomic_selector_supported: true,
        helper_survives_outside_slots: true,
        complete_process_tree_cleanup: true,
        verification_only_launch: true,
        data_compatibility: SlotDataCompatibilityV1::RollbackCompatible,
        capability_generation: 3,
        valid_from_epoch_ms: 100,
        expires_at_epoch_ms: 10_000,
    }
}

struct Observations(Mutex<ProfileRuntimeObservationsV1>);

impl ProfileObservationPortV1 for Observations {
    fn observe(&self) -> Result<ProfileRuntimeObservationsV1, ProfileError> {
        Ok(self.0.lock().expect("observation lock").clone())
    }
}

fn adapter(origin: BuildOriginV1) -> (ManagedLocalBuildProfileAdapter, Arc<Observations>) {
    let observations = Arc::new(Observations(Mutex::new(observations(origin))));
    (
        ManagedLocalBuildProfileAdapter::new(
            Arc::clone(&observations) as Arc<dyn ProfileObservationPortV1>,
            layout(),
        ),
        observations,
    )
}

#[test]
fn source_checkout_requires_explicit_enrollment() {
    let (adapter, _) = adapter(BuildOriginV1::SourceCheckout {
        projected_provenance_hash: hash(0x14),
    });
    let report = adapter
        .capability_report(
            &provenance(),
            &enrollment(LocalBuildEnrollmentStateV1::NotEnrolled),
            &bundle("candidate.1", 0x40),
            Some(&bundle("previous.1", 0x41)),
        )
        .expect("report");
    assert_eq!(
        report.eligibility,
        ActivationEligibilityV1::EnrollmentRequired
    );
    assert_eq!(report.reason.code, "enrollment_required");
}

#[test]
fn packaged_and_unknown_origins_are_report_only() {
    for origin in [
        BuildOriginV1::PackagedDistribution {
            owner: "package-manager".to_owned(),
        },
        BuildOriginV1::Unknown,
        BuildOriginV1::Conflicting {
            detail: "two roots".to_owned(),
        },
    ] {
        let (adapter, _) = adapter(origin);
        let report = adapter
            .capability_report(
                &provenance(),
                &enrollment(LocalBuildEnrollmentStateV1::Enrolled),
                &bundle("candidate.1", 0x40),
                Some(&bundle("previous.1", 0x41)),
            )
            .expect("report");
        assert_ne!(
            report.eligibility,
            ActivationEligibilityV1::SupportedManagedLocal
        );
    }
}

#[test]
fn managed_local_report_is_deterministic_and_generation_bound() {
    let (adapter, _) = adapter(BuildOriginV1::ManagedLocal {
        enrollment_digest: hash(0x30),
        active_slot_hash: hash(0x31),
    });
    let args = (
        provenance(),
        enrollment(LocalBuildEnrollmentStateV1::Enrolled),
        bundle("candidate.1", 0x40),
        bundle("previous.1", 0x41),
    );
    let first = adapter
        .capability_report(&args.0, &args.1, &args.2, Some(&args.3))
        .expect("first");
    let second = adapter
        .capability_report(&args.0, &args.1, &args.2, Some(&args.3))
        .expect("second");
    assert_eq!(first, second);
    assert_eq!(
        first.eligibility,
        ActivationEligibilityV1::SupportedManagedLocal
    );
    let mut unhashed = first.clone();
    unhashed.capability_digest.clear();
    assert_eq!(
        canonical_hash(&unhashed).expect("digest"),
        first.capability_digest
    );
}

#[test]
fn every_lost_runtime_guarantee_downgrades_supported() {
    let (adapter, observations) = adapter(BuildOriginV1::ManagedLocal {
        enrollment_digest: hash(0x30),
        active_slot_hash: hash(0x31),
    });
    let candidate = bundle("candidate.1", 0x40);
    let previous = bundle("previous.1", 0x41);
    for mutate in [
        |value: &mut ProfileRuntimeObservationsV1| value.per_user_owned = false,
        |value: &mut ProfileRuntimeObservationsV1| value.same_local_durable_volume = false,
        |value: &mut ProfileRuntimeObservationsV1| value.atomic_selector_supported = false,
        |value: &mut ProfileRuntimeObservationsV1| value.complete_process_tree_cleanup = false,
        |value: &mut ProfileRuntimeObservationsV1| value.verification_only_launch = false,
    ] {
        let original = observations.0.lock().expect("lock").clone();
        mutate(&mut observations.0.lock().expect("lock"));
        let report = adapter
            .capability_report(
                &provenance(),
                &enrollment(LocalBuildEnrollmentStateV1::Enrolled),
                &candidate,
                Some(&previous),
            )
            .expect("report");
        assert_eq!(report.eligibility, ActivationEligibilityV1::Unsupported);
        *observations.0.lock().expect("lock") = original;
    }
    observations.0.lock().expect("lock").data_compatibility =
        SlotDataCompatibilityV1::ForwardOnlyMigrationRequired;
    assert_eq!(
        adapter
            .capability_report(
                &provenance(),
                &enrollment(LocalBuildEnrollmentStateV1::Enrolled),
                &candidate,
                Some(&previous),
            )
            .expect("report")
            .eligibility,
        ActivationEligibilityV1::Unsupported
    );
}

#[test]
fn enrollment_plan_is_fixed_and_only_for_matching_source_provenance() {
    let (adapter, observations) = adapter(BuildOriginV1::SourceCheckout {
        projected_provenance_hash: hash(0x14),
    });
    let request = ManagedLocalEnrollmentRequestV1 {
        request_id: id("enrollment.1"),
        explicit_user_decision_id: id("decision.1"),
        group_id: id("group.1"),
        candidate_id: id("candidate.1"),
        candidate_version: 7,
        candidate_hash: hash(0x33),
        projected_provenance_hash: hash(0x14),
        whole_bundle: bundle("candidate.1", 0x40),
        capability_report_id: id("report.1"),
        capability_digest: hash(0x50),
    };
    let first = adapter.enrollment_plan(&request).expect("plan");
    let second = adapter.enrollment_plan(&request).expect("same plan");
    assert_eq!(first, second);
    assert_eq!(first.installation_id, layout().installation_id);

    observations
        .0
        .lock()
        .expect("lock")
        .embedded_provenance_digest = hash(0xff);
    assert!(adapter.enrollment_plan(&request).is_err());
}

fn verified_slot(name: &str, byte: u8) -> VerifiedBuildSlotV1 {
    VerifiedBuildSlotV1 {
        build_content_hash: hash(byte),
        manifest_hash: hash(byte.wrapping_add(1)),
        root_identity_hash: hash(byte.wrapping_add(2)),
        owner_identity_hash: hash(0xa0),
        volume_identity_hash: hash(0xa1),
        expected_core_entry: "bin/core".to_owned(),
        data_compatibility: SlotDataCompatibilityV1::RollbackCompatible,
        handle: OpenBuildSlotHandleV1 {
            handle_id: id(name),
            build_content_hash: hash(byte),
            root_identity_hash: hash(byte.wrapping_add(2)),
            manifest_hash: hash(byte.wrapping_add(1)),
            verification_generation: 1,
        },
    }
}

struct StaticSlots {
    slots: HashMap<StableId, VerifiedBuildSlotV1>,
}

impl BuildSlotVerifyPortV1 for StaticSlots {
    fn verify_staged_artifact(
        &self,
        _bundle: &BuildBundleRefV1,
        _provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedStagedBuildV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }

    fn materialize_immutable_slot(
        &self,
        _bundle: &BuildBundleRefV1,
        _provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }

    fn open_verified_slot(
        &self,
        _build_content_hash: &str,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        Err(BuildSlotError::NotFound)
    }

    fn reverify_opened_slot(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        self.slots
            .get(&handle.handle_id)
            .filter(|slot| slot.handle == *handle)
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

fn selector_harness() -> (
    ManagedLocalSelectorAdapter,
    Arc<HermeticSelectorPort>,
    ActivationPlanV1,
) {
    let current = verified_slot("handle.current", 0x60);
    let candidate = verified_slot("handle.candidate", 0x70);
    let previous = verified_slot("handle.previous", 0x80);
    let slots = Arc::new(StaticSlots {
        slots: [current.clone(), candidate.clone(), previous.clone()]
            .into_iter()
            .map(|slot| (slot.handle.handle_id.clone(), slot))
            .collect(),
    });
    let mut initial = ActiveSelectorObservationV1 {
        selector_identity_hash: hash(0x24),
        selected_build_content_hash: current.build_content_hash.clone(),
        selected_root_identity_hash: current.root_identity_hash.clone(),
        capability_generation: 3,
        observation_hash: String::new(),
    };
    initial.observation_hash = canonical_hash(&initial).expect("observation hash");
    let native = Arc::new(HermeticSelectorPort::new(initial));
    let adapter = ManagedLocalSelectorAdapter::new(
        Arc::clone(&native) as Arc<dyn SelectorMutationPortV1>,
        slots as Arc<dyn BuildSlotVerifyPortV1>,
    );
    let plan = ActivationPlanV1 {
        activation_id: id("activation.1"),
        capability_generation: 3,
        capability_digest: hash(0x90),
        selector_identity_hash: hash(0x24),
        current,
        candidate,
        previous,
        current_process_generation: ProcessGeneration(10),
        candidate_process_generation: ProcessGeneration(11),
        rollback_process_generation: ProcessGeneration(12),
        deadlines: BootstrapDeadlinesV1 {
            admission_ms: 1_000,
            cleanup_ms: 1_000,
            startup_ms: 1_000,
            focused_verification_ms: 1_000,
            rollback_ms: 1_000,
            result_read_ms: 1_000,
        },
    };
    (adapter, native, plan)
}

#[test]
fn selector_switch_and_restore_are_exact_and_idempotent() {
    let (adapter, _, plan) = selector_harness();
    let switched = adapter
        .apply_candidate_selector(&plan, BootstrapPhaseV1::QuiescingCurrent)
        .expect("switch");
    assert_eq!(
        adapter
            .apply_candidate_selector(&plan, BootstrapPhaseV1::QuiescingCurrent)
            .expect("idempotent switch retry"),
        switched
    );
    assert_eq!(
        switched.after.selected_build_content_hash,
        plan.candidate.build_content_hash
    );
    assert_eq!(
        adapter
            .verify_selector(&plan, &plan.candidate.handle)
            .expect("verify candidate")
            .selected_build_content_hash,
        plan.candidate.build_content_hash
    );
    let restored = adapter
        .restore_previous_selector(&plan, BootstrapPhaseV1::RollingBack)
        .expect("restore");
    assert_eq!(
        restored.after.selected_build_content_hash,
        plan.previous.build_content_hash
    );
}

#[test]
fn selector_never_retries_ambiguous_or_illegal_mutations() {
    let (adapter, native, plan) = selector_harness();
    native.fail_next_ambiguous();
    assert!(matches!(
        adapter.apply_candidate_selector(&plan, BootstrapPhaseV1::QuiescingCurrent),
        Err(ProfileError::AmbiguousSelector)
    ));
    assert!(matches!(
        adapter.apply_candidate_selector(&plan, BootstrapPhaseV1::CandidateSelected),
        Err(ProfileError::Invalid(_))
    ));
    assert_eq!(
        native
            .observe()
            .expect("observe")
            .selected_build_content_hash,
        plan.current.build_content_hash
    );
}

#[test]
fn selector_capability_drift_fails_before_mutation() {
    let (adapter, native, plan) = selector_harness();
    let mut drifted = native.observe().expect("observe");
    drifted.capability_generation = 4;
    drifted.observation_hash.clear();
    drifted.observation_hash = canonical_hash(&drifted).expect("hash");
    native.replace_observation(drifted);
    assert!(matches!(
        adapter.apply_candidate_selector(&plan, BootstrapPhaseV1::QuiescingCurrent),
        Err(ProfileError::SelectorDrift)
    ));
}
